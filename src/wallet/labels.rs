//! Names for transactions and addresses, in BIP-329's format.
//!
//! A history of unlabelled amounts is a history nobody can use, and for a
//! wallet built around not linking coins, a label is what makes avoiding a link
//! possible later: you cannot decide to keep two payments apart if you cannot
//! remember which was which.
//!
//! Stored as BIP-329 JSONL — one JSON object per line — so labels are not
//! trapped here. The same file imports into any wallet that reads the standard,
//! and a file exported from one imports into Sieve.
//!
//! **These are not secrets in the vault's sense, and they are not encrypted.**
//! A watch-only wallet has no password at all, so there is no key to encrypt
//! them with; and a label is metadata about a chain that is already public.
//! What the file gets is the same `0600` every other Sieve file gets, and the
//! UI says plainly where it lives. Anyone who can read this file can already
//! read the transaction history sitting beside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// What a label is attached to.
///
/// BIP-329 defines more kinds than this — `pubkey`, `input`, `output`, `xpub`.
/// Sieve reads them all so an imported file is not silently truncated, and
/// writes back everything it read, but only shows the two it can attach to
/// something on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tx,
    Addr,
    #[serde(rename = "pubkey")]
    PubKey,
    Input,
    Output,
    Xpub,
}

/// One line of the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(rename = "type")]
    pub kind: Kind,
    /// What it names: a txid, an address, an outpoint.
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub label: String,
    /// Fields Sieve does not use but must not destroy: a file that round-trips
    /// through here should come out the way it went in.
    #[serde(flatten)]
    pub rest: HashMap<String, serde_json::Value>,
}

/// Every label for one wallet.
#[derive(Debug, Clone, Default)]
pub struct Labels {
    entries: HashMap<(Kind, String), Entry>,
}

impl Labels {
    /// Read the wallet's labels. A wallet with none is not an error.
    pub fn load(dir: &Path) -> Self {
        let path = file(dir);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut labels = Self::default();
        for (number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(line) {
                Ok(entry) => {
                    labels
                        .entries
                        .insert((entry.kind, entry.reference.clone()), entry);
                }
                // One bad line must not cost the rest of the file.
                Err(e) => tracing::warn!(line = number + 1, %e, "skipping an unreadable label"),
            }
        }
        labels
    }

    pub fn get(&self, kind: Kind, reference: &str) -> Option<&str> {
        self.entries
            .get(&(kind, reference.to_owned()))
            .map(|entry| entry.label.as_str())
            .filter(|label| !label.is_empty())
    }

    /// Name something, or clear its name when the text is empty.
    pub fn set(&mut self, kind: Kind, reference: &str, label: &str) {
        let label = label.trim();
        let key = (kind, reference.to_owned());
        if label.is_empty() {
            // Only the *name* is being cleared. An entry carrying anything
            // else — a frozen coin's `spendable: false`, an imported
            // `origin` — has to survive losing its name, or unnaming a coin
            // would quietly unfreeze it.
            match self.entries.get_mut(&key) {
                Some(entry) if !entry.rest.is_empty() => entry.label.clear(),
                _ => {
                    self.entries.remove(&key);
                }
            }
            return;
        }
        match self.entries.get_mut(&key) {
            // Keep whatever else the entry carried — an imported `origin`, a
            // `spendable` flag — rather than rewriting the line from scratch.
            Some(entry) => entry.label = label.to_owned(),
            None => {
                self.entries.insert(
                    key,
                    Entry {
                        kind,
                        reference: reference.to_owned(),
                        label: label.to_owned(),
                        rest: HashMap::new(),
                    },
                );
            }
        }
    }

    /// Whether this coin may be spent.
    ///
    /// BIP-329's `spendable`, which defaults to **true**: an entry without the
    /// field, or no entry at all, is an ordinary coin. Only an explicit `false`
    /// freezes one, so a label file from another wallet cannot accidentally
    /// make money unspendable here.
    ///
    /// `outpoint` is BIP-329's own form, `txid:vout`.
    pub fn spendable(&self, outpoint: &str) -> bool {
        self.entries
            .get(&(Kind::Output, outpoint.to_owned()))
            .and_then(|entry| entry.rest.get("spendable"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
    }

    /// Freeze a coin, or let it be spent again.
    ///
    /// Written into the label file rather than a store of our own, because
    /// BIP-329 already defines this field and a wallet that reads the export
    /// should see the same coins held back. Unfreezing removes the field rather
    /// than writing `true`, so an untouched coin and a deliberately released
    /// one look the same — which they are.
    pub fn set_spendable(&mut self, outpoint: &str, spendable: bool) {
        let key = (Kind::Output, outpoint.to_owned());
        match self.entries.get_mut(&key) {
            Some(entry) => {
                match spendable {
                    true => entry.rest.remove("spendable"),
                    false => entry
                        .rest
                        .insert("spendable".to_owned(), serde_json::Value::Bool(false)),
                };
                // Nothing left to say about it: no name, no flags.
                if entry.label.is_empty() && entry.rest.is_empty() {
                    self.entries.remove(&key);
                }
            }
            // Nothing recorded yet, and nothing to record unless it is being
            // frozen.
            None if !spendable => {
                let mut rest = HashMap::new();
                rest.insert("spendable".to_owned(), serde_json::Value::Bool(false));
                self.entries.insert(
                    key,
                    Entry {
                        kind: Kind::Output,
                        reference: outpoint.to_owned(),
                        label: String::new(),
                        rest,
                    },
                );
            }
            None => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The file as BIP-329 sees it: one object per line, in a stable order so
    /// two saves of the same labels produce the same bytes.
    pub fn to_jsonl(&self) -> Result<String> {
        let mut lines: Vec<(u8, &str, String)> = Vec::with_capacity(self.entries.len());
        for ((kind, reference), entry) in &self.entries {
            lines.push((
                *kind as u8,
                reference.as_str(),
                serde_json::to_string(entry).context("could not write a label")?,
            ));
        }
        lines.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));

        let mut out = String::new();
        for (_, _, line) in lines {
            out.push_str(&line);
            out.push('\n');
        }
        Ok(out)
    }

    /// Write them beside the wallet, atomically and readable only by this user.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = file(dir);
        if self.is_empty() && path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("could not clear {}", path.display()))?;
            return Ok(());
        }
        if self.is_empty() {
            return Ok(());
        }
        crate::vault::write_atomic(&path, self.to_jsonl()?.as_bytes())?;
        super::restrict(&path)
    }

    /// Merge a BIP-329 file in. Returns how many labels it carried.
    ///
    /// An imported label wins: importing is a deliberate act, and the file
    /// being imported is the one the person is holding.
    pub fn import(&mut self, text: &str) -> Result<usize> {
        let mut read = 0;
        for (number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: Entry = serde_json::from_str(line)
                .with_context(|| format!("line {} is not a BIP-329 label", number + 1))?;
            self.entries
                .insert((entry.kind, entry.reference.clone()), entry);
            read += 1;
        }
        Ok(read)
    }
}

/// Where a wallet's labels live.
pub fn file(dir: &Path) -> PathBuf {
    dir.join("labels.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TXID: &str = "208fce1aef000000000000000000000000000000000000000000000000000000";

    #[test]
    fn a_label_round_trips_through_the_file_format() {
        let mut labels = Labels::default();
        labels.set(Kind::Tx, TXID, "  Rent  ");
        labels.set(Kind::Addr, "bc1qexample", "Donations");

        let jsonl = labels.to_jsonl().unwrap();
        assert_eq!(jsonl.lines().count(), 2);

        let mut read = Labels::default();
        assert_eq!(read.import(&jsonl).unwrap(), 2);
        // Trimmed on the way in, so a stray space is not part of the name.
        assert_eq!(read.get(Kind::Tx, TXID), Some("Rent"));
        assert_eq!(read.get(Kind::Addr, "bc1qexample"), Some("Donations"));
    }

    #[test]
    fn it_writes_what_bip_329_specifies() {
        let mut labels = Labels::default();
        labels.set(Kind::Tx, TXID, "Rent");
        let line = labels.to_jsonl().unwrap();
        assert!(line.contains(r#""type":"tx""#), "{line}");
        assert!(line.contains(&format!(r#""ref":"{TXID}""#)), "{line}");
        assert!(line.contains(r#""label":"Rent""#), "{line}");
    }

    #[test]
    fn clearing_a_label_removes_it() {
        let mut labels = Labels::default();
        labels.set(Kind::Tx, TXID, "Rent");
        labels.set(Kind::Tx, TXID, "   ");
        assert_eq!(labels.get(Kind::Tx, TXID), None);
        assert!(labels.is_empty());
    }

    #[test]
    fn fields_sieve_does_not_use_survive_a_round_trip() {
        // A file from another wallet may carry more than Sieve shows. Dropping
        // it would quietly damage someone's labels on the way through.
        let line =
            format!(r#"{{"type":"output","ref":"{TXID}:0","label":"Change","spendable":false}}"#);
        let mut labels = Labels::default();
        labels.import(&line).unwrap();
        assert!(labels.to_jsonl().unwrap().contains(r#""spendable":false"#));
    }

    #[test]
    fn one_bad_line_does_not_cost_the_file() {
        let dir = std::env::temp_dir().join(format!("sieve-labels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            file(&dir),
            format!("{{ not json }}\n{{\"type\":\"tx\",\"ref\":\"{TXID}\",\"label\":\"Rent\"}}\n"),
        )
        .unwrap();

        let labels = Labels::load(&dir);
        assert_eq!(labels.get(Kind::Tx, TXID), Some("Rent"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// BIP-329 already defines `spendable`, so freezing writes into the file
    /// every other label lives in rather than a store of Sieve's own — and a
    /// wallet reading the export sees the same coins held back.
    #[test]
    fn a_coin_can_be_frozen_and_released() {
        let outpoint = format!("{TXID}:1");
        let mut labels = Labels::default();

        // Nothing recorded means spendable. A label file from another wallet
        // cannot accidentally make money unspendable here.
        assert!(labels.spendable(&outpoint));

        labels.set_spendable(&outpoint, false);
        assert!(!labels.spendable(&outpoint));
        assert!(labels.to_jsonl().unwrap().contains(r#""spendable":false"#));

        // Releasing removes the field rather than writing `true`, so a coin
        // nobody touched and one deliberately released look the same — which
        // they are.
        labels.set_spendable(&outpoint, true);
        assert!(labels.spendable(&outpoint));
        assert!(!labels.to_jsonl().unwrap().contains("spendable"));
    }

    /// Clearing a name used to delete the whole entry, which would have taken
    /// the freeze with it: a coin would quietly become spendable because
    /// somebody unnamed it.
    #[test]
    fn unnaming_a_frozen_coin_leaves_it_frozen() {
        let outpoint = format!("{TXID}:0");
        let mut labels = Labels::default();
        labels.set(Kind::Output, &outpoint, "From an exchange");
        labels.set_spendable(&outpoint, false);

        labels.set(Kind::Output, &outpoint, "");
        assert_eq!(
            labels.get(Kind::Output, &outpoint),
            None,
            "the name is gone"
        );
        assert!(!labels.spendable(&outpoint), "and the freeze is not");

        // Releasing a coin with no name left drops the entry entirely, rather
        // than leaving a line that says nothing.
        labels.set_spendable(&outpoint, true);
        assert!(labels.is_empty());
    }
}
