//! Derivation paths, and the wallets that live on them.
//!
//! One seed produces a different wallet on every standard derivation path, and
//! they share nothing: an address derived under BIP84 tells you nothing about
//! the coins sitting under BIP44. So restoring a seed means opening *all* the
//! standard paths, not guessing one.
//!
//! This costs almost nothing to sync. A compact block filter covers an entire
//! block regardless of what you are looking for, so the download is identical
//! whether you watch one path or four — only the local script matching grows.
//!
//! BDK's `Wallet` holds exactly two descriptors (receive and change), so each
//! path is its own wallet with its own database file, and `bdk_kyoto` drives
//! them all from a single node.

use std::fmt;
use std::path::Path;

use anyhow::{Result, anyhow};
use bdk_wallet::bitcoin::Network;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::chain::DescriptorExt;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::template::{Bip44, Bip49, Bip84, Bip86};
use bdk_wallet::{KeychainKind, PersistedWallet, Wallet};

use super::restrict;

/// How many unused addresses past the last used one must be checked before a
/// keychain is considered exhausted. Twenty is the figure every other wallet
/// uses, so a wallet created elsewhere will not have left a wider hole.
pub const GAP_LIMIT: u32 = 20;

/// How far ahead to derive scripts when importing.
///
/// BDK's default is 25, which is fine for a wallet starting empty but far too
/// small for one that already has history: a recovery scan only tests the
/// scripts it has derived, so an address used at index 60 would never be seen.
/// The scan cost of a wider window is local matching only — the filters
/// downloaded are identical.
pub const IMPORT_LOOKAHEAD: u32 = 200;

/// A standard derivation path, identified by its BIP purpose field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ScriptType {
    /// BIP44, `1...` addresses.
    Legacy,
    /// BIP49, `3...` addresses.
    NestedSegwit,
    /// BIP84, `bc1q...` addresses. What most wallets default to.
    NativeSegwit,
    /// BIP86, `bc1p...` addresses. What Sieve creates.
    Taproot,
}

impl ScriptType {
    /// Every path a restore should open, oldest first so the list reads the way
    /// the ecosystem grew.
    pub const ALL: [ScriptType; 4] = [
        ScriptType::Legacy,
        ScriptType::NestedSegwit,
        ScriptType::NativeSegwit,
        ScriptType::Taproot,
    ];

    pub fn purpose(self) -> u32 {
        match self {
            ScriptType::Legacy => 44,
            ScriptType::NestedSegwit => 49,
            ScriptType::NativeSegwit => 84,
            ScriptType::Taproot => 86,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScriptType::Legacy => "Legacy",
            ScriptType::NestedSegwit => "Nested SegWit",
            ScriptType::NativeSegwit => "Native SegWit",
            ScriptType::Taproot => "Taproot",
        }
    }

    /// How addresses on this path look, which is how people actually recognise
    /// which wallet a seed came from.
    pub fn example_prefix(self, network: Network) -> &'static str {
        match (self, network) {
            (ScriptType::Legacy, Network::Bitcoin) => "1…",
            (ScriptType::NestedSegwit, Network::Bitcoin) => "3…",
            (ScriptType::NativeSegwit, Network::Bitcoin) => "bc1q…",
            (ScriptType::Taproot, Network::Bitcoin) => "bc1p…",
            (ScriptType::Legacy, _) => "m/n…",
            (ScriptType::NestedSegwit, _) => "2…",
            (ScriptType::NativeSegwit, _) => "tb1q…",
            (ScriptType::Taproot, _) => "tb1p…",
        }
    }

    /// The earliest block this path could possibly hold coins in.
    ///
    /// Taproot did not exist before block 709,632, so a BIP86 wallet scanned
    /// from earlier is scanning blocks it cannot match. The others go back to
    /// the beginning.
    // Under test rather than in use: no scan currently starts from a script
    // type's own floor, but the fact that taproot did not exist before block
    // 709,632 is worth keeping true.
    #[allow(dead_code)]
    pub fn earliest_possible(self, network: Network) -> Option<u32> {
        match (self, network) {
            (ScriptType::Taproot, Network::Bitcoin) => Some(709_632),
            _ => None,
        }
    }

    /// The output descriptor wrapping a single key, for imports that carry one
    /// key rather than a seed.
    ///
    /// A WIF key has no derivation, so the same key can have been used under
    /// any of these script types and each has to be checked separately.
    pub fn single_key_descriptor(self, key: &str) -> String {
        match self {
            ScriptType::Legacy => format!("pkh({key})"),
            ScriptType::NestedSegwit => format!("sh(wpkh({key}))"),
            ScriptType::NativeSegwit => format!("wpkh({key})"),
            ScriptType::Taproot => format!("tr({key})"),
        }
    }

    /// Its own database file — BDK's SQLite tables have fixed names, so paths
    /// cannot share one.
    pub fn db_file(self) -> String {
        format!("wallet-bip{}.sqlite", self.purpose())
    }
}

impl fmt::Display for ScriptType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (BIP{})", self.label(), self.purpose())
    }
}

/// What the user brings to an import.
///
/// Each kind expands across the script types differently: a seed derives a
/// full HD wallet per path, a bare key is watched under each path, and a
/// descriptor already names its own script type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// 12 or 24 BIP-39 words, optionally with a BIP-39 passphrase.
    Mnemonic,
    /// A single private key in Wallet Import Format.
    Wif,
    /// A BIP32 extended private key. Many wallets export one of these and
    /// never show a recovery phrase at all.
    ExtendedKey,
    /// An output descriptor or extended public key — watch-only, no keys.
    Descriptor,
    /// A device on the end of a USB cable. Watch-only here too: what comes
    /// across is a public key, and signing stays on the device.
    Hardware,
}

impl CredentialKind {
    pub fn label(self) -> &'static str {
        match self {
            // Short, because a ComboRow shows the chosen one in the space
            // left over on the right and truncates whatever will not fit. The
            // detail belongs in the group description underneath.
            CredentialKind::Mnemonic => "Recovery phrase",
            CredentialKind::Wif => "Private key",
            CredentialKind::ExtendedKey => "Extended key",
            CredentialKind::Descriptor => "Descriptor or xpub",
            CredentialKind::Hardware => "Hardware wallet",
        }
    }

    /// Whether importing this hands Sieve the ability to spend.
    ///
    /// Worth surfacing: a descriptor import cannot lose money to a bug in this
    /// software, and the other two can.
    pub fn carries_keys(self) -> bool {
        !matches!(self, CredentialKind::Descriptor | CredentialKind::Hardware)
    }

    /// Whether this credential derives many addresses or holds exactly one.
    pub fn is_hd(self) -> bool {
        matches!(self, CredentialKind::Mnemonic | CredentialKind::ExtendedKey)
    }
}

/// One derivation path's wallet, with the connection that persists it.
pub struct Account {
    pub script_type: ScriptType,
    pub wallet: PersistedWallet<Connection>,
    pub conn: Connection,
}

impl Account {
    /// Create the wallet for one path from an extended private key.
    ///
    /// The key goes in; only public descriptors come out into the database.
    pub fn create(
        xprv: Xpriv,
        script_type: ScriptType,
        db: &Path,
        network: Network,
        lookahead: u32,
    ) -> Result<Self> {
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(db)?;
        restrict(db)?;

        // Each template has a different script context, so the builder chain
        // has to complete inside the match arm.
        let wallet = match script_type {
            ScriptType::Legacy => Wallet::create(
                Bip44(xprv, KeychainKind::External),
                Bip44(xprv, KeychainKind::Internal),
            )
            .network(network)
            .lookahead(lookahead)
            .create_wallet(&mut conn),
            ScriptType::NestedSegwit => Wallet::create(
                Bip49(xprv, KeychainKind::External),
                Bip49(xprv, KeychainKind::Internal),
            )
            .network(network)
            .lookahead(lookahead)
            .create_wallet(&mut conn),
            ScriptType::NativeSegwit => Wallet::create(
                Bip84(xprv, KeychainKind::External),
                Bip84(xprv, KeychainKind::Internal),
            )
            .network(network)
            .lookahead(lookahead)
            .create_wallet(&mut conn),
            ScriptType::Taproot => Wallet::create(
                Bip86(xprv, KeychainKind::External),
                Bip86(xprv, KeychainKind::Internal),
            )
            .network(network)
            .lookahead(lookahead)
            .create_wallet(&mut conn),
        }
        .map_err(|e| anyhow!("could not create the {script_type} wallet: {e}"))?;

        Ok(Account {
            script_type,
            wallet,
            conn,
        })
    }

    /// Create a wallet holding exactly one key, from WIF.
    ///
    /// `create_single` rather than `create`: a bare key has no change chain, so
    /// there is no second descriptor to give it.
    pub fn create_from_wif(
        wif: &str,
        script_type: ScriptType,
        db: &Path,
        network: Network,
    ) -> Result<Self> {
        Self::create_single(
            &script_type.single_key_descriptor(wif),
            script_type,
            db,
            network,
        )
    }

    /// Create a watch-only wallet from a descriptor the user supplied.
    ///
    /// Accepts public descriptors, which is the safest way to import an
    /// existing wallet: Sieve can see the coins and never the keys.
    pub fn create_single(
        descriptor: &str,
        script_type: ScriptType,
        db: &Path,
        network: Network,
    ) -> Result<Self> {
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(db)?;
        restrict(db)?;

        let wallet = Wallet::create_single(descriptor.to_owned())
            .network(network)
            .create_wallet(&mut conn)
            .map_err(|e| anyhow!("could not import the {script_type} key: {e}"))?;

        Ok(Account {
            script_type,
            wallet,
            conn,
        })
    }

    /// Create a wallet from public descriptors — the hardware-wallet case.
    ///
    /// `create_single` would take only one descriptor, leaving change on the
    /// receive chain; a device's wallet has both, so both are given.
    pub fn create_watching(
        external: &str,
        internal: &str,
        script_type: ScriptType,
        db: &Path,
        network: Network,
    ) -> Result<Self> {
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(db)?;
        restrict(db)?;

        let wallet = Wallet::create(external.to_owned(), internal.to_owned())
            .network(network)
            .lookahead(IMPORT_LOOKAHEAD)
            .create_wallet(&mut conn)
            .map_err(|e| anyhow!("could not read that descriptor: {e}"))?;

        Ok(Account {
            script_type,
            wallet,
            conn,
        })
    }

    /// Reopen a wallet watch-only. No key material is involved: the database
    /// holds public descriptors, which is all that browsing needs.
    pub fn load(script_type: ScriptType, db: &Path, network: Network) -> Result<Option<Self>> {
        if !db.exists() {
            return Ok(None);
        }
        let mut conn = Connection::open(db)?;
        restrict(db)?;
        let loaded = Wallet::load()
            .check_network(network)
            .load_wallet(&mut conn)
            .map_err(|e| anyhow!("could not load the {script_type} wallet: {e}"))?;

        Ok(loaded.map(|wallet| Account {
            script_type,
            wallet,
            conn,
        }))
    }

    /// Identifies this account when updates arrive for several wallets at once.
    pub fn descriptor_id(&self) -> bdk_wallet::chain::DescriptorId {
        self.wallet
            .public_descriptor(KeychainKind::External)
            .descriptor_id()
    }

    pub fn persist(&mut self) -> Result<()> {
        self.wallet
            .persist(&mut self.conn)
            .map(|_| ())
            .map_err(|e| anyhow!("could not persist the {} wallet: {e}", self.script_type))
    }
}

impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account")
            .field("script_type", &self.script_type)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published BIP-44/49/84/86 test vectors for the standard test
    /// mnemonic. If Sieve ever derives a different address from the same
    /// words, it is handing out addresses the seed's owner cannot recover
    /// from — which is why this is checked against numbers written down
    /// outside this codebase rather than against ourselves.
    #[test]
    fn the_first_address_of_each_path_matches_the_published_vectors() {
        use bdk_wallet::bitcoin::bip32::Xpriv;
        use bdk_wallet::keys::{
            DerivableKey, ExtendedKey,
            bip39::{Language, Mnemonic},
        };
        use bdk_wallet::miniscript::Tap;

        const PHRASE: &str = "abandon abandon abandon abandon abandon abandon \
                              abandon abandon abandon abandon abandon about";

        let expected = [
            (ScriptType::Legacy, "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA"),
            (
                ScriptType::NestedSegwit,
                "37VucYSaXLCAsxYyAPfbSi9eh4iEcbShgf",
            ),
            (
                ScriptType::NativeSegwit,
                "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu",
            ),
            (
                ScriptType::Taproot,
                "bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr",
            ),
        ];

        let mnemonic = Mnemonic::parse_in(Language::English, PHRASE).unwrap();
        let xkey: ExtendedKey<Tap> = (mnemonic, None).into_extended_key().unwrap();
        let xprv: Xpriv = xkey.into_xprv(Network::Bitcoin.into()).unwrap();

        let dir = std::env::temp_dir().join(format!("sieve-vectors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for (script_type, address) in expected {
            let db = dir.join(script_type.db_file());
            let account = Account::create(xprv, script_type, &db, Network::Bitcoin, 25).unwrap();
            let first = account
                .wallet
                .peek_address(KeychainKind::External, 0)
                .address
                .to_string();
            assert_eq!(
                first, address,
                "{script_type} derived the wrong first address"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_path_has_its_own_database() {
        let files: Vec<String> = ScriptType::ALL.iter().map(|s| s.db_file()).collect();
        let unique: std::collections::HashSet<_> = files.iter().collect();
        assert_eq!(
            files.len(),
            unique.len(),
            "paths must not share a database file"
        );
    }

    #[test]
    fn single_key_descriptors_are_well_formed() {
        // A WIF key has no derivation, so the same key may have been used under
        // any script type and each needs its own descriptor.
        let key = "cVt4o7BGAig1UXywgGSmARhxMdzP5qvQsxKkSsc1XEkw3tDTQFpy";
        assert_eq!(
            ScriptType::Legacy.single_key_descriptor(key),
            format!("pkh({key})")
        );
        assert_eq!(
            ScriptType::NestedSegwit.single_key_descriptor(key),
            format!("sh(wpkh({key}))")
        );
        assert_eq!(
            ScriptType::NativeSegwit.single_key_descriptor(key),
            format!("wpkh({key})")
        );
        assert_eq!(
            ScriptType::Taproot.single_key_descriptor(key),
            format!("tr({key})")
        );
    }

    #[test]
    fn a_wif_key_imports_under_every_script_type() {
        // Signet WIF. Every script type must accept it and produce a wallet.
        let key = "cVt4o7BGAig1UXywgGSmARhxMdzP5qvQsxKkSsc1XEkw3tDTQFpy";
        let dir = std::env::temp_dir().join(format!("sieve-wif-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for script_type in ScriptType::ALL {
            let db = dir.join(script_type.db_file());
            let mut account = Account::create_from_wif(key, script_type, &db, Network::Signet)
                .unwrap_or_else(|e| panic!("{script_type} should accept a WIF key: {e}"));
            let address = account
                .wallet
                .next_unused_address(KeychainKind::External)
                .address
                .to_string();
            assert!(!address.is_empty(), "{script_type} produced no address");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_import_window_clears_the_standard_gap_limit() {
        // BDK's default of 25 is barely above the gap limit every other wallet
        // uses, which leaves no room for a wallet that already has history.
        // A `const` block, so this fails the build rather than the test: two
        // constants that have drifted apart are not a thing to find out about
        // when somebody happens to run the suite.
        const {
            assert!(
                IMPORT_LOOKAHEAD > GAP_LIMIT * 4,
                "the import window is too tight for the gap limit"
            )
        };
        assert_eq!(GAP_LIMIT, 20, "the ecosystem standard");
    }

    #[test]
    fn only_taproot_has_an_activation_floor() {
        for script_type in ScriptType::ALL {
            let floor = script_type.earliest_possible(Network::Bitcoin);
            if script_type == ScriptType::Taproot {
                assert_eq!(floor, Some(709_632));
            } else {
                assert_eq!(
                    floor, None,
                    "{script_type} predates taproot and has no floor"
                );
            }
        }
    }
}

/// Every derivation path being watched for one seed, and which of them the UI
/// treats as the default for receiving.
pub struct Portfolio {
    pub accounts: Vec<Account>,
    pub primary: ScriptType,
}

impl Portfolio {
    /// Open all the paths an HD credential could have used.
    ///
    /// A path whose database is missing is simply absent — a wallet created
    /// before a path was supported must still load.
    pub fn load(
        dir: &Path,
        script_types: &[ScriptType],
        primary: ScriptType,
        network: Network,
    ) -> Result<Self> {
        let mut accounts = Vec::new();
        for script_type in script_types {
            let db = dir.join(script_type.db_file());
            if let Some(account) = Account::load(*script_type, &db, network)? {
                accounts.push(account);
            }
        }
        Ok(Portfolio { accounts, primary })
    }

    pub fn create_from_xprv(
        xprv: Xpriv,
        dir: &Path,
        script_types: &[ScriptType],
        primary: ScriptType,
        network: Network,
        lookahead: u32,
    ) -> Result<Self> {
        let mut accounts = Vec::new();
        for script_type in script_types {
            let db = dir.join(script_type.db_file());
            accounts.push(Account::create(
                xprv,
                *script_type,
                &db,
                network,
                lookahead,
            )?);
        }
        Ok(Portfolio { accounts, primary })
    }

    pub fn create_from_wif(
        wif: &str,
        dir: &Path,
        script_types: &[ScriptType],
        primary: ScriptType,
        network: Network,
    ) -> Result<Self> {
        let mut accounts = Vec::new();
        for script_type in script_types {
            let db = dir.join(script_type.db_file());
            accounts.push(Account::create_from_wif(wif, *script_type, &db, network)?);
        }
        Ok(Portfolio { accounts, primary })
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Extend each keychain so there are always `GAP_LIMIT` unused addresses
    /// past the last used one, and report whether anything moved.
    ///
    /// A recovery scan only tests scripts that have been derived. If a wallet's
    /// last used address sits near the edge of what was derived, there may be
    /// more beyond it that were never checked — so the window is widened and
    /// the caller rescans. Without this, coins past the initial window are
    /// invisible and nothing says so.
    pub fn extend_gaps(&mut self) -> Result<bool> {
        let mut extended = false;
        for account in self.accounts.iter_mut() {
            for keychain in [KeychainKind::External, KeychainKind::Internal] {
                let Some(last_used) = account.wallet.spk_index().last_used_index(keychain) else {
                    continue;
                };
                let revealed = account.wallet.derivation_index(keychain).unwrap_or(0);
                let target = last_used.saturating_add(GAP_LIMIT);
                if revealed < target {
                    tracing::info!(
                        path = %account.script_type,
                        ?keychain,
                        last_used,
                        revealed,
                        target,
                        "extending the gap window; a rescan will follow"
                    );
                    let _ = account.wallet.reveal_addresses_to(keychain, target);
                    extended = true;
                }
            }
            account.persist()?;
        }
        Ok(extended)
    }

    /// Route an update to the account it belongs to.
    pub fn account_for(&mut self, id: bdk_wallet::chain::DescriptorId) -> Option<&mut Account> {
        self.accounts.iter_mut().find(|a| a.descriptor_id() == id)
    }
}

impl fmt::Debug for Portfolio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Portfolio")
            .field("paths", &self.accounts.len())
            .field("primary", &self.primary)
            .finish()
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use bdk_wallet::bitcoin::{
        Amount, Network, OutPoint, Transaction, TxIn, TxOut, Txid, absolute, transaction,
    };
    use bdk_wallet::chain::ChainPosition;

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon \
                          abandon abandon abandon abandon abandon about";

    /// A transaction Sieve broadcast is recorded before it is in a block, and
    /// that record has to survive a restart: the wallet cannot see a mempool,
    /// so if the row were lost on quit, a payment already on the network would
    /// simply vanish from the history until it confirmed.
    #[test]
    fn an_unconfirmed_transaction_survives_a_restart() {
        let network = Network::Signet;
        let dir = std::env::temp_dir().join(format!("sieve-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join(ScriptType::Taproot.db_file());

        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let txid = {
            let mut account = Account::create(xprv, ScriptType::Taproot, &db, network, 25).unwrap();
            let address = account
                .wallet
                .reveal_next_address(KeychainKind::External)
                .address;
            let tx = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(
                        "0000000000000000000000000000000000000000000000000000000000000001"
                            .parse::<Txid>()
                            .unwrap(),
                        0,
                    ),
                    ..Default::default()
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(50_000),
                    script_pubkey: address.script_pubkey(),
                }],
            };
            let txid = tx.compute_txid();
            account
                .wallet
                .apply_unconfirmed_txs([(tx, 1_700_000_000u64)]);
            account.persist().unwrap();
            txid
        };

        // Reopened the way a restart reopens it: from the database alone.
        let account = Account::load(ScriptType::Taproot, &db, network)
            .unwrap()
            .unwrap();
        let found = account
            .wallet
            .transactions()
            .find(|tx| tx.tx_node.txid == txid)
            .expect("the pending transaction did not survive the restart");
        assert!(
            matches!(found.chain_position, ChainPosition::Unconfirmed { .. }),
            "it came back, but not as pending"
        );
        assert_eq!(
            account.wallet.balance().untrusted_pending,
            Amount::from_sat(50_000)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
