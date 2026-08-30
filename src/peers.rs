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

/// Addresses last known to be part of a working sync on this network.
///
/// Returned as written rather than parsed: an onion address is not an
/// `IpAddr`, and the peers worth remembering from a run over Tor are precisely
/// the onion ones.
pub fn remembered(network: Network) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path(network)) else {
        return Vec::new();
    };
    let stored: Vec<String> = serde_json::from_slice(&bytes).unwrap_or_default();
    stored.into_iter().filter(|a| is_address(a)).collect()
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

/// Record the peers currently connected on this network.
///
/// Called after a sync lands, so these are addresses that were part of one.
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

    let Ok(bytes) = serde_json::to_vec_pretty(&keep) else { return };
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

    #[test]
    fn networks_do_not_share_a_file() {
        // Pointing a signet wallet at mainnet peers would waste every
        // connection attempt on nodes speaking a different chain.
        assert_ne!(path(Network::Bitcoin), path(Network::Signet));
        assert!(path(Network::Bitcoin).to_string_lossy().contains("bitcoin"));
        assert!(path(Network::Signet).to_string_lossy().contains("signet"));
    }
}
