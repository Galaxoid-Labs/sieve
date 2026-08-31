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
    /// Read an amount typed by a person, in whichever unit is on display.
    ///
    /// Integer arithmetic throughout. A float would be the obvious way to read
    /// "0.1" and the wrong one: 0.1 is not representable in binary, and a
    /// satoshi lost to rounding here is money.
    ///
    /// The error is the message shown, so it says what to do rather than what
    /// went wrong internally.
    pub fn parse(self, text: &str) -> Result<u64, String> {
        // Typed amounts get read back from a display that groups digits, so
        // separators are accepted rather than treated as an error.
        let cleaned: String = text
            .chars()
            .filter(|c| !matches!(c, ',' | ' ' | '_' | '\''))
            .collect();
        let cleaned = cleaned.trim();

        if cleaned.is_empty() {
            return Err("Enter an amount".into());
        }
        if cleaned.starts_with('-') {
            return Err("An amount cannot be negative".into());
        }

        let sats = match self {
            Denomination::Sats => {
                if cleaned.contains('.') {
                    return Err("Satoshis are whole numbers".into());
                }
                cleaned
                    .parse::<u64>()
                    .map_err(|_| "That is not a number".to_string())?
            }
            Denomination::Btc => {
                let (whole, fraction) = match cleaned.split_once('.') {
                    Some((w, f)) => (w, f),
                    None => (cleaned, ""),
                };
                if fraction.len() > 8 {
                    return Err("A bitcoin divides into 100,000,000 satoshis, no further".into());
                }
                if !fraction.chars().all(|c| c.is_ascii_digit())
                    || !(whole.is_empty() || whole.chars().all(|c| c.is_ascii_digit()))
                {
                    return Err("That is not a number".into());
                }
                let whole: u64 = if whole.is_empty() {
                    0
                } else {
                    whole
                        .parse()
                        .map_err(|_| "That is not a number".to_string())?
                };
                // Pad rather than scale: "0.1" is 10,000,000 satoshis, not 1.
                let padded = format!("{fraction:0<8}");
                let fraction: u64 = padded
                    .parse()
                    .map_err(|_| "That is not a number".to_string())?;
                whole
                    .checked_mul(100_000_000)
                    .and_then(|w| w.checked_add(fraction))
                    .ok_or("That is more bitcoin than exists")?
            }
        };

        if sats > 2_100_000_000_000_000 {
            return Err("That is more bitcoin than exists".into());
        }
        Ok(sats)
    }
}

/// Digit grouping, so six-figure amounts stay readable.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// How long the wallet stays open with nobody touching it.
///
/// Not about the seed — that is decrypted only at the moment of signing, and
/// an idle wallet holds no key. It is about what is on the screen: the balance,
/// the history, the addresses, and a payment somebody could drive as far as the
/// password prompt while you are away from the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum IdleLock {
    Never,
    #[default]
    After5Minutes,
    After15Minutes,
    After30Minutes,
    AfterHour,
}

impl IdleLock {
    pub const ALL: [IdleLock; 5] = [
        IdleLock::Never,
        IdleLock::After5Minutes,
        IdleLock::After15Minutes,
        IdleLock::After30Minutes,
        IdleLock::AfterHour,
    ];

    pub fn label(self) -> &'static str {
        match self {
            IdleLock::Never => "Never",
            IdleLock::After5Minutes => "After 5 minutes",
            IdleLock::After15Minutes => "After 15 minutes",
            IdleLock::After30Minutes => "After 30 minutes",
            IdleLock::AfterHour => "After an hour",
        }
    }

    /// `None` means never.
    pub fn duration(self) -> Option<std::time::Duration> {
        let minutes = match self {
            IdleLock::Never => return None,
            IdleLock::After5Minutes => 5,
            IdleLock::After15Minutes => 15,
            IdleLock::After30Minutes => 30,
            IdleLock::AfterHour => 60,
        };
        Some(std::time::Duration::from_secs(minutes * 60))
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
    /// Ask mempool.space what fees are going for, instead of reading the last
    /// block ourselves.
    ///
    /// Off by default. The local estimate costs a block download and tells
    /// nobody anything; this one is a better number bought with a disclosure
    /// that is worse than the price lookup — a request for fee rates says a
    /// payment is about to be sent, and roughly when.
    #[serde(default)]
    pub mempool_fees: bool,
    /// Route every connection — peers, price, fee rates — through a Tor
    /// SOCKS5 proxy.
    ///
    /// Off by default because it needs a Tor daemon this app does not ship.
    /// When it is on and the proxy cannot be reached, Sieve refuses to
    /// connect rather than quietly going out over the clear.
    #[serde(default)]
    pub tor: bool,
    /// Where that proxy is, when it is not in the usual place.
    #[serde(default)]
    pub tor_proxy: Option<String>,
    /// How long an untouched wallet stays open.
    ///
    /// Five minutes by default. A wallet is not a document: leaving one open
    /// on an unattended screen shows a stranger the balance and every payment
    /// ever made, and the cost of being wrong is a password typed again.
    #[serde(default)]
    pub idle_lock: IdleLock,
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
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        if let Err(e) = crate::vault::write_atomic(&path(), &bytes) {
            tracing::warn!(%e, "could not save settings");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_lock_choices_are_ordered_and_finite() {
        use IdleLock::*;
        // Never is first because it is the exception, and the rest climb.
        assert_eq!(IdleLock::ALL[0], Never);
        assert_eq!(Never.duration(), None);

        let minutes: Vec<u64> = IdleLock::ALL
            .iter()
            .filter_map(|choice| choice.duration())
            .map(|d| d.as_secs() / 60)
            .collect();
        assert_eq!(minutes, vec![5, 15, 30, 60]);
        assert!(
            minutes.windows(2).all(|pair| pair[0] < pair[1]),
            "must climb"
        );

        // The default locks. A wallet that ships with this off protects
        // nobody who never opens preferences.
        assert!(IdleLock::default().duration().is_some());
    }

    #[test]
    fn btc_amounts_are_read_without_floating_point() {
        for (text, sats) in [
            ("0.1", 10_000_000u64),
            ("1", 100_000_000),
            (".00000001", 1),
            ("0.00000001", 1),
            ("1.23456789", 123_456_789),
            ("21,000,000", 2_100_000_000_000_000),
        ] {
            assert_eq!(Denomination::Btc.parse(text), Ok(sats), "{text}");
        }
    }

    #[test]
    fn what_is_shown_can_be_read_back() {
        for sats in [1u64, 999, 100_000_000, 123_456_789, 2_100_000_000_000_000] {
            for unit in [Denomination::Btc, Denomination::Sats] {
                let shown = unit.format(sats, "bitcoin");
                let (amount, _) = shown.rsplit_once(' ').unwrap();
                assert_eq!(unit.parse(amount), Ok(sats), "{shown}");
            }
        }
    }

    #[test]
    fn nonsense_amounts_are_refused() {
        assert!(Denomination::Sats.parse("0.5").is_err());
        assert!(Denomination::Btc.parse("0.000000001").is_err());
        assert!(Denomination::Btc.parse("-1").is_err());
        assert!(Denomination::Btc.parse("").is_err());
        assert!(Denomination::Btc.parse("lots").is_err());
        assert!(Denomination::Btc.parse("21000001").is_err());
        assert!(Denomination::Sats.parse("2100000000000001").is_err());

        // Big enough that whole * 100_000_000 leaves u64 before the supply
        // cap below it is ever reached. A wrap here would turn an impossible
        // amount into a plausible one.
        assert!(Denomination::Btc.parse("184467440738").is_err());
        assert!(Denomination::Btc.parse("18446744073709551616").is_err());
    }

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

        assert_eq!(
            Denomination::Btc.format(100_000_000, "signet"),
            "1.00000000 sBTC"
        );
    }

    #[test]
    fn amounts_are_exact() {
        // Integer arithmetic, so no rounding artefact can move a satoshi.
        assert_eq!(Denomination::Sats.format(53_713, "bitcoin"), "53,713 sats");
        assert_eq!(
            Denomination::Btc.format(53_713, "bitcoin"),
            "0.00053713 BTC"
        );
        assert_eq!(
            Denomination::Btc.format(100_000_000, "bitcoin"),
            "1.00000000 BTC"
        );
        assert_eq!(
            Denomination::Btc.format(2_100_000_000_000_000, "bitcoin"),
            "21,000,000.00000000 BTC"
        );
        assert_eq!(Denomination::Btc.format(1, "bitcoin"), "0.00000001 BTC");
        assert_eq!(Denomination::Sats.format(0, "bitcoin"), "0 sats");
    }

    #[test]
    fn there_is_no_appearance_to_remember() {
        // Light and dark belong to the desktop. The setting that used to live
        // here let somebody contradict it, which is a way to look foreign on
        // your own machine and — once Sieve started following a desktop
        // palette — a way to put one theme's backgrounds under another's text.
        // Old files may still carry the key; serde ignores what it does not
        // know, so upgrading does not fail.
        let old = br#"{"denomination":"Btc","appearance":"Dark","show_fiat":true}"#;
        let settings: Settings = serde_json::from_slice(old).expect("an old file still loads");
        assert!(settings.show_fiat);
    }

    #[test]
    fn toggling_twice_returns() {
        assert_eq!(Denomination::Sats.toggled().toggled(), Denomination::Sats);
        assert_eq!(Denomination::Btc.toggled(), Denomination::Sats);

        // And it has to change what is on screen, which is the whole reason
        // the balance is pressable. A toggle between two names for the same
        // number would satisfy the lines above and nothing else.
        let sats = Denomination::Sats.format(53_713, "bitcoin");
        let btc = Denomination::Sats.toggled().format(53_713, "bitcoin");
        assert_ne!(sats, btc, "toggling must change what the balance reads");
        assert_eq!(
            Denomination::Sats
                .toggled()
                .toggled()
                .format(53_713, "bitcoin"),
            sats
        );
    }
}
