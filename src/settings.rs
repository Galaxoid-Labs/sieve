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

    pub fn label(self) -> &'static str {
        match self {
            Denomination::Btc => "BTC",
            Denomination::Sats => "sats",
        }
    }

    /// Render an amount.
    ///
    /// Formatted by integer arithmetic rather than floating point: a balance
    /// must never be off by a satoshi because of a rounding artefact.
    pub fn format(self, sats: u64) -> String {
        match self {
            Denomination::Sats => format!("{} sats", group(sats)),
            Denomination::Btc => {
                let whole = sats / 100_000_000;
                let fraction = sats % 100_000_000;
                format!("{}.{fraction:08} BTC", group(whole))
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

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
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
    fn amounts_are_exact() {
        // Integer arithmetic, so no rounding artefact can move a satoshi.
        assert_eq!(Denomination::Sats.format(53_713), "53,713 sats");
        assert_eq!(Denomination::Btc.format(53_713), "0.00053713 BTC");
        assert_eq!(Denomination::Btc.format(100_000_000), "1.00000000 BTC");
        assert_eq!(Denomination::Btc.format(2_100_000_000_000_000), "21,000,000.00000000 BTC");
        assert_eq!(Denomination::Btc.format(1), "0.00000001 BTC");
        assert_eq!(Denomination::Sats.format(0), "0 sats");
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
