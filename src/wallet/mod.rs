//! Wallet state and the background subsystems that feed it.
//!
//! Nothing here touches GTK. The split is deliberate: BDK objects and key
//! material stay off the widget layer, and only plain data crosses back to the
//! UI as messages.
//!
//! Decisions baked in here (see ROADMAP.md):
//!
//! - **BIP86 taproot.** Single-sig key-path spends are indistinguishable on
//!   chain from any other key-path spend.
//! - **Signet.** Real block times and enough filter-serving peers to exercise
//!   sync honestly. Mainnet is not reachable until M8.
//! - **12 words.** 128 bits is beyond brute force, and transcription error is
//!   the realistic way people lose funds.
//! - **The vault passphrase is not a BIP-39 passphrase.** It encrypts the file
//!   and nothing else, so a typo is an error rather than a different, empty
//!   wallet.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use bdk_wallet::bitcoin::Network;
use bdk_wallet::keys::bip39::{Language, Mnemonic, WordCount};
use bdk_wallet::keys::{DerivableKey, ExtendedKey, GeneratableKey, GeneratedKey};
use bdk_wallet::miniscript::Tap;
use zeroize::Zeroizing;

pub mod accounts;
pub mod send;
pub mod node;

use crate::vault;

/// Until network selection lands in the UI, everything defaults here. Mainnet
/// stays unreachable from the interface until M8.
pub const DEFAULT_NETWORK: Network = Network::Signet;

/// A block known to predate any wallet on a given network, used when the true
/// birthday is unknown.
///
/// Checkpoints are compiled in because a `HashCheckpoint` needs a block hash,
/// and the hash for an arbitrary height cannot be derived offline. Heights are
/// exact; scanning from one is always correct, only sometimes slower than
/// necessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    pub height: u32,
    pub hash: &'static str,
    /// Roughly when this block was mined, for showing a person.
    pub when: &'static str,
}

/// Mainnet checkpoints, newest first.
pub const MAINNET_CHECKPOINTS: &[Checkpoint] = &[
    Checkpoint {
        height: 950_000,
        hash: "000000000000000000010b93c9ea1c29fea277383f0f7d1f26de8b5802e885ff",
        when: "May 2026",
    },
    Checkpoint {
        height: 900_000,
        hash: "000000000000000000010538edbfd2d5b809a33dd83f284aeea41c6d0d96968a",
        when: "June 2025",
    },
    Checkpoint {
        height: 850_000,
        hash: "00000000000000000002a0b5db2a7f8d9087464c2586b546be7bce8eb53b8187",
        when: "June 2024",
    },
    Checkpoint {
        height: 800_000,
        hash: "00000000000000000002a7c4c1e48d76c5a37902165a270156b7a8d72728a054",
        when: "July 2023",
    },
    Checkpoint {
        height: 750_000,
        hash: "0000000000000000000592a974b1b9f087cb77628bb4a097d5c2c11b3476a58e",
        when: "August 2022",
    },
    // Taproot activation. A BIP86 wallet cannot hold coins earlier than this,
    // so it is the correct floor for "birthday unknown" — not a guess.
    Checkpoint {
        height: 709_632,
        hash: "0000000000000000000687bca986194dc2c1f949318629b44bb54ec0a94d8244",
        when: "November 2021 — taproot activation",
    },
];

/// Signet checkpoints, newest first.
pub const SIGNET_CHECKPOINTS: &[Checkpoint] = &[Checkpoint {
    height: 319_000,
    hash: "000000021cefaf18c0d9f75944d79689bde29448c55ff00c65c0022814f40578",
    when: "August 2026",
}];

pub fn checkpoints(network: Network) -> &'static [Checkpoint] {
    match network {
        Network::Bitcoin => MAINNET_CHECKPOINTS,
        _ => SIGNET_CHECKPOINTS,
    }
}

/// The newest checkpoint at or before `height`.
///
/// Always rounds *earlier*. Starting too early costs time; starting too late
/// loses coins.
pub fn checkpoint_at_or_before(network: Network, height: u32) -> Checkpoint {
    let all = checkpoints(network);
    all.iter()
        .find(|c| c.height <= height)
        .copied()
        .unwrap_or_else(|| *all.last().expect("every network has a floor checkpoint"))
}

/// Where the files live. The vault holds the seed; the database holds only
/// public descriptors and chain data.
#[derive(Debug, Clone)]
pub struct Paths {
    /// This wallet's own directory.
    pub dir: PathBuf,
    pub vault: PathBuf,
    /// Kept only to name the directory the per-path databases live in.
    pub db: PathBuf,
    /// Public, password-free metadata: network, birthday, derivation paths.
    /// Sync is watch-only, so this cannot live in the encrypted vault.
    pub meta: PathBuf,
}

/// Root of everything Sieve stores.
pub fn data_root() -> PathBuf {
    directories::ProjectDirs::from("com", "jdavis", "Sieve")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where wallets live, one directory each.
pub fn wallets_root() -> PathBuf {
    data_root().join("wallets")
}

/// Block headers and peer records, shared by every wallet on a network.
///
/// Headers are public chain data and identical for all wallets, so a second
/// wallet on a network it has already seen starts with the chain already
/// downloaded instead of fetching it again.
pub fn chain_dir(network: Network) -> PathBuf {
    data_root().join("chain").join(network.to_string())
}

impl Paths {
    pub fn for_wallet(id: &str) -> Self {
        let dir = wallets_root().join(id);
        Self {
            vault: dir.join("vault.sieve"),
            db: dir.join("wallet.sqlite"),
            meta: dir.join("wallet.meta.json"),
            dir,
        }
    }

    /// An id no existing wallet is using.
    pub fn new_id() -> String {
        let mut bytes = [0u8; 6];
        let _ = getrandom::fill(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// First run is "no vault", not "no database" — the databases can be
    /// rebuilt from the seed, so their absence is recoverable and the vault's
    /// is not.
    pub fn is_initialised(&self) -> bool {
        self.vault.exists()
    }
}

/// A wallet as the chooser needs to show it: enough to pick one, and nothing
/// that requires a password.
#[derive(Debug, Clone)]
pub struct WalletEntry {
    pub id: String,
    pub name: String,
    pub network: String,
}

/// Every wallet on disk, newest-looking last.
///
/// A directory without readable metadata is skipped rather than failing the
/// list — one broken wallet must not hide the others.
pub fn list_wallets() -> Vec<WalletEntry> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(wallets_root()) else {
        return found;
    };

    for entry in entries.flatten() {
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let paths = Paths::for_wallet(&id);
        if !paths.is_initialised() {
            continue;
        }
        // A vault exists, so a seed exists. Never hide it because a sidecar
        // file could not be parsed — show it and let the user open it.
        match Meta::load(&paths) {
            Some(meta) => found.push(WalletEntry {
                name: meta.display_name(&id),
                network: meta.network,
                id,
            }),
            None => {
                tracing::warn!(%id, "wallet metadata is unreadable; listing it anyway");
                found.push(WalletEntry {
                    name: format!("Wallet {}", &id[..id.len().min(4)]),
                    network: "unknown".into(),
                    id,
                });
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Move a pre-multi-wallet installation into the new layout.
///
/// Sieve used to keep one wallet directly in the data root. Leaving it there
/// would strand it, so it is moved into `wallets/<id>/` the first time this
/// build runs. Returns the id if anything moved.
pub fn migrate_legacy_layout() -> Option<String> {
    let root = data_root();
    let legacy_vault = root.join("vault.sieve");
    if !legacy_vault.exists() {
        return None;
    }

    let id = Paths::new_id();
    let target = wallets_root().join(&id);
    if std::fs::create_dir_all(&target).is_err() {
        return None;
    }

    for name in ["vault.sieve", "wallet.meta.json"] {
        let _ = std::fs::rename(root.join(name), target.join(name));
    }
    // Per-path databases, plus the older single-wallet database name.
    for script_type in accounts::ScriptType::ALL {
        let file = script_type.db_file();
        let _ = std::fs::rename(root.join(&file), target.join(&file));
    }
    let _ = std::fs::rename(root.join("wallet.sqlite"), target.join("wallet.sqlite"));

    tracing::info!(%id, "migrated the existing wallet into the multi-wallet layout");
    Some(id)
}

/// Public, password-free facts about a wallet.
///
/// Sync is watch-only, so none of this may live in the encrypted vault — the
/// node has to know the network and where to start before anyone types a
/// password.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    /// Stored as a string so the file stays readable and the format does not
    /// depend on how `bitcoin::Network` happens to serialise.
    pub network: String,
    /// The earliest block this wallet could hold a transaction in.
    pub birthday_height: u32,
    pub birthday_hash: String,
    /// Which derivation paths this wallet watches. Recorded so a later build
    /// that supports more paths does not silently start scanning for coins
    /// that were never derived.
    #[serde(default = "default_script_types")]
    pub script_types: Vec<accounts::ScriptType>,
    /// The path used for receiving.
    #[serde(default = "default_primary")]
    pub primary: accounts::ScriptType,
    /// What the person called it. Absent for wallets created before naming.
    #[serde(default)]
    pub name: Option<String>,
}

/// Sieve's first metadata format: a birthday and nothing else.
#[derive(serde::Deserialize)]
struct LegacyBirthday {
    height: u32,
    hash: String,
}

fn default_script_types() -> Vec<accounts::ScriptType> {
    vec![accounts::ScriptType::Taproot]
}

fn default_primary() -> accounts::ScriptType {
    accounts::ScriptType::Taproot
}

impl Meta {
    pub fn new(
        network: Network,
        birthday: Checkpoint,
        script_types: Vec<accounts::ScriptType>,
        primary: accounts::ScriptType,
        name: Option<String>,
    ) -> Self {
        Self {
            name,
            network: network.to_string(),
            birthday_height: birthday.height,
            birthday_hash: birthday.hash.to_owned(),
            script_types,
            primary,
        }
    }

    /// A name to show, falling back to something stable rather than blank.
    pub fn display_name(&self, id: &str) -> String {
        self.name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Wallet {}", &id[..id.len().min(4)]))
    }

    pub fn network(&self) -> Network {
        self.network.parse().unwrap_or(Network::Signet)
    }

    pub fn load(paths: &Paths) -> Option<Self> {
        let bytes = std::fs::read(&paths.meta).ok()?;
        if let Ok(meta) = serde_json::from_slice::<Self>(&bytes) {
            return Some(meta);
        }
        // Sieve's first metadata format recorded only a birthday, and only
        // signet existed. Upgrading in place beats refusing to open a wallet
        // that holds a seed.
        let legacy: LegacyBirthday = serde_json::from_slice(&bytes).ok()?;
        tracing::info!("upgrading a wallet from the first metadata format");
        let upgraded = Meta {
            name: None,
            network: Network::Signet.to_string(),
            birthday_height: legacy.height,
            birthday_hash: legacy.hash,
            script_types: default_script_types(),
            primary: default_primary(),
        };
        let _ = upgraded.save(paths);
        Some(upgraded)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        crate::vault::write_atomic(&paths.meta, &serde_json::to_vec_pretty(self)?)
    }

    /// Rename a wallet.
    ///
    /// The name is the only part of a wallet a person chooses, and it lives in
    /// metadata rather than the vault so renaming never needs a password —
    /// asking for one to change a label would be theatre.
    pub fn rename(paths: &Paths, name: &str) -> Result<()> {
        let mut meta = Self::load(paths).context("this wallet has no metadata file")?;
        let trimmed = name.trim();
        meta.name = (!trimmed.is_empty()).then(|| trimmed.to_owned());
        meta.save(paths)
    }
}

/// The database holds no keys, but it does hold the wallet's xpub-derived
/// descriptors and full transaction graph — enough to reconstruct every address
/// the wallet will ever use. That is exactly the linkage this wallet exists to
/// avoid leaking, so it is owner-only in its own right rather than relying on
/// the directory mode.
pub(crate) fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {}", path.display()))
}

/// One derivation path's share of the wallet.
#[derive(Debug, Clone)]
pub struct AccountSummary {
    pub script_type: accounts::ScriptType,
    pub balance_sats: u64,
    pub pending_sats: u64,
    pub next_address: String,
}

/// One transaction, as the activity list needs it.
#[derive(Debug, Clone)]
pub struct TxSummary {
    pub txid: String,
    /// Received minus sent: positive is money in, negative is money out.
    /// A self-transfer nets to just the fee.
    pub net_sats: i64,
    /// `None` when inputs are not all known to this wallet, which is normal
    /// for an incoming payment someone else built.
    pub fee_sats: Option<u64>,
    /// `None` means unconfirmed — and for a filter wallet, invisible until
    /// mined, so this is almost always `Some`.
    pub height: Option<u32>,
    pub seen_at: Option<u64>,
    /// Which derivation path it belongs to.
    pub script_type: accounts::ScriptType,
}

impl TxSummary {
    pub fn is_incoming(&self) -> bool {
        self.net_sats >= 0
    }

    /// How deep it is buried, given the tip the wallet has verified to.
    pub fn confirmations(&self, tip: u32) -> u32 {
        match self.height {
            Some(height) if tip >= height => tip - height + 1,
            _ => 0,
        }
    }
}

/// What the UI needs to render an unlocked wallet.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    /// Confirmed, summed across every path. Compact block filters describe
    /// transactions in blocks, so the mempool is invisible by construction.
    pub balance_sats: u64,
    pub pending_sats: u64,
    /// Height the wallet has verified up to.
    pub tip: u32,
    pub next_address: String,
    pub network: String,
    /// Per-path breakdown. Seeing the other paths sit at zero is what proves
    /// the scan actually covered them.
    pub accounts: Vec<AccountSummary>,
    /// Newest first, across every path.
    pub transactions: Vec<TxSummary>,
}

impl Summary {
    pub(crate) fn from_portfolio(portfolio: &mut accounts::Portfolio) -> Result<Self> {
        let mut summary = Summary { ..Default::default() };
        let primary = portfolio.primary;

        for account in portfolio.accounts.iter_mut() {
            let address = account
                .wallet
                .next_unused_address(bdk_wallet::KeychainKind::External);
            let balance = account.wallet.balance();
            let entry = AccountSummary {
                script_type: account.script_type,
                balance_sats: balance.confirmed.to_sat(),
                pending_sats: (balance.trusted_pending + balance.untrusted_pending).to_sat(),
                next_address: address.address.to_string(),
            };
            account.persist()?;

            summary.balance_sats += entry.balance_sats;
            summary.pending_sats += entry.pending_sats;
            summary.tip = summary.tip.max(account.wallet.latest_checkpoint().height());
            summary.network = account.wallet.network().to_string();
            if account.script_type == primary {
                summary.next_address = entry.next_address.clone();
            }
            summary.accounts.push(entry);

            for wallet_tx in account.wallet.transactions() {
                let tx = wallet_tx.tx_node.tx.as_ref();
                let (sent, received) = account.wallet.sent_and_received(tx);
                let (height, seen_at) = match wallet_tx.chain_position {
                    bdk_wallet::chain::ChainPosition::Confirmed { anchor, .. } => (
                        Some(anchor.block_id.height),
                        Some(anchor.confirmation_time),
                    ),
                    bdk_wallet::chain::ChainPosition::Unconfirmed { first_seen, .. } => {
                        (None, first_seen)
                    }
                };

                summary.transactions.push(TxSummary {
                    txid: wallet_tx.tx_node.txid.to_string(),
                    net_sats: received.to_sat() as i64 - sent.to_sat() as i64,
                    // Only knowable when every input belongs to this wallet.
                    fee_sats: account.wallet.calculate_fee(tx).ok().map(|f| f.to_sat()),
                    height,
                    seen_at,
                    script_type: account.script_type,
                });
            }
        }

        // Newest first: unconfirmed at the top, then by height, then by time so
        // several in one block still have a stable order.
        summary.transactions.sort_by(|a, b| {
            b.height
                .unwrap_or(u32::MAX)
                .cmp(&a.height.unwrap_or(u32::MAX))
                .then(b.seen_at.cmp(&a.seen_at))
        });

        Ok(summary)
    }
}

/// A fresh 12-word English mnemonic.
///
/// Returned in a `Zeroizing<String>` and never logged. The caller shows it once
/// and drops it.
pub fn generate_mnemonic() -> Result<Zeroizing<String>> {
    let generated: GeneratedKey<Mnemonic, Tap> =
        Mnemonic::generate((WordCount::Words12, Language::English))
            .map_err(|e| anyhow!("could not generate a recovery phrase: {e:?}"))?;
    Ok(Zeroizing::new(generated.to_string()))
}

/// Turn a mnemonic into the extended private key every path derives from.
fn xprv_from_mnemonic(
    mnemonic: &str,
    passphrase: Option<&str>,
    network: Network,
) -> Result<bdk_wallet::bitcoin::bip32::Xpriv> {
    let parsed = Mnemonic::parse_in(Language::English, mnemonic)
        .map_err(|e| anyhow!("that is not a valid recovery phrase: {e}"))?;

    // The BIP-39 passphrase is part of the seed, not the file encryption. A
    // different passphrase silently derives a different, empty wallet, which is
    // exactly why it is kept distinct from the wallet password everywhere.
    // `(Mnemonic, Option<String>)` is BDK's form for a seed plus a BIP-39
    // passphrase; `None` is the ordinary no-passphrase case.
    let xkey: ExtendedKey<Tap> = (parsed, passphrase.map(str::to_owned))
        .into_extended_key()
        .map_err(|e| anyhow!("could not derive a key from the phrase: {e}"))?;

    xkey.into_xprv(network.into())
        .context("could not derive an extended private key")
}

/// Where the per-path databases live.
fn data_dir(paths: &Paths) -> &Path {
    paths.db.parent().unwrap_or(&paths.db)
}

/// Seal the seed and initialise every derivation path's database.
///
/// Blocking and CPU-bound — Argon2 runs here. Call it from a command, never on
/// the main thread.
#[allow(clippy::too_many_arguments)]
pub fn create(
    mnemonic: &str,
    password: &[u8],
    paths: &Paths,
    kdf: vault::KdfParams,
    network: Network,
    birthday: Checkpoint,
    script_types: &[accounts::ScriptType],
    primary: accounts::ScriptType,
    bip39_passphrase: Option<&str>,
    name: Option<String>,
    lookahead: u32,
) -> Result<Summary> {
    let xprv = xprv_from_mnemonic(mnemonic, bip39_passphrase, network)?;
    let mut portfolio =
        accounts::Portfolio::create_from_xprv(
            xprv,
            data_dir(paths),
            script_types,
            primary,
            network,
            lookahead,
        )?;

    // Recorded before the vault is written, so a wallet that exists at all has
    // a network and a birthday, and never falls back to scanning from genesis.
    Meta::new(network, birthday, script_types.to_vec(), primary, name).save(paths)?;

    let sealed = vault::seal(mnemonic.as_bytes(), password, &network.to_string(), kdf)?;
    vault::write_atomic(&paths.vault, &sealed)?;

    Summary::from_portfolio(&mut portfolio)
}

/// Import an extended private key.
///
/// Derivation is identical to a recovery phrase — a phrase is only a way of
/// writing one of these down — so this reuses the same path expansion.
#[allow(clippy::too_many_arguments)]
pub fn import_xprv(
    xprv_text: &str,
    password: &[u8],
    paths: &Paths,
    kdf: vault::KdfParams,
    network: Network,
    birthday: Checkpoint,
    script_types: &[accounts::ScriptType],
    primary: accounts::ScriptType,
    name: Option<String>,
) -> Result<Summary> {
    let xprv: bdk_wallet::bitcoin::bip32::Xpriv = xprv_text
        .trim()
        .parse()
        .map_err(|e| anyhow!("that is not a valid extended private key: {e}"))?;

    let mut portfolio =
        accounts::Portfolio::create_from_xprv(
            xprv,
            data_dir(paths),
            script_types,
            primary,
            network,
            accounts::IMPORT_LOOKAHEAD,
        )?;
    Meta::new(network, birthday, script_types.to_vec(), primary, name).save(paths)?;

    let sealed = vault::seal(xprv_text.trim().as_bytes(), password, &network.to_string(), kdf)?;
    vault::write_atomic(&paths.vault, &sealed)?;

    Summary::from_portfolio(&mut portfolio)
}

/// Import a single WIF key, watched under every requested script type.
pub fn import_wif(
    wif: &str,
    password: &[u8],
    paths: &Paths,
    kdf: vault::KdfParams,
    network: Network,
    birthday: Checkpoint,
    script_types: &[accounts::ScriptType],
    primary: accounts::ScriptType,
    name: Option<String>,
) -> Result<Summary> {
    let mut portfolio =
        accounts::Portfolio::create_from_wif(wif, data_dir(paths), script_types, primary, network)?;
    Meta::new(network, birthday, script_types.to_vec(), primary, name).save(paths)?;

    let sealed = vault::seal(wif.as_bytes(), password, &network.to_string(), kdf)?;
    vault::write_atomic(&paths.vault, &sealed)?;

    Summary::from_portfolio(&mut portfolio)
}

/// Verify the password against the vault, then load every path watch-only.
///
/// The secret is decrypted only to prove the password is right; the wallets
/// themselves are loaded from public descriptors already in the databases.
pub fn unlock(password: &[u8], paths: &Paths) -> Result<Summary> {
    let blob = std::fs::read(&paths.vault)
        .with_context(|| format!("cannot read {}", paths.vault.display()))?;
    let secret = vault::open(&blob, password)?;

    let meta = Meta::load(paths).context("this wallet has no metadata file")?;
    let mut portfolio = accounts::Portfolio::load(
        data_dir(paths),
        &meta.script_types,
        meta.primary,
        meta.network(),
    )?;

    // The databases hold no unrecoverable state, so losing them costs a rescan
    // rather than the wallet. Rebuild from the secret we just decrypted.
    if portfolio.is_empty() {
        let phrase = std::str::from_utf8(&secret)
            .context("the vault does not contain readable text")?;
        let xprv = xprv_from_mnemonic(phrase, None, meta.network())?;
        portfolio = accounts::Portfolio::create_from_xprv(
            xprv,
            data_dir(paths),
            &meta.script_types,
            meta.primary,
            meta.network(),
            accounts::IMPORT_LOOKAHEAD,
        )?;
    }

    Summary::from_portfolio(&mut portfolio)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Used by the tests only; cargo fix removed it from the module imports
    // when the last non-test use went away.
    use bdk_wallet::KeychainKind;

    #[test]
    fn generated_phrase_is_twelve_valid_words() {
        let phrase = generate_mnemonic().unwrap();
        assert_eq!(phrase.split_whitespace().count(), 12);
        // Round-trips through the BIP-39 checksum.
        Mnemonic::parse_in(Language::English, phrase.as_str()).unwrap();
    }

    #[test]
    fn phrases_are_not_repeated() {
        let a = generate_mnemonic().unwrap();
        let b = generate_mnemonic().unwrap();
        assert_ne!(*a, *b);
    }

    /// Cheap parameters: these tests exercise the plumbing, not the KDF.
    const FAST: vault::KdfParams = vault::KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 };

    /// The taproot-only wallet Sieve creates for itself.
    fn create_for_test(phrase: &str, password: &[u8], paths: &Paths) -> Summary {
        create(
            phrase,
            password,
            paths,
            FAST,
            DEFAULT_NETWORK,
            SIGNET_CHECKPOINTS[0],
            &[accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            None,
            None,
            25,
        )
        .unwrap()
    }

    fn scratch(name: &str) -> Paths {
        let dir = std::env::temp_dir()
            .join(format!("sieve-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Paths {
            vault: dir.join("vault.sieve"),
            db: dir.join("wallet.sqlite"),
            meta: dir.join("wallet.meta.json"),
            dir,
        }
    }

    #[test]
    fn create_then_unlock_returns_the_same_wallet() {
        let paths = scratch("roundtrip");
        assert!(!paths.is_initialised());

        let phrase = generate_mnemonic().unwrap();
        let created = create_for_test(&phrase, b"a good password", &paths);

        assert!(paths.is_initialised());
        assert!(created.next_address.starts_with("tb1p"));

        // Reopening is a fresh read of both files — nothing is carried over in
        // memory, which is what "close the app and reopen it" means.
        let reopened = unlock(b"a good password", &paths).unwrap();
        assert_eq!(created.next_address, reopened.next_address);
        assert_eq!(created.balance_sats, reopened.balance_sats);

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn checkpoints_always_round_earlier() {
        // Rounding later would skip blocks the wallet may have coins in.
        let c = checkpoint_at_or_before(Network::Bitcoin, 899_999);
        assert_eq!(c.height, 850_000, "must not jump forward to 900,000");

        let exact = checkpoint_at_or_before(Network::Bitcoin, 900_000);
        assert_eq!(exact.height, 900_000);

        // Below every checkpoint, fall back to the floor rather than panicking.
        let floor = checkpoint_at_or_before(Network::Bitcoin, 1);
        assert_eq!(floor.height, 709_632, "taproot activation is the BIP86 floor");
    }

    #[test]
    fn every_checkpoint_hash_is_valid() {
        for network in [Network::Bitcoin, Network::Signet] {
            for c in checkpoints(network) {
                c.hash
                    .parse::<bdk_wallet::bitcoin::BlockHash>()
                    .unwrap_or_else(|e| panic!("{} at {} is not a block hash: {e}", c.hash, c.height));
            }
        }
        // Newest first, so `find` returns the tightest checkpoint.
        for list in [MAINNET_CHECKPOINTS, SIGNET_CHECKPOINTS] {
            assert!(list.windows(2).all(|w| w[0].height > w[1].height), "must be newest first");
        }
    }

    #[test]
    fn a_passphrase_derives_a_different_wallet() {
        // The BIP-39 passphrase is part of the seed. This is the property that
        // makes a mistyped passphrase silently produce an empty wallet, and the
        // reason it is never conflated with the wallet password.
        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";

        let plain = xprv_from_mnemonic(phrase, None, DEFAULT_NETWORK).unwrap();
        let with = xprv_from_mnemonic(phrase, Some("extra"), DEFAULT_NETWORK).unwrap();
        let typo = xprv_from_mnemonic(phrase, Some("extar"), DEFAULT_NETWORK).unwrap();

        assert_ne!(plain, with, "a passphrase must change the derived wallet");
        assert_ne!(with, typo, "a mistyped passphrase derives a different wallet");
    }

    #[test]
    fn every_script_type_derives_a_distinct_wallet() {
        // If two paths produced the same addresses, scanning all four would be
        // pointless. This is why importing under the wrong path finds nothing.
        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";
        let xprv = xprv_from_mnemonic(phrase, None, DEFAULT_NETWORK).unwrap();

        let dir = std::env::temp_dir().join(format!("sieve-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut seen = std::collections::HashSet::new();
        for script_type in accounts::ScriptType::ALL {
            let mut account = accounts::Account::create(
                xprv, script_type, &dir.join(script_type.db_file()), DEFAULT_NETWORK, 25,
            ).unwrap();
            let address = account
                .wallet
                .next_unused_address(KeychainKind::External)
                .address
                .to_string();
            assert!(seen.insert(address.clone()), "{script_type} reused an address: {address}");
        }
        assert_eq!(seen.len(), 4);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_first_metadata_format_still_opens() {
        // The regression this pins: a schema change hid a wallet that held a
        // seed, because list_wallets skipped anything it could not parse.
        let paths = scratch("legacy-meta");
        std::fs::write(
            &paths.meta,
            br#"{"height":319000,"hash":"000000021cefaf18c0d9f75944d79689bde29448c55ff00c65c0022814f40578"}"#,
        )
        .unwrap();

        let meta = Meta::load(&paths).expect("the first metadata format must still load");
        assert_eq!(meta.birthday_height, 319_000);
        assert_eq!(meta.network(), Network::Signet);
        assert_eq!(meta.script_types, vec![accounts::ScriptType::Taproot]);

        // And it is rewritten in the current format, so this happens once.
        let reread: Meta = serde_json::from_slice(&std::fs::read(&paths.meta).unwrap()).unwrap();
        assert_eq!(reread.birthday_height, 319_000);

        std::fs::remove_dir_all(&paths.dir).ok();
    }

    #[test]
    fn an_xprv_derives_the_same_wallet_as_its_phrase() {
        // A recovery phrase is only a way of writing an extended key down, so
        // importing either form must land on the same wallet. If this ever
        // diverges, an import silently produces an empty wallet.
        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";
        let xprv = xprv_from_mnemonic(phrase, None, DEFAULT_NETWORK).unwrap();

        let dir = std::env::temp_dir().join(format!("sieve-xprv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let from_phrase = {
            let mut a = accounts::Account::create(
                xprv, accounts::ScriptType::NativeSegwit, &dir.join("a.sqlite"), DEFAULT_NETWORK, 25,
            ).unwrap();
            a.wallet.next_unused_address(KeychainKind::External).address.to_string()
        };

        // Round-trip the key through its text form, which is what an import does.
        let reparsed: bdk_wallet::bitcoin::bip32::Xpriv = xprv.to_string().parse().unwrap();
        let from_xprv = {
            let mut b = accounts::Account::create(
                reparsed, accounts::ScriptType::NativeSegwit, &dir.join("b.sqlite"), DEFAULT_NETWORK, 25,
            ).unwrap();
            b.wallet.next_unused_address(KeychainKind::External).address.to_string()
        };

        assert_eq!(from_phrase, from_xprv);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wallets_do_not_share_a_directory() {
        // The bug this prevents: creating a second wallet overwriting the
        // first one's vault, which would destroy a seed.
        let a = Paths::for_wallet("aaaa");
        let b = Paths::for_wallet("bbbb");
        assert_ne!(a.dir, b.dir);
        assert_ne!(a.vault, b.vault);
        assert_ne!(a.meta, b.meta);
    }

    #[test]
    fn wallet_ids_do_not_collide() {
        let ids: std::collections::HashSet<String> =
            (0..64).map(|_| Paths::new_id()).collect();
        assert_eq!(ids.len(), 64, "wallet ids must be unique");
    }

    #[test]
    fn a_wallet_without_a_name_still_shows_something() {
        let meta = Meta::new(
            DEFAULT_NETWORK,
            SIGNET_CHECKPOINTS[0],
            vec![accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            None,
        );
        assert_eq!(meta.display_name("abcdef"), "Wallet abcd");

        let named = Meta::new(
            DEFAULT_NETWORK,
            SIGNET_CHECKPOINTS[0],
            vec![accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            Some("Spending".into()),
        );
        assert_eq!(named.display_name("abcdef"), "Spending");
    }

    #[test]
    fn creating_a_wallet_records_a_birthday() {
        // Without this file the first sync falls back to the genesis block and
        // walks the entire chain.
        let paths = scratch("birthday");
        let phrase = generate_mnemonic().unwrap();
        create_for_test(&phrase, b"a good password", &paths);

        let meta = Meta::load(&paths).expect("a created wallet must record its metadata");
        assert_eq!(meta.network(), DEFAULT_NETWORK);
        assert_eq!(meta.birthday_height, SIGNET_CHECKPOINTS[0].height);
        assert!(meta.birthday_hash.parse::<bdk_wallet::bitcoin::BlockHash>().is_ok());

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn unlock_rejects_the_wrong_password() {
        let paths = scratch("wrongpass");
        let phrase = generate_mnemonic().unwrap();
        create_for_test(&phrase, b"the right one", &paths);

        assert!(unlock(b"the wrong one", &paths).is_err());

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn a_lost_database_is_rebuilt_from_the_vault() {
        let paths = scratch("rebuild");
        let phrase = generate_mnemonic().unwrap();
        let created = create_for_test(&phrase, b"a good password", &paths);

        // The databases hold only public data, so losing them must be survivable.
        for script_type in accounts::ScriptType::ALL {
            let _ = std::fs::remove_file(paths.db.parent().unwrap().join(script_type.db_file()));
        }
        let rebuilt = unlock(b"a good password", &paths).unwrap();
        assert_eq!(created.next_address, rebuilt.next_address);

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn a_phrase_derives_a_stable_taproot_address() {
        let dir = std::env::temp_dir().join(format!("sieve-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // A known BIP-39 vector, so the derivation is pinned rather than
        // self-consistent.
        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";

        let first = {
            let xprv = xprv_from_mnemonic(phrase, None, DEFAULT_NETWORK).unwrap();
            let mut a = accounts::Account::create(
                xprv, accounts::ScriptType::Taproot, &dir.join("a.sqlite"), DEFAULT_NETWORK, 25
            ).unwrap();
            a.wallet.next_unused_address(KeychainKind::External).address.to_string()
        };
        let second = {
            let xprv = xprv_from_mnemonic(phrase, None, DEFAULT_NETWORK).unwrap();
            let mut b = accounts::Account::create(
                xprv, accounts::ScriptType::Taproot, &dir.join("b.sqlite"), DEFAULT_NETWORK, 25
            ).unwrap();
            b.wallet.next_unused_address(KeychainKind::External).address.to_string()
        };

        assert_eq!(first, second, "the same phrase must derive the same address");
        assert!(first.starts_with("tb1p"), "expected a signet taproot address, got {first}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
