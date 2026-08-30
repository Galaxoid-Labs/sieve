//! The block headers a network has already given us.
//!
//! kyoto accepts a `data_dir` and ignores it, so its header chain lives in
//! memory and is fetched again on every start — and again for every wallet,
//! since each one runs its own node. This is that persistence, done here.
//!
//! Headers are public chain data, identical for every wallet on a network, so
//! the file is per network rather than per wallet. Eighty bytes each: the
//! whole of mainnet is about seventy-seven megabytes, and the range a wallet
//! actually needs is usually far less.
//!
//! **What this file is trusted for.** It becomes the node's idea of the chain,
//! so it is anchored: the first header must hash to a checkpoint compiled into
//! this binary, and every header after it must name its predecessor. A file
//! that fails either test is not repaired or partly used — it is ignored, and
//! the chain is fetched from the network as before. kyoto validates again on
//! its own account, but a wallet should not hand its node a chain it has not
//! checked itself.

use std::io::Write;
use std::path::PathBuf;

use bdk_wallet::bitcoin::Network;
use bdk_wallet::bitcoin::block::Header;
use bdk_wallet::bitcoin::consensus::{deserialize, serialize};
use bdk_kyoto::bip157::chain::IndexedHeader;

/// Recognises the file, and refuses one written by a different layout.
const MAGIC: &[u8; 8] = b"sievehdr";
const VERSION: u8 = 1;
/// A serialized block header, always.
const HEADER_LEN: usize = 80;

fn path(network: Network) -> PathBuf {
    super::chain_dir(network).join("headers.dat")
}

/// Everything stored for this network, or nothing if it cannot be trusted.
///
/// `Some` only when the file parses, starts at a height whose hash is a
/// checkpoint in this binary, and links header to header all the way through.
pub fn load(network: Network) -> Option<Vec<IndexedHeader>> {
    let bytes = std::fs::read(path(network)).ok()?;
    let headers = parse(&bytes)?;

    // Anchored to something this binary knows, so a swapped file cannot become
    // the chain. Without this the only defence is proof of work, which an
    // attacker with a file on your disk does not have to beat — they can hand
    // you a valid chain from somewhere else entirely.
    let first = headers.first()?;
    // Anchored either way round: the first header may *be* a checkpoint, or it
    // may be the block straight after one — which is the ordinary case, since
    // a node's chain begins just past its anchor and has no header for the
    // anchor itself.
    let anchored = super::checkpoints(network).iter().any(|c| {
        (c.height == first.height && first.header.block_hash().to_string() == c.hash)
            || (c.height + 1 == first.height
                && first.header.prev_blockhash.to_string() == c.hash)
    });
    if !anchored {
        tracing::warn!(
            %network,
            height = first.height,
            "stored headers do not start at a known checkpoint; ignoring them"
        );
        return None;
    }

    tracing::info!(
        %network,
        from = first.height,
        count = headers.len(),
        "reusing stored block headers"
    );
    Some(headers)
}

/// Write what a node has learned, for the next wallet and the next start.
///
/// **The stored range only ever widens.** A wallet whose birthday is recent
/// knows the chain from there; one imported from an old device knows it from
/// far earlier. If the recent wallet simply wrote what it had, it would throw
/// away the older wallet's range, and that wallet would refetch its whole
/// history on the next start — the file is shared, so the narrowest wallet
/// would keep spoiling it for the widest.
pub fn save(network: Network, headers: &[IndexedHeader]) -> std::io::Result<()> {
    let Some(first) = headers.first() else { return Ok(()) };

    // A union of what is on disk and what has just been walked, so the file
    // only ever grows.
    //
    // The rule this replaces kept the stored chain only when it started
    // *earlier*. Two chains that both start at height one are not "earlier",
    // so a freshly started node with two thousand headers overwrote a stored
    // nine hundred thousand — the file went backwards, and the next start
    // fetched the lot again.
    let combined: Vec<IndexedHeader> = match load(network) {
        Some(stored) => {
            let mut by_height: std::collections::BTreeMap<u32, IndexedHeader> =
                stored.into_iter().map(|h| (h.height, h)).collect();
            for header in headers {
                by_height.insert(header.height, header.clone());
            }

            // The longest run with no gap in it, starting from the lowest
            // height: a chain with a hole is not a chain, and the loader would
            // refuse it anyway.
            let mut contiguous: Vec<IndexedHeader> = Vec::with_capacity(by_height.len());
            for (height, header) in by_height {
                match contiguous.last() {
                    Some(previous) if previous.height + 1 != height => break,
                    _ => contiguous.push(header),
                }
            }
            contiguous
        }
        None => headers.to_vec(),
    };

    let headers = &combined[..];
    let Some(first) = headers.first() else { return Ok(()) };

    let dir = super::chain_dir(network);
    std::fs::create_dir_all(&dir)?;

    let mut bytes = Vec::with_capacity(16 + headers.len() * HEADER_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&first.height.to_le_bytes());
    bytes.extend_from_slice(&(headers.len() as u32).to_le_bytes());
    for indexed in headers {
        bytes.extend_from_slice(&serialize(&indexed.header));
    }

    // Written whole or not at all: a half-written chain is exactly the kind of
    // file the loader would have to reject anyway.
    let temporary = dir.join("headers.dat.tmp");
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path(network))?;

    tracing::info!(%network, from = first.height, count = headers.len(), "stored block headers");
    Ok(())
}

/// Read the file into headers, checking the shape and the chain of them.
///
/// Its own function, taking bytes, so every way it can go wrong is testable
/// without a filesystem.
fn parse(bytes: &[u8]) -> Option<Vec<IndexedHeader>> {
    if bytes.len() < 17 || &bytes[..8] != MAGIC || bytes[8] != VERSION {
        return None;
    }
    let start = u32::from_le_bytes(bytes[9..13].try_into().ok()?);
    let count = u32::from_le_bytes(bytes[13..17].try_into().ok()?) as usize;

    let body = &bytes[17..];
    if body.len() != count * HEADER_LEN {
        return None;
    }

    let mut headers = Vec::with_capacity(count);
    let mut previous: Option<Header> = None;
    for (index, chunk) in body.chunks_exact(HEADER_LEN).enumerate() {
        let header: Header = deserialize(chunk).ok()?;
        // Contiguous and connected. A gap or a swapped block would otherwise
        // become a chain the node believes in.
        if let Some(previous) = previous
            && header.prev_blockhash != previous.block_hash()
        {
            return None;
        }
        previous = Some(header);
        headers.push(IndexedHeader { height: start + index as u32, header });
    }
    Some(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mainnet 400,000 and the two blocks after it, taken from the chain.
    /// Real headers, because the point of the checks is that they hold against
    /// real data and fail against tampered data.
    fn sample() -> Vec<IndexedHeader> {
        let hexes = [
            "0200000068f0b1c9d4f0d1e6f9a1b8b0e0f1d1c1b1a191817161514131211101f0e0d0c0b0a09080706050403020100ffff001d00000000",
        ];
        // Built rather than fetched: a header only has to be well-formed and
        // linked for these tests, and a real one would tie the test to a
        // network round trip.
        let mut headers = Vec::new();
        let mut previous = None;
        for (index, _) in hexes.iter().chain(hexes.iter()).chain(hexes.iter()).enumerate() {
            let header = Header {
                version: bdk_wallet::bitcoin::block::Version::TWO,
                prev_blockhash: previous.unwrap_or_else(|| {
                    "000000000000000004ec466ce4732fe6f1ed1cddc2ed4b328fff5224276e3f6f"
                        .parse()
                        .unwrap()
                }),
                merkle_root: "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b"
                    .parse()
                    .unwrap(),
                time: 1_450_000_000 + index as u32,
                bits: bdk_wallet::bitcoin::CompactTarget::from_consensus(0x1d00ffff),
                nonce: index as u32,
            };
            previous = Some(header.block_hash());
            headers.push(IndexedHeader { height: 400_000 + index as u32, header });
        }
        headers
    }

    /// A run of linked headers starting at `from`.
    fn sample_run(from: u32, count: u32) -> Vec<IndexedHeader> {
        let mut headers = Vec::new();
        let mut previous: Option<bdk_wallet::bitcoin::BlockHash> = None;
        for index in 0..count {
            let header = Header {
                version: bdk_wallet::bitcoin::block::Version::TWO,
                prev_blockhash: previous.unwrap_or_else(|| {
                    "000000000000000004ec466ce4732fe6f1ed1cddc2ed4b328fff5224276e3f6f"
                        .parse()
                        .unwrap()
                }),
                merkle_root: "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b"
                    .parse()
                    .unwrap(),
                time: 1_450_000_000 + index,
                bits: bdk_wallet::bitcoin::CompactTarget::from_consensus(0x1d00ffff),
                nonce: index,
            };
            previous = Some(header.block_hash());
            headers.push(IndexedHeader { height: from + index, header });
        }
        headers
    }

    /// The merge `save` performs, without touching a filesystem.
    fn union(stored: &[IndexedHeader], fresh: &[IndexedHeader]) -> Vec<IndexedHeader> {
        let mut by_height: std::collections::BTreeMap<u32, IndexedHeader> =
            stored.iter().map(|h| (h.height, h.clone())).collect();
        for header in fresh {
            by_height.insert(header.height, header.clone());
        }
        let mut contiguous: Vec<IndexedHeader> = Vec::new();
        for (height, header) in by_height {
            match contiguous.last() {
                Some(previous) if previous.height + 1 != height => break,
                _ => contiguous.push(header),
            }
        }
        contiguous
    }

    #[test]
    fn what_is_written_is_read_back() {
        let headers = sample();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&headers[0].height.to_le_bytes());
        bytes.extend_from_slice(&(headers.len() as u32).to_le_bytes());
        for h in &headers {
            bytes.extend_from_slice(&serialize(&h.header));
        }

        let read = parse(&bytes).expect("a file we just wrote must read back");
        assert_eq!(read.len(), headers.len());
        assert_eq!(read[0].height, 400_000);
        assert_eq!(read[2].height, 400_002);
        assert_eq!(read[1].header.block_hash(), headers[1].header.block_hash());
    }

    #[test]
    fn a_broken_chain_is_refused() {
        let headers = sample();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&headers[0].height.to_le_bytes());
        bytes.extend_from_slice(&(headers.len() as u32).to_le_bytes());
        // The middle header replaced by one that does not follow the first.
        for (index, h) in headers.iter().enumerate() {
            let mut header = h.header;
            if index == 1 {
                header.prev_blockhash = "00000000000000000000000000000000000000000000000000000000deadbeef"
                    .parse()
                    .unwrap();
            }
            bytes.extend_from_slice(&serialize(&header));
        }

        assert!(parse(&bytes).is_none(), "a header that names the wrong parent was accepted");
    }

    #[test]
    fn a_file_that_is_not_ours_is_refused() {
        assert!(parse(b"").is_none());
        assert!(parse(b"not a header file at all").is_none());

        // Right magic, wrong version.
        let mut wrong = MAGIC.to_vec();
        wrong.push(VERSION + 1);
        wrong.extend_from_slice(&[0u8; 8]);
        assert!(parse(&wrong).is_none());

        // A count that does not match the body: truncated, or lying.
        let mut short = MAGIC.to_vec();
        short.push(VERSION);
        short.extend_from_slice(&400_000u32.to_le_bytes());
        short.extend_from_slice(&5u32.to_le_bytes());
        short.extend_from_slice(&[0u8; HEADER_LEN]);
        assert!(parse(&short).is_none());
    }

    /// The file must never go backwards.
    ///
    /// A node that has just started holds a couple of thousand headers; the
    /// file may hold a million. Saving the short one over the long one — which
    /// is what happened — throws away everything and fetches it all again on
    /// the next start.
    #[test]
    fn a_shorter_chain_never_replaces_a_longer_one() {
        let long = sample_run(1, 50);
        let short = sample_run(1, 5);

        // Both start at the same height, so "starts earlier" cannot decide it;
        // the union has to.
        let merged = union(&long, &short);
        assert_eq!(merged.len(), 50, "the longer chain was lost");

        // And a later chunk extends rather than replaces.
        let tail = sample_run(51, 10);
        let extended = union(&merged, &tail);
        assert_eq!(extended.len(), 60);
        assert_eq!(extended.first().unwrap().height, 1);
        assert_eq!(extended.last().unwrap().height, 60);
    }

    /// A gap means the run stops there rather than storing a chain with a hole
    /// in it, which the loader would refuse anyway.
    #[test]
    fn a_gap_ends_the_stored_chain() {
        let first = sample_run(1, 10);
        let detached = sample_run(100, 10);
        assert_eq!(union(&first, &detached).len(), 10);
    }

    /// Networks keep their own file: mainnet headers fed to a signet node
    /// would be a chain from another universe.
    #[test]
    fn networks_do_not_share_a_file() {
        assert_ne!(path(Network::Bitcoin), path(Network::Signet));
        assert!(path(Network::Bitcoin).to_string_lossy().contains("bitcoin"));
    }
}
