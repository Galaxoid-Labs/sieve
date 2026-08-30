//! Peers worth trying again, remembered per network.
//!
//! Kyoto drops the data directory it is given, so every start re-discovers the
//! network from DNS — about a minute before anything syncs. Remembering the
//! peers that were connected when a sync last succeeded turns that into a
//! handful of direct connections.
//!
//! Deliberately not a shipped list of known nodes. Hardcoding addresses would
//! point every Sieve user at the same few machines, which is a disclosure those
//! operators did not ask for and a single point of failure besides. What worked
//! for this machine is nobody else's business and stays on it.

use std::net::IpAddr;
use std::path::PathBuf;

use bdk_wallet::bitcoin::Network;

/// How many to keep. Enough to seed a connection round without pinning the
/// wallet to a fixed set of watchers.
const KEEP: usize = 12;

fn path(network: Network) -> PathBuf {
    crate::wallet::data_root()
        .join("peers")
        .join(format!("{network}.json"))
}

/// What is written to the file, so its provenance travels with it.
///
/// The first format was a bare array of addresses, written by a version that
/// remembered whatever happened to be connected — including peers that cannot
/// serve compact filters, which are worse than useless to this wallet and
/// crowd out the ones that can. Those files are not deleted; they are simply
/// no longer believed. A list nobody can vouch for is not a head start.
#[derive(serde::Serialize, serde::Deserialize)]
struct Remembered {
    version: u32,
    /// Every address here advertised `NODE_COMPACT_FILTERS` while connected.
    serves_filters: bool,
    peers: Vec<String>,
}

/// The only format worth reading: addresses confirmed to serve filters.
const FORMAT: u32 = 2;

/// Addresses last known to serve compact filters on this network.
///
/// Returned as written rather than parsed: an onion address is not an
/// `IpAddr`, and the peers worth remembering from a run over Tor are precisely
/// the onion ones.
pub fn remembered(network: Network) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path(network)) else {
        return Vec::new();
    };
    let Ok(stored) = serde_json::from_slice::<Remembered>(&bytes) else {
        // The old bare-array format, or something unreadable. Either way its
        // contents cannot be vouched for.
        tracing::debug!(%network, "ignoring a peer list from an older format");
        return Vec::new();
    };
    if stored.version != FORMAT || !stored.serves_filters {
        return Vec::new();
    }
    stored.peers.into_iter().filter(|a| is_address(a)).collect()
}

/// Something we could dial again: an IP, or an onion address whose checksum
/// holds. Anything else is a corrupted file or a hand-edit, and connecting to
/// it would only waste attempts.
fn is_address(text: &str) -> bool {
    if crate::tor::onion::looks_like_onion(text) {
        return crate::tor::onion::decode(text).is_some();
    }
    text.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().is_ok()
}

/// Record peers that serve compact filters on this network.
///
/// Called after a sync lands, with addresses that advertised
/// `NODE_COMPACT_FILTERS` while connected. Nothing else belongs here: a peer
/// that cannot serve a filter takes a connection slot from one that can, and
/// this wallet's entire sync is filters.
pub fn remember(network: Network, addresses: &[String]) {
    // Deduplicated: kyoto reports one entry per connection and several can
    // carry the same address, so a naive copy fills the list with one peer.
    // Seeding eight connections to a single node would concentrate every
    // request on it — worse for speed and worse for privacy than not seeding.
    let mut seen = std::collections::HashSet::new();
    let keep: Vec<String> = addresses
        .iter()
        .filter(|a| is_address(a))
        .filter(|a| seen.insert((*a).clone()))
        .take(KEEP)
        .cloned()
        .collect();
    if keep.is_empty() {
        return;
    }

    let record = Remembered { version: FORMAT, serves_filters: true, peers: keep };
    let Ok(bytes) = serde_json::to_vec_pretty(&record) else { return };
    if let Err(e) = crate::vault::write_atomic(&path(network), &bytes) {
        // Losing this costs a slower start, nothing more.
        tracing::debug!(%e, "could not remember peers");
    }
}

pub fn count(network: Network) -> usize {
    remembered(network).len()
}

/// Forget them, so the next start discovers the network afresh.
pub fn clear(network: Network) {
    match std::fs::remove_file(path(network)) {
        Ok(()) => tracing::info!(%network, "forgot remembered peers"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(%e, "could not forget remembered peers"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_ONION: &str =
        "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    #[test]
    fn only_addresses_that_could_be_dialled_are_kept() {
        // Tests the filter itself, not a copy of it: a malformed entry must
        // not poison the list, and an onion address must survive it — those
        // are the peers a run over Tor has to remember.
        assert!(is_address("1.2.3.4"));
        assert!(is_address("::1"));
        assert!(is_address("[::1]"));
        assert!(is_address(REAL_ONION));

        assert!(!is_address("nonsense"));
        assert!(!is_address(""));
        // An onion address with one character changed is unreachable, and
        // storing it would spend connection attempts on nothing.
        assert!(!is_address(&REAL_ONION.replacen("2gzyxa5", "2gzyxa6", 1)));
    }

    #[test]
    fn duplicates_are_not_remembered_repeatedly() {
        // Several connections to one peer report the same address, and a list
        // of one peer eight times is worse than an empty one.
        let addresses = vec![
            "1.2.3.4".to_string(),
            "1.2.3.4".to_string(),
            REAL_ONION.to_string(),
            "nonsense".to_string(),
            REAL_ONION.to_string(),
        ];
        let mut seen = std::collections::HashSet::new();
        let kept: Vec<&String> = addresses
            .iter()
            .filter(|a| is_address(a))
            .filter(|a| seen.insert((*a).clone()))
            .collect();
        assert_eq!(kept.len(), 2, "{kept:?}");
    }

    /// The list is only as good as its provenance. A file written by the
    /// version that remembered any connected peer cannot be told apart from
    /// one written carefully, so it is not read at all.
    #[test]
    fn a_list_from_the_old_format_is_not_believed() {
        let bare = serde_json::to_vec(&["1.2.3.4", REAL_ONION]).unwrap();
        assert!(
            serde_json::from_slice::<Remembered>(&bare).is_err(),
            "the old bare array must not parse as a vouched list"
        );

        // And a well-formed file that does not claim filter-serving peers is
        // ignored just the same.
        let unvouched = serde_json::to_vec(&Remembered {
            version: FORMAT,
            serves_filters: false,
            peers: vec!["1.2.3.4".into()],
        })
        .unwrap();
        let parsed: Remembered = serde_json::from_slice(&unvouched).unwrap();
        assert!(!parsed.serves_filters);
    }

    #[test]
    fn what_is_written_can_be_read_back() {
        let record = Remembered {
            version: FORMAT,
            serves_filters: true,
            peers: vec!["1.2.3.4".into(), REAL_ONION.into()],
        };
        let bytes = serde_json::to_vec(&record).unwrap();
        let read: Remembered = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(read.version, FORMAT);
        assert!(read.serves_filters);
        assert_eq!(read.peers.len(), 2);
    }

    #[test]
    fn networks_do_not_share_a_file() {
        // Pointing a signet wallet at mainnet peers would waste every
        // connection attempt on nodes speaking a different chain.
        assert_ne!(path(Network::Bitcoin), path(Network::Signet));
        assert!(path(Network::Bitcoin).to_string_lossy().contains("bitcoin"));
        assert!(path(Network::Signet).to_string_lossy().contains("signet"));
    }
}
