//! Small, non-secret preferences that outlive a session.
//!
//! Deliberately separate from wallet metadata: these describe how a person
//! likes to look at Sieve, not anything about a particular wallet.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Denomination {
    /// Whole bitcoin, eight decimal places.
    Btc,
    /// Satoshis. The default, because a light wallet's amounts are usually
    /// small enough that BTC is mostly leading zeros.
    #[default]
    Sats,
}

impl Denomination {
    pub fn toggled(self) -> Self {
        match self {
            Denomination::Btc => Denomination::Sats,
            Denomination::Sats => Denomination::Btc,
        }
    }

    /// The unit as written, for the network it belongs to.
    ///
    /// Test coins get their own ticker — sBTC on signet, tBTC on testnet —
    /// because "0.005 BTC" reads as real money whatever the header says, and
    /// these are worth nothing. The satoshi has no such convention and
    /// inventing one would be worse than leaning on the network shown beside
    /// it.
    pub fn label(self, network: &str) -> &'static str {
        match self {
            Denomination::Sats => "sats",
            Denomination::Btc => match network {
                "bitcoin" => "BTC",
                "signet" => "sBTC",
                "testnet" | "testnet4" => "tBTC",
                "regtest" => "rBTC",
                _ => "BTC",
            },
        }
    }

    /// Render an amount.
    ///
    /// Formatted by integer arithmetic rather than floating point: a balance
    /// must never be off by a satoshi because of a rounding artefact.
    pub fn format(self, sats: u64, network: &str) -> String {
        let unit = self.label(network);
        match self {
            Denomination::Sats => format!("{} {unit}", group(sats)),
            Denomination::Btc => {
                let whole = sats / 100_000_000;
                let fraction = sats % 100_000_000;
                format!("{}.{fraction:08} {unit}", group(whole))
            }
        }
    }
}

/// Digit grouping, so six-figure amounts stay readable.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Light, dark, or whatever the desktop says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Appearance {
    /// Follow the desktop. The default, and the right answer for most people:
    /// the desktop already knows, and an app that ignores it looks foreign.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "Follow the system",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub denomination: Denomination,
    /// Whether to show a fiat value beside the balance.
    ///
    /// Off by default: fetching a price is the only connection Sieve makes
    /// that is not Bitcoin peer-to-peer, and a wallet built to avoid
    /// disclosures should not make one nobody asked for.
    #[serde(default)]
    pub show_fiat: bool,
    #[serde(default)]
    pub appearance: Appearance,
    /// The wallet opened last, so a restart returns to it.
    ///
    /// Without this, startup opens whichever wallet sorts first by name, which
    /// is not a choice anybody made.
    #[serde(default)]
    pub last_wallet: Option<String>,
}

fn path() -> PathBuf {
    crate::wallet::data_root().join("settings.json")
}

impl Settings {
    /// Missing or unreadable settings fall back to defaults rather than
    /// failing — a preferences file is never worth blocking startup over.
    pub fn load() -> Self {
        std::fs::read(path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Ok(bytes) = serde_json::to_vec_pretty(self) else { return };
        if let Err(e) = crate::vault::write_atomic(&path(), &bytes) {
            tracing::warn!(%e, "could not save settings");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coins_carry_their_own_ticker() {
        // "0.005 BTC" on signet reads as real money. It is worth nothing.
        assert_eq!(Denomination::Btc.label("bitcoin"), "BTC");
        assert_eq!(Denomination::Btc.label("signet"), "sBTC");
        assert_eq!(Denomination::Btc.label("testnet"), "tBTC");
        assert_eq!(Denomination::Btc.label("testnet4"), "tBTC");
        assert_eq!(Denomination::Btc.label("regtest"), "rBTC");

        // Satoshis have no such convention, and inventing one is worse.
        assert_eq!(Denomination::Sats.label("signet"), "sats");

        assert_eq!(Denomination::Btc.format(100_000_000, "signet"), "1.00000000 sBTC");
    }

    #[test]
    fn amounts_are_exact() {
        // Integer arithmetic, so no rounding artefact can move a satoshi.
        assert_eq!(Denomination::Sats.format(53_713, "bitcoin"), "53,713 sats");
        assert_eq!(Denomination::Btc.format(53_713, "bitcoin"), "0.00053713 BTC");
        assert_eq!(Denomination::Btc.format(100_000_000, "bitcoin"), "1.00000000 BTC");
        assert_eq!(Denomination::Btc.format(2_100_000_000_000_000, "bitcoin"), "21,000,000.00000000 BTC");
        assert_eq!(Denomination::Btc.format(1, "bitcoin"), "0.00000001 BTC");
        assert_eq!(Denomination::Sats.format(0, "bitcoin"), "0 sats");
    }

    #[test]
    fn appearance_defaults_to_the_desktop() {
        // An app that picks its own look before being asked is an app that
        // looks foreign on somebody's desktop.
        assert_eq!(Appearance::default(), Appearance::System);
        assert_eq!(Settings::default().appearance, Appearance::System);
    }

    #[test]
    fn toggling_twice_returns() {
        assert_eq!(Denomination::Sats.toggled().toggled(), Denomination::Sats);
        assert_eq!(Denomination::Btc.toggled(), Denomination::Sats);
    }
}
