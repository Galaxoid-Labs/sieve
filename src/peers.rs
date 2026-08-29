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
pub fn remembered(network: Network) -> Vec<IpAddr> {
    let Ok(bytes) = std::fs::read(path(network)) else {
        return Vec::new();
    };
    let stored: Vec<String> = serde_json::from_slice(&bytes).unwrap_or_default();
    stored.iter().filter_map(|s| s.parse().ok()).collect()
}

/// Record the peers currently connected on this network.
///
/// Called after a sync lands, so these are addresses that were part of one.
pub fn remember(network: Network, addresses: &[String]) {
    let keep: Vec<&String> = addresses
        .iter()
        .filter(|a| a.parse::<IpAddr>().is_ok())
        .take(KEEP)
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

    #[test]
    fn only_parseable_addresses_are_kept() {
        // A malformed entry must not poison the list for everything after it.
        let stored = ["1.2.3.4".to_string(), "nonsense".to_string(), "::1".to_string()];
        let kept: Vec<IpAddr> = stored.iter().filter_map(|s| s.parse().ok()).collect();
        assert_eq!(kept.len(), 2);
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
