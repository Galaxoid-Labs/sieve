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

use anyhow::{Context, Result, anyhow, bail};
use bdk_wallet::bitcoin::Network;
use bdk_wallet::keys::bip39::{Language, Mnemonic, WordCount};
use bdk_wallet::keys::{DerivableKey, ExtendedKey, GeneratableKey, GeneratedKey};
use bdk_wallet::miniscript::Tap;
use zeroize::Zeroizing;

pub mod accounts;
pub mod labels;
pub mod node;
pub mod send;
pub mod uri;
pub mod watch;

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
    // Older than taproot, for wallets that are. A hardware wallet set up in
    // 2018 has coins no later checkpoint can find, and an import that starts
    // after the money arrived shows an empty wallet — which reads as lost
    // rather than as the wrong starting point.
    //
    // The cost is stated where they are chosen: each of these is another
    // hundred thousand blocks of filters, and filters are the download.
    Checkpoint {
        height: 700_000,
        hash: "0000000000000000000590fc0f3eba193a278534220b2b37e9849e1a770ca959",
        when: "September 2021",
    },
    Checkpoint {
        height: 600_000,
        hash: "00000000000000000007316856900e76b4f7a9139cfbfba89842c8d196cd5f91",
        when: "October 2019",
    },
    Checkpoint {
        height: 500_000,
        hash: "00000000000000000024fb37364cbf81fd49cc2d51c09c75c35433c3a1945d04",
        when: "December 2017",
    },
    Checkpoint {
        height: 400_000,
        hash: "000000000000000004ec466ce4732fe6f1ed1cddc2ed4b328fff5224276e3f6f",
        when: "February 2016",
    },
    // The honest answer to "I do not know": everything. Slow — every filter
    // since 2009 — but a wallet that finds nothing because the search began
    // too late is worse than one that took an afternoon.
    Checkpoint {
        height: 0,
        hash: "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        when: "I don't know — search the whole chain",
    },
];

/// Signet checkpoints, newest first.
pub const SIGNET_CHECKPOINTS: &[Checkpoint] = &[
    Checkpoint {
        height: 319_000,
        hash: "000000021cefaf18c0d9f75944d79689bde29448c55ff00c65c0022814f40578",
        when: "August 2026",
    },
    Checkpoint {
        height: 0,
        hash: "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6",
        when: "I don't know — search the whole chain",
    },
];

/// A block whose height and time are both known, for estimating where the
/// chain has got to since.
///
/// The node cannot answer that during a header sync: it only knows what it has
/// walked so far, and peers are not asked. But blocks arrive every ten minutes
/// on average, so a known block plus the clock puts the tip within a per cent
/// or so — enough to fill a progress bar honestly, which beats a spinner that
/// says nothing for a quarter of an hour.
const TIP_REFERENCE: &[(Network, u32, u64)] = &[
    (Network::Bitcoin, 950_000, 1_779_141_269),
    (Network::Signet, 319_000, 1_787_490_227),
];

/// Roughly where the chain is now.
///
/// An estimate, and labelled as one wherever it is shown. Never lower than the
/// reference block, so it cannot go backwards if the clock is wrong.
pub fn estimated_tip(network: Network) -> Option<u32> {
    let (_, height, time) = TIP_REFERENCE.iter().find(|(n, _, _)| *n == network)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let elapsed = now.saturating_sub(*time);
    Some(height + (elapsed / 600) as u32)
}

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
// Read by the tests that hold its rule — a birthday must always round
// earlier — rather than by the app, which reaches for `birthday_choices`.
#[allow(dead_code)]
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
    /// Present only on a watch-only wallet somebody has chosen to lock.
    ///
    /// A wallet with no keys has no seed to seal, so there is nothing for a
    /// password to decrypt — this holds a known constant instead, and opening
    /// it is what proves the password. See `lock` below for what that does and
    /// does not protect.
    pub lock: PathBuf,
}

/// Root of everything Sieve stores.
pub fn data_root() -> PathBuf {
    directories::ProjectDirs::from("com", "galaxoidlabs", "Sieve")
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
            lock: dir.join("lock.sieve"),
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
    /// Whether this directory holds a wallet.
    ///
    /// A vault *or* metadata: a watch-only wallet has no vault, because it has
    /// no secret to seal.
    pub fn is_initialised(&self) -> bool {
        self.vault.exists() || self.meta.exists()
    }
}

/// A wallet as the chooser needs to show it: enough to pick one, and nothing
/// that requires a password.
#[derive(Debug, Clone)]
pub struct WalletEntry {
    pub id: String,
    pub name: String,
    pub network: String,
    /// Whether opening it asks for a password — true for any wallet holding a
    /// seed, and for a watch-only wallet somebody has chosen to lock.
    pub locked: bool,
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
                locked: is_locked(&paths),
                id,
            }),
            None => {
                tracing::warn!(%id, "wallet metadata is unreadable; listing it anyway");
                found.push(WalletEntry {
                    name: format!("Wallet {}", &id[..id.len().min(4)]),
                    network: "unknown".into(),
                    locked: is_locked(&paths),
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
    /// How many blocks the last completed scan had to fetch and read.
    ///
    /// A filter says a block *probably* touches this wallet; the block itself
    /// is what proves it, and fetching them is the last and slowest phase of a
    /// scan. Nothing in the node says how many are coming — but the same
    /// wallet over the same chain matches the same blocks, its own
    /// transactions plus a false-positive rate that depends only on how many
    /// scripts are being matched. So the last count is a measurement, and the
    /// only honest way to draw a bar for that phase.
    #[serde(default)]
    pub matched_blocks: Option<u32>,
    /// The path used for receiving.
    #[serde(default = "default_primary")]
    pub primary: accounts::ScriptType,
    /// What the person called it. Absent for wallets created before naming.
    #[serde(default)]
    pub name: Option<String>,
    /// How far a scan has checked filters, so an interrupted one resumes
    /// instead of starting again.
    ///
    /// BDK's own checkpoint cannot help here: bdk_kyoto only produces a wallet
    /// update when the *entire* filter sync completes, so a scan killed after
    /// an hour leaves no trace and begins again at the birthday. This is that
    /// trace, and it is deliberately behind where the scan really got to — a
    /// resume point that is too far along skips blocks, and skipped blocks are
    /// missing money.
    #[serde(default)]
    pub scanned_to: Option<u32>,
    /// The hash of the block at `scanned_to`.
    ///
    /// A resume point is a height *and* a hash — the node will not take one
    /// without the other. Stored here, beside the height it belongs to, rather
    /// than looked up in a copy of the chain: one header is what this needs,
    /// and keeping the other nine hundred thousand cost far more than it ever
    /// returned.
    #[serde(default)]
    pub scanned_hash: Option<String>,
    /// Whether a BIP-39 passphrase is part of this wallet's key.
    ///
    /// The passphrase itself is never stored — storing it beside the vault
    /// would undo the only thing it is for. This is the *fact* that one
    /// exists, which signing needs in order to ask for it: without it the
    /// vault decrypts perfectly, derives a different and empty wallet, and the
    /// only sign of trouble is a refusal at the moment of spending.
    #[serde(default)]
    pub bip39_passphrase: bool,
    /// No keys here: the descriptors are public and there is no vault.
    ///
    /// Such a wallet opens without a password — there is nothing to decrypt —
    /// and cannot sign. Signing is the device's job, through a PSBT.
    #[serde(default)]
    pub watch_only: bool,
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
        bip39_passphrase: bool,
    ) -> Self {
        Self {
            name,
            bip39_passphrase,
            network: network.to_string(),
            birthday_height: birthday.height,
            birthday_hash: birthday.hash.to_owned(),
            script_types,
            primary,
            scanned_to: None,
            scanned_hash: None,
            matched_blocks: None,
            watch_only: false,
        }
    }

    /// Record how far a scan has verified, if it is further than before.
    ///
    /// Never backwards: a fresh scan of a wallet that has already been scanned
    /// starts where it left off, and a lower figure would throw that away.
    pub fn record_scanned_to(paths: &Paths, height: u32, hash: &str) {
        let Some(mut meta) = Self::load(paths) else {
            return;
        };
        if meta.scanned_to.is_some_and(|already| already >= height) {
            return;
        }
        meta.scanned_to = Some(height);
        meta.scanned_hash = Some(hash.to_owned());
        if let Err(e) = meta.save(paths) {
            tracing::debug!(%e, "could not record scan progress");
        }
    }

    /// Remember how many blocks the scan just finished had to read.
    pub fn record_matched_blocks(paths: &Paths, blocks: u32) {
        let Some(mut meta) = Self::load(paths) else {
            return;
        };
        if meta.matched_blocks == Some(blocks) {
            return;
        }
        meta.matched_blocks = Some(blocks);
        if let Err(e) = meta.save(paths) {
            tracing::debug!(%e, "could not record the matched block count");
        }
    }

    /// Forget how far a scan got, so the next one starts from the birthday.
    ///
    /// The counterpart to `record_scanned_to`, and the reason a rescan is a
    /// rescan: leaving the resume point in place would have the fresh
    /// databases skip straight back to where the old ones already were.
    pub fn forget_scan_progress(paths: &Paths) -> Result<()> {
        let Some(mut meta) = Self::load(paths) else {
            return Ok(());
        };
        meta.scanned_to = None;
        meta.scanned_hash = None;
        meta.save(paths)
    }

    /// The same, for a wallet whose keys live somewhere else.
    pub fn watch_only(
        network: Network,
        birthday: Checkpoint,
        script_type: accounts::ScriptType,
        name: Option<String>,
    ) -> Self {
        Self {
            watch_only: true,
            ..Self::new(
                network,
                birthday,
                vec![script_type],
                script_type,
                name,
                false,
            )
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
            bip39_passphrase: false,
            network: Network::Signet.to_string(),
            birthday_height: legacy.height,
            birthday_hash: legacy.hash,
            script_types: default_script_types(),
            primary: default_primary(),
            scanned_to: None,
            scanned_hash: None,
            matched_blocks: None,
            // That format predates watch-only wallets, so it can only be one
            // with a vault.
            watch_only: false,
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

/// One spendable coin, as the coin picker needs it.
///
/// A coin is the unit a payment is actually made of, and which ones a payment
/// spends is the single biggest thing a wallet leaks: two coins spent together
/// tell anyone reading the chain that the same person held both. That is the
/// decision this exists to let somebody make.
#[derive(Debug, Clone)]
pub struct CoinSummary {
    pub outpoint: bdk_wallet::bitcoin::OutPoint,
    pub sats: u64,
    pub address: String,
    pub script_type: accounts::ScriptType,
    /// Written out in full, when the descriptor states an origin.
    pub path: Option<String>,
    /// `None` while it is still unconfirmed, which is also what makes it
    /// unspendable.
    pub height: Option<u32>,
    /// The transaction that paid it in. What a label on that payment names.
    pub from_txid: String,
    /// Whether it landed on an address that has been paid more than once.
    pub reused_address: bool,
}

impl CoinSummary {
    pub fn confirmations(&self, tip: u32) -> u32 {
        match self.height {
            Some(height) if tip >= height => tip - height + 1,
            _ => 0,
        }
    }

    /// Spendable once it is in a block. A payment built on an unconfirmed coin
    /// dies with the payment that produced it.
    pub fn spendable(&self) -> bool {
        self.height.is_some()
    }
}

/// One address this wallet has handed out.
#[derive(Debug, Clone)]
pub struct AddressSummary {
    pub address: String,
    pub script_type: accounts::ScriptType,
    /// Its position on the receive chain. What makes one of your addresses
    /// distinguishable from the next.
    pub index: u32,
    /// Written out in full — `m/86'/0'/0'/0/3` — when the descriptor states an
    /// origin to build it from.
    pub path: Option<String>,
    pub received_sats: u64,
    /// How many separate payments landed on it. More than one is address
    /// reuse, which is the thing this list can tell you that nothing else can.
    pub payments: usize,
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
    /// Virtual size, which is what the fee is charged against.
    pub vsize: u64,
    pub inputs: usize,
    pub outputs: usize,
    /// Where the money went, for the outputs that are not this wallet's:
    /// address (or a description, for an unusual script) and amount.
    pub paid_to: Vec<(String, u64)>,
    /// Outputs that came back to this wallet — change on a payment, the
    /// payment itself on a receive.
    pub paid_to_self: Vec<OwnOutput>,
    /// The account this transaction's path derives from, written out:
    /// `m/84'/0'/0'`. Read from the descriptor, so an imported one shows
    /// whatever account it actually names.
    pub account_path: Option<String>,
    /// Whether any input signals it may be replaced while unconfirmed.
    pub replaceable: bool,
    /// What this transaction published, as written and as the bytes actually
    /// are. The detail page is the only place a message ever becomes readable
    /// again after it is sent. A list: this reads transactions other software
    /// made, and more than one data output is standard under Core 30.
    pub data: Vec<(String, String)>,
    /// Payments this one replaced, by transaction id.
    ///
    /// Read back from the transaction graph rather than remembered separately:
    /// a replacement spends the same coins as what it replaces, so the graph
    /// already knows they conflict — and it keeps the loser, which is why this
    /// survives a restart with no bookkeeping of ours.
    pub replaces: Vec<String>,
    /// An address here has been paid more than once across this wallet's
    /// history. Worth saying: reuse is what links payments together for
    /// anybody watching the chain.
    pub reused_address: bool,
    /// The block it landed in, when it has landed.
    pub block_hash: Option<String>,
}

impl TxSummary {
    pub fn is_incoming(&self) -> bool {
        self.net_sats >= 0
    }

    /// What the fee worked out at, per virtual byte.
    pub fn fee_rate(&self) -> Option<f64> {
        let fee = self.fee_sats? as f64;
        (self.vsize > 0).then(|| fee / self.vsize as f64)
    }

    /// Change coming back, on a payment that had some.
    pub fn change_sats(&self) -> u64 {
        if self.is_incoming() {
            return 0;
        }
        self.paid_to_self.iter().map(|out| out.sats).sum()
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
    /// Every address handed out, oldest first within each path.
    pub addresses: Vec<AddressSummary>,
    /// Every unspent coin, largest first within each path.
    pub coins: Vec<CoinSummary>,
}

impl Summary {
    pub(crate) fn from_portfolio(portfolio: &mut accounts::Portfolio) -> Result<Self> {
        let mut summary = Summary {
            ..Default::default()
        };
        let primary = portfolio.primary;
        // How many times each address has been paid across the whole wallet.
        // Counted here because it takes the walk we are already doing, and
        // reuse is what ties one payment to another for anyone watching.
        let mut paid: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        // And how much landed on each, for the address list.
        let mut landed_on: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();

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
                // Every output, sorted into "somebody else's" and "ours".
                // What a payment actually paid, and what came back as change,
                // are the two things a person wants from this screen.
                let split = split_outputs(tx, &account.wallet, account.wallet.network());
                for address in split
                    .ours
                    .iter()
                    .map(|out| &out.address)
                    .chain(split.theirs.iter().map(|(address, _)| address))
                {
                    *paid.entry(address.clone()).or_insert(0usize) += 1;
                }
                for out in &split.ours {
                    *landed_on.entry(out.address.clone()).or_insert(0) += out.sats;
                }

                let (height, seen_at, block_hash) = match wallet_tx.chain_position {
                    bdk_wallet::chain::ChainPosition::Confirmed { anchor, .. } => (
                        Some(anchor.block_id.height),
                        Some(anchor.confirmation_time),
                        Some(anchor.block_id.hash),
                    ),
                    bdk_wallet::chain::ChainPosition::Unconfirmed { first_seen, .. } => {
                        (None, first_seen, None)
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
                    vsize: tx.vsize() as u64,
                    inputs: tx.input.len(),
                    outputs: tx.output.len(),
                    paid_to: split.theirs,
                    paid_to_self: split.ours,
                    account_path: account_path(&account.wallet, bdk_wallet::KeychainKind::External),
                    // BIP-125: any input below the final sequence number says
                    // this may still be replaced.
                    replaceable: tx.input.iter().any(|i| i.sequence.is_rbf()),
                    data: send::data_in(tx)
                        .into_iter()
                        .map(|bytes| {
                            (
                                String::from_utf8_lossy(&bytes).into_owned(),
                                bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                            )
                        })
                        .collect(),
                    replaces: account
                        .wallet
                        .tx_graph()
                        .direct_conflicts(tx)
                        .map(|(_, conflicting)| conflicting.to_string())
                        .collect(),
                    reused_address: false,
                    block_hash: block_hash.map(|hash| hash.to_string()),
                });
            }
        }

        // Every address this wallet has actually handed out — the revealed
        // range of each receive chain. Derived rather than remembered: the
        // keychain's revealed index is the record of what was given out, and
        // deriving from it cannot drift from what the wallet is watching.
        for account in portfolio.accounts.iter_mut() {
            let last = account
                .wallet
                .derivation_index(bdk_wallet::KeychainKind::External);
            let Some(last) = last else { continue };
            let prefix = account_path(&account.wallet, bdk_wallet::KeychainKind::External);

            for index in 0..=last {
                let address = account
                    .wallet
                    .peek_address(bdk_wallet::KeychainKind::External, index)
                    .address
                    .to_string();
                summary.addresses.push(AddressSummary {
                    received_sats: landed_on.get(&address).copied().unwrap_or(0),
                    payments: paid.get(&address).copied().unwrap_or(0),
                    path: prefix.as_ref().map(|prefix| format!("{prefix}/0/{index}")),
                    address,
                    script_type: account.script_type,
                    index,
                });
            }
        }

        // The coins themselves. Listed per path for the same reason a
        // transaction is built from one path: each is its own wallet with its
        // own UTXOs.
        for account in portfolio.accounts.iter_mut() {
            let network = account.wallet.network();
            let mut coins: Vec<CoinSummary> = account
                .wallet
                .list_unspent()
                .map(|utxo| {
                    let address = bdk_wallet::bitcoin::Address::from_script(
                        &utxo.txout.script_pubkey,
                        network,
                    )
                    .map(|address| address.to_string())
                    .unwrap_or_else(|_| "An unusual script, not an address".into());

                    CoinSummary {
                        height: match utxo.chain_position {
                            bdk_wallet::chain::ChainPosition::Confirmed { anchor, .. } => {
                                Some(anchor.block_id.height)
                            }
                            bdk_wallet::chain::ChainPosition::Unconfirmed { .. } => None,
                        },
                        reused_address: paid.get(&address).is_some_and(|count| *count > 1),
                        sats: utxo.txout.value.to_sat(),
                        from_txid: utxo.outpoint.txid.to_string(),
                        path: derivation_of(&account.wallet, &utxo.txout.script_pubkey),
                        outpoint: utxo.outpoint,
                        script_type: account.script_type,
                        address,
                    }
                })
                .collect();

            // Largest first: the coin that covers a payment on its own is the
            // one that leaks least, and it should be the easy answer.
            coins.sort_by(|a, b| b.sats.cmp(&a.sats));
            summary.coins.extend(coins);
        }

        // Now that every transaction has been seen, say which of them touched
        // an address that has been paid more than once.
        for tx in &mut summary.transactions {
            let reused = |address: &String| paid.get(address).is_some_and(|count| *count > 1);
            tx.reused_address = tx.paid_to.iter().map(|(address, _)| address).any(reused)
                || tx.paid_to_self.iter().map(|out| &out.address).any(reused);
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

/// How long a generated recovery phrase is.
///
/// Both are beyond brute force — 128 bits is not a number anybody searches —
/// so this is not a security decision so much as a preference, and some people
/// arrive with one. The cost of the longer phrase is real and falls on the
/// person: twice as many words to copy down correctly, and transcription is
/// the way phrases are actually lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseLength {
    Twelve,
    TwentyFour,
}

impl PhraseLength {
    pub fn words(self) -> usize {
        match self {
            PhraseLength::Twelve => 12,
            PhraseLength::TwentyFour => 24,
        }
    }
}

/// A fresh English mnemonic of the requested length.
///
/// Returned in a `Zeroizing<String>` and never logged. The caller shows it once
/// and drops it.
pub fn generate_mnemonic(length: PhraseLength) -> Result<Zeroizing<String>> {
    let count = match length {
        PhraseLength::Twelve => WordCount::Words12,
        PhraseLength::TwentyFour => WordCount::Words24,
    };
    let generated: GeneratedKey<Mnemonic, Tap> = Mnemonic::generate((count, Language::English))
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
    let mut portfolio = accounts::Portfolio::create_from_xprv(
        xprv,
        data_dir(paths),
        script_types,
        primary,
        network,
        lookahead,
    )?;

    // Recorded before the vault is written, so a wallet that exists at all has
    // a network and a birthday, and never falls back to scanning from genesis.
    // The passphrase is recorded as a fact, never as a value: signing has to
    // know to ask for it, and a wallet that forgot would refuse to spend with
    // no way for anybody to work out why.
    Meta::new(
        network,
        birthday,
        script_types.to_vec(),
        primary,
        name,
        bip39_passphrase.is_some(),
    )
    .save(paths)?;

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

    let mut portfolio = accounts::Portfolio::create_from_xprv(
        xprv,
        data_dir(paths),
        script_types,
        primary,
        network,
        accounts::IMPORT_LOOKAHEAD,
    )?;
    // An extended key is already past the seed, so a BIP-39 passphrase has no
    // meaning here: whatever one was used is baked into the key itself.
    Meta::new(
        network,
        birthday,
        script_types.to_vec(),
        primary,
        name,
        false,
    )
    .save(paths)?;

    let sealed = vault::seal(
        xprv_text.trim().as_bytes(),
        password,
        &network.to_string(),
        kdf,
    )?;
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
    // A WIF is one key with no derivation, so there is no seed for a
    // passphrase to be part of.
    Meta::new(
        network,
        birthday,
        script_types.to_vec(),
        primary,
        name,
        false,
    )
    .save(paths)?;

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
        // Except when a passphrase is part of the key, which is not in the
        // vault and is not being asked for here. Rebuilding without it would
        // succeed — that is the whole problem with a BIP-39 passphrase — and
        // hand back a valid, different, empty wallet. Somebody would read that
        // as their money being gone. Refusing says what actually happened and
        // what recovers it.
        if meta.bip39_passphrase {
            bail!(
                "this wallet's databases are missing and it was set up with a BIP-39 \
                 passphrase, which is part of the key and is not stored here. Restore \
                 from the recovery phrase and that passphrase to rebuild it; the coins \
                 are on the chain either way."
            );
        }
        let phrase =
            std::str::from_utf8(&secret).context("the vault does not contain readable text")?;
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
    /// Renaming the application identifier must not move anybody's wallets.
    ///
    /// On Linux `ProjectDirs` ignores the qualifier and organisation and uses
    /// the lowercased application name alone — checked when the identifier
    /// changed from one organisation to another — so the directory is
    /// `~/.local/share/sieve` regardless. This test is here so that a future
    /// rename that *would* move it fails loudly instead of orphaning a vault.
    #[test]
    fn the_data_directory_does_not_depend_on_the_organisation() {
        let root = super::data_root();
        assert!(
            root.ends_with("sieve"),
            "wallets live in a directory named for the app, not the vendor: {}",
            root.display()
        );
    }

    use super::*;
    // Used by the tests only; cargo fix removed it from the module imports
    // when the last non-test use went away.
    use bdk_wallet::KeychainKind;

    #[test]
    fn generated_phrase_is_twelve_valid_words() {
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        assert_eq!(phrase.split_whitespace().count(), 12);
        // Round-trips through the BIP-39 checksum.
        Mnemonic::parse_in(Language::English, phrase.as_str()).unwrap();
    }

    #[test]
    fn phrases_are_not_repeated() {
        let a = generate_mnemonic(PhraseLength::Twelve).unwrap();
        let b = generate_mnemonic(PhraseLength::Twelve).unwrap();
        assert_ne!(*a, *b);
    }

    /// Cheap parameters: these tests exercise the plumbing, not the KDF.
    const FAST: vault::KdfParams = vault::KdfParams {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };

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
        let dir = std::env::temp_dir().join(format!("sieve-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Paths {
            vault: dir.join("vault.sieve"),
            db: dir.join("wallet.sqlite"),
            meta: dir.join("wallet.meta.json"),
            lock: dir.join("lock.sieve"),
            dir,
        }
    }

    #[test]
    fn create_then_unlock_returns_the_same_wallet() {
        let paths = scratch("roundtrip");
        assert!(!paths.is_initialised());

        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
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
    fn the_address_list_covers_everything_handed_out() {
        let paths = scratch("addresses");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create_for_test(&phrase, b"a good password", &paths);

        let dir = data_dir(&paths).to_path_buf();
        let db = dir.join(accounts::ScriptType::Taproot.db_file());
        let mut account =
            accounts::Account::load(accounts::ScriptType::Taproot, &db, Network::Signet)
                .unwrap()
                .unwrap();
        for _ in 0..5 {
            account
                .wallet
                .reveal_next_address(bdk_wallet::KeychainKind::External);
        }
        account.persist().unwrap();
        drop(account);

        let meta = Meta::load(&paths).unwrap();
        let mut portfolio =
            accounts::Portfolio::load(&dir, &meta.script_types, meta.primary, Network::Signet)
                .unwrap();
        let summary = Summary::from_portfolio(&mut portfolio).unwrap();

        let taproot: Vec<_> = summary
            .addresses
            .iter()
            .filter(|a| a.script_type == accounts::ScriptType::Taproot)
            .collect();

        // Index 0 through the last revealed, and no gaps: an address handed
        // out and missing from this list is an address nobody can find again.
        assert_eq!(taproot.len(), 6, "revealed 0..=5");
        for (position, entry) in taproot.iter().enumerate() {
            assert_eq!(entry.index, position as u32);
            assert!(entry.address.starts_with("tb1p"), "{}", entry.address);
            assert_eq!(entry.payments, 0, "nothing has been paid on a fresh wallet");
        }

        // The path is written out in full, so an address can be checked
        // against a hardware wallet screen.
        assert_eq!(taproot[3].path.as_deref(), Some("m/86'/1'/0'/0/3"));

        // Addresses are unique: a repeat would mean the derivation is wrong.
        let unique: std::collections::HashSet<_> =
            summary.addresses.iter().map(|a| &a.address).collect();
        assert_eq!(unique.len(), summary.addresses.len());

        std::fs::remove_dir_all(&paths.dir).ok();
    }

    #[test]
    fn a_watch_only_wallet_can_be_locked_and_unlocked() {
        // A real public descriptor rather than an invented one: made by
        // creating a wallet and reading back what its database holds,
        // which is exactly the shape a device hands over.
        let source = scratch("watchlock-source");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create_for_test(&phrase, b"a good password", &source);
        let db = data_dir(&source).join(accounts::ScriptType::Taproot.db_file());
        let account = accounts::Account::load(accounts::ScriptType::Taproot, &db, DEFAULT_NETWORK)
            .unwrap()
            .unwrap();
        let descriptor = account
            .wallet
            .public_descriptor(bdk_wallet::KeychainKind::External)
            .to_string();
        drop(account);
        std::fs::remove_dir_all(&source.dir).ok();

        let paths = scratch("watchlock");
        let birthday = SIGNET_CHECKPOINTS[0];
        import_descriptor(&descriptor, &paths, DEFAULT_NETWORK, birthday, None)
            .expect("a public descriptor imports");

        // No password to begin with, which is what makes this worth fixing:
        // the wallet opens and shows everything.
        assert!(!is_locked(&paths), "a watch-only wallet starts unlocked");
        assert!(open_watch_only(&paths).is_ok());

        set_watch_only_password(&paths, b"a good password").unwrap();
        assert!(is_locked(&paths));
        assert!(paths.lock.exists());

        // A wrong password fails on the AEAD tag, exactly as a real vault
        // does, so there is only one way for this to be wrong.
        assert!(open_locked_watch_only(&paths, b"not the password").is_err());
        assert!(open_locked_watch_only(&paths, b"a good password").is_ok());

        // And the wallet itself is untouched by any of it: the lock is a
        // separate file, and taking it off leaves the wallet as it was.
        clear_watch_only_password(&paths).unwrap();
        assert!(!is_locked(&paths));
        assert!(open_watch_only(&paths).is_ok());

        std::fs::remove_dir_all(&paths.dir).ok();
    }

    #[test]
    fn a_wallet_with_a_seed_will_not_take_a_watch_only_lock() {
        // Its password is the vault's, and a second one would be a second
        // thing to get wrong for no gain.
        let paths = scratch("seedlock");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create_for_test(&phrase, b"a good password", &paths);

        let refused = set_watch_only_password(&paths, b"another password").unwrap_err();
        assert!(refused.to_string().contains("holds a key"), "{refused}");
        assert!(!paths.lock.exists());

        std::fs::remove_dir_all(&paths.dir).ok();
    }

    #[test]
    fn a_rescan_keeps_the_wallet_and_forgets_the_chain() {
        let paths = scratch("rescan");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        let before = create_for_test(&phrase, b"a good password", &paths);

        // Hand out a few addresses, as a wallet in use would have.
        let dir = data_dir(&paths).to_path_buf();
        let db = dir.join(accounts::ScriptType::Taproot.db_file());
        let mut account =
            accounts::Account::load(accounts::ScriptType::Taproot, &db, Network::Signet)
                .unwrap()
                .unwrap();
        for _ in 0..8 {
            account
                .wallet
                .reveal_next_address(bdk_wallet::KeychainKind::External);
        }
        let revealed = account
            .wallet
            .derivation_index(bdk_wallet::KeychainKind::External)
            .unwrap();
        account.persist().unwrap();
        drop(account);

        Meta::record_scanned_to(&paths, 205_008, "somehashvalue");
        assert!(Meta::load(&paths).unwrap().scanned_to.is_some());

        rescan(&paths).unwrap();

        // Same wallet: the descriptors are what make it that wallet, and the
        // addresses it has already given out must still be watched.
        let after = accounts::Account::load(accounts::ScriptType::Taproot, &db, Network::Signet)
            .unwrap()
            .unwrap();
        assert_eq!(
            after
                .wallet
                .derivation_index(bdk_wallet::KeychainKind::External),
            Some(revealed),
            "a rescan must not stop watching addresses already handed out"
        );

        // And the scan starts over: no resume point, and nothing verified.
        let meta = Meta::load(&paths).unwrap();
        assert_eq!(meta.scanned_to, None);
        assert_eq!(meta.scanned_hash, None);
        assert_eq!(after.wallet.latest_checkpoint().height(), 0);

        let mut portfolio =
            accounts::Portfolio::load(&dir, &meta.script_types, meta.primary, Network::Signet)
                .unwrap();
        let reopened = Summary::from_portfolio(&mut portfolio).unwrap();
        assert_eq!(reopened.next_address, before.next_address);

        std::fs::remove_dir_all(&paths.dir).ok();
    }

    #[test]
    fn checkpoints_always_round_earlier() {
        // Rounding later would skip blocks the wallet may have coins in.
        let c = checkpoint_at_or_before(Network::Bitcoin, 899_999);
        assert_eq!(c.height, 850_000, "must not jump forward to 900,000");

        let exact = checkpoint_at_or_before(Network::Bitcoin, 900_000);
        assert_eq!(exact.height, 900_000);

        // Below every checkpoint, fall back to the floor rather than panicking.
        // The floor is the oldest checkpoint offered, not taproot activation:
        // a wallet imported from a device set up in 2018 has coins that no
        // later starting point can find, and taproot's floor belongs to the
        // taproot *account* rather than to the wallet.
        let floor = checkpoint_at_or_before(Network::Bitcoin, 1);
        assert_eq!(floor.height, 0, "the last resort is the whole chain");

        // And the list stays in order, newest first, or the search above
        // returns the wrong one.
        let heights: Vec<u32> = checkpoints(Network::Bitcoin)
            .iter()
            .map(|c| c.height)
            .collect();
        let mut sorted = heights.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(heights, sorted, "checkpoints must be newest first");
    }

    #[test]
    fn every_checkpoint_hash_is_valid() {
        for network in [Network::Bitcoin, Network::Signet] {
            for c in checkpoints(network) {
                c.hash
                    .parse::<bdk_wallet::bitcoin::BlockHash>()
                    .unwrap_or_else(|e| {
                        panic!("{} at {} is not a block hash: {e}", c.hash, c.height)
                    });
            }
        }
        // Newest first, so `find` returns the tightest checkpoint.
        for list in [MAINNET_CHECKPOINTS, SIGNET_CHECKPOINTS] {
            assert!(
                list.windows(2).all(|w| w[0].height > w[1].height),
                "must be newest first"
            );
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
        assert_ne!(
            with, typo,
            "a mistyped passphrase derives a different wallet"
        );
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
                xprv,
                script_type,
                &dir.join(script_type.db_file()),
                DEFAULT_NETWORK,
                25,
            )
            .unwrap();
            let address = account
                .wallet
                .next_unused_address(KeychainKind::External)
                .address
                .to_string();
            assert!(
                seen.insert(address.clone()),
                "{script_type} reused an address: {address}"
            );
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
                xprv,
                accounts::ScriptType::NativeSegwit,
                &dir.join("a.sqlite"),
                DEFAULT_NETWORK,
                25,
            )
            .unwrap();
            a.wallet
                .next_unused_address(KeychainKind::External)
                .address
                .to_string()
        };

        // Round-trip the key through its text form, which is what an import does.
        let reparsed: bdk_wallet::bitcoin::bip32::Xpriv = xprv.to_string().parse().unwrap();
        let from_xprv = {
            let mut b = accounts::Account::create(
                reparsed,
                accounts::ScriptType::NativeSegwit,
                &dir.join("b.sqlite"),
                DEFAULT_NETWORK,
                25,
            )
            .unwrap();
            b.wallet
                .next_unused_address(KeychainKind::External)
                .address
                .to_string()
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
        let ids: std::collections::HashSet<String> = (0..64).map(|_| Paths::new_id()).collect();
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
            false,
        );
        assert_eq!(meta.display_name("abcdef"), "Wallet abcd");

        let named = Meta::new(
            DEFAULT_NETWORK,
            SIGNET_CHECKPOINTS[0],
            vec![accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            Some("Spending".into()),
            false,
        );
        assert_eq!(named.display_name("abcdef"), "Spending");
    }

    #[test]
    fn a_twenty_four_word_phrase_is_asked_for_and_returned() {
        let short = generate_mnemonic(PhraseLength::Twelve).unwrap();
        assert_eq!(short.split_whitespace().count(), 12);
        Mnemonic::parse_in(Language::English, short.as_str()).unwrap();

        let long = generate_mnemonic(PhraseLength::TwentyFour).unwrap();
        assert_eq!(long.split_whitespace().count(), 24);
        Mnemonic::parse_in(Language::English, long.as_str()).unwrap();
    }

    #[test]
    fn a_passphrase_is_recorded_as_a_fact_and_never_as_a_value() {
        // Signing has to know to ask. Without this the vault opens, the wrong
        // key is derived, and the only symptom is a refusal to spend.
        let paths = scratch("passphrase-meta");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create(
            &phrase,
            b"a good password",
            &paths,
            FAST,
            DEFAULT_NETWORK,
            SIGNET_CHECKPOINTS[0],
            &[accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            Some("correct horse"),
            None,
            25,
        )
        .unwrap();

        let meta = Meta::load(&paths).unwrap();
        assert!(meta.bip39_passphrase, "the fact must be recorded");

        // And the passphrase itself must not be anywhere on disk.
        let written = std::fs::read(&paths.meta).unwrap();
        assert!(
            !String::from_utf8_lossy(&written).contains("correct horse"),
            "the passphrase itself must never be written down"
        );

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn a_wallet_made_without_a_passphrase_says_so() {
        let paths = scratch("no-passphrase-meta");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create_for_test(&phrase, b"a good password", &paths);
        assert!(!Meta::load(&paths).unwrap().bip39_passphrase);
        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn a_metadata_file_written_before_passphrases_still_loads() {
        // Every wallet on disk predates this field. One that cannot be read is
        // a wallet somebody cannot open.
        let older = r#"{
            "network": "signet",
            "birthday_height": 1,
            "birthday_hash": "00000008819873e925422c1ff0f99f7cc9bbb232af63a077a480a3633bee1ef6",
            "script_types": ["Taproot"],
            "primary": "Taproot"
        }"#;
        let meta: Meta = serde_json::from_str(older).unwrap();
        assert!(!meta.bip39_passphrase);
    }

    #[test]
    fn losing_the_databases_refuses_rather_than_rebuilding_an_empty_wallet() {
        // The passphrase is not in the vault, so a rebuild would derive a
        // different wallet — successfully, with a zero balance. Somebody would
        // read that as their money being gone.
        let paths = scratch("passphrase-rebuild");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create(
            &phrase,
            b"a good password",
            &paths,
            FAST,
            DEFAULT_NETWORK,
            SIGNET_CHECKPOINTS[0],
            &[accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            Some("correct horse"),
            None,
            25,
        )
        .unwrap();

        std::fs::remove_dir_all(data_dir(&paths)).unwrap();
        let error = unlock(b"a good password", &paths)
            .expect_err("a rebuild without the passphrase must not be attempted");
        let said = error.to_string();
        assert!(said.contains("passphrase"), "{said}");

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn signing_a_passphrase_wallet_needs_the_passphrase_and_is_refused_without_it() {
        // The whole reason the flag above exists. A missing BIP-39 passphrase
        // is not an error anywhere in the key derivation — it produces a
        // valid, different, empty wallet — so the only thing standing between
        // somebody and a signature that finalizes nothing is this check.
        const PASSPHRASE: &str = "correct horse";
        let paths = scratch("passphrase-signing");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create(
            &phrase,
            b"a good password",
            &paths,
            FAST,
            DEFAULT_NETWORK,
            SIGNET_CHECKPOINTS[0],
            &[accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            Some(PASSPHRASE),
            None,
            25,
        )
        .unwrap();

        // What the wallet on disk is actually watching.
        use bdk_wallet::chain::DescriptorExt;
        let portfolio = accounts::Portfolio::load(
            data_dir(&paths),
            &[accounts::ScriptType::Taproot],
            accounts::ScriptType::Taproot,
            DEFAULT_NETWORK,
        )
        .unwrap();
        let watching = portfolio.accounts[0]
            .wallet
            .public_descriptor(bdk_wallet::KeychainKind::External)
            .descriptor_id();

        let with = send::signer(
            &phrase,
            accounts::ScriptType::Taproot,
            DEFAULT_NETWORK,
            Some(PASSPHRASE),
        )
        .unwrap();
        send::check_signer(&with, watching).expect("the passphrase must derive these addresses");

        let without = send::signer(
            &phrase,
            accounts::ScriptType::Taproot,
            DEFAULT_NETWORK,
            None,
        )
        .unwrap();
        send::check_signer(&without, watching)
            .expect_err("signing without the passphrase must be refused, not attempted");

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn creating_a_wallet_records_a_birthday() {
        // Without this file the first sync falls back to the genesis block and
        // walks the entire chain.
        let paths = scratch("birthday");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create_for_test(&phrase, b"a good password", &paths);

        let meta = Meta::load(&paths).expect("a created wallet must record its metadata");
        assert_eq!(meta.network(), DEFAULT_NETWORK);
        assert_eq!(meta.birthday_height, SIGNET_CHECKPOINTS[0].height);
        assert!(
            meta.birthday_hash
                .parse::<bdk_wallet::bitcoin::BlockHash>()
                .is_ok()
        );

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn unlock_rejects_the_wrong_password() {
        let paths = scratch("wrongpass");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
        create_for_test(&phrase, b"the right one", &paths);

        assert!(unlock(b"the wrong one", &paths).is_err());

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn a_lost_database_is_rebuilt_from_the_vault() {
        let paths = scratch("rebuild");
        let phrase = generate_mnemonic(PhraseLength::Twelve).unwrap();
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
                xprv,
                accounts::ScriptType::Taproot,
                &dir.join("a.sqlite"),
                DEFAULT_NETWORK,
                25,
            )
            .unwrap();
            a.wallet
                .next_unused_address(KeychainKind::External)
                .address
                .to_string()
        };
        let second = {
            let xprv = xprv_from_mnemonic(phrase, None, DEFAULT_NETWORK).unwrap();
            let mut b = accounts::Account::create(
                xprv,
                accounts::ScriptType::Taproot,
                &dir.join("b.sqlite"),
                DEFAULT_NETWORK,
                25,
            )
            .unwrap();
            b.wallet
                .next_unused_address(KeychainKind::External)
                .address
                .to_string()
        };

        assert_eq!(
            first, second,
            "the same phrase must derive the same address"
        );
        assert!(
            first.starts_with("tb1p"),
            "expected a signet taproot address, got {first}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// A transaction's outputs, sorted by who they belong to.
struct Outputs {
    /// Ours: change on a payment, or the payment itself on a receive.
    ours: Vec<OwnOutput>,
    /// Somebody else's: where a payment actually went.
    theirs: Vec<(String, u64)>,
}

/// Split a transaction's outputs into ours and theirs.
///
/// Addresses rather than scripts, because an address is the thing a person can
/// compare against what they were given. A script that is not a standard
/// address — a bare multisig, an `OP_RETURN` — gets said plainly rather than
/// rendered as hex nobody can act on.
/// One output of a transaction that belongs to this wallet.
#[derive(Debug, Clone)]
pub struct OwnOutput {
    pub address: String,
    pub sats: u64,
    /// Which key paid it, written out in full: `m/84'/0'/0'/1/7`. `None` for an
    /// address the wallet recognises but has not handed out itself.
    pub path: Option<String>,
}

/// The full derivation path behind one of this wallet's own outputs.
///
/// The account part comes from the descriptor's own origin rather than being
/// assumed, so an imported descriptor shows the account it actually names
/// instead of the one Sieve would have chosen.
fn derivation_of(
    wallet: &bdk_wallet::PersistedWallet<bdk_wallet::rusqlite::Connection>,
    spk: &bdk_wallet::bitcoin::ScriptBuf,
) -> Option<String> {
    let (keychain, index) = wallet.derivation_of_spk(spk.clone())?;
    let change = match keychain {
        bdk_wallet::KeychainKind::External => 0,
        bdk_wallet::KeychainKind::Internal => 1,
    };
    match account_path(wallet, keychain) {
        Some(account) => Some(format!("{account}/{change}/{index}")),
        // Without an origin there is nothing honest to put in front of it.
        None => Some(format!("{change}/{index}")),
    }
}

/// The account prefix of a keychain's descriptor, as `m/84'/0'/0'`.
///
/// Taken out of the key origin the descriptor carries. A descriptor with no
/// origin — a bare xpub — has no account to name, and gets `None` rather than
/// a guess.
fn account_path(
    wallet: &bdk_wallet::PersistedWallet<bdk_wallet::rusqlite::Connection>,
    keychain: bdk_wallet::KeychainKind,
) -> Option<String> {
    let descriptor = wallet.public_descriptor(keychain).to_string();
    let origin = descriptor.split_once('[')?.1.split_once(']')?.0;
    // `[fingerprint/84h/0h/0h]` — the fingerprint identifies the seed, not the
    // path, so it is dropped.
    let path = origin.split_once('/')?.1;
    if path.is_empty() {
        return None;
    }
    Some(format!("m/{}", path.replace('h', "'")))
}

fn split_outputs(
    tx: &bdk_wallet::bitcoin::Transaction,
    wallet: &bdk_wallet::PersistedWallet<bdk_wallet::rusqlite::Connection>,
    network: Network,
) -> Outputs {
    let mut ours = Vec::new();
    let mut theirs = Vec::new();

    for out in &tx.output {
        // A data output is not somebody being paid. It has its own row on the
        // detail page, carrying what it actually says; listing it here as well
        // both repeats it and calls it a recipient, which it is not.
        if out.script_pubkey.is_op_return() {
            continue;
        }
        let address = bdk_wallet::bitcoin::Address::from_script(&out.script_pubkey, network)
            .map(|address| address.to_string())
            .unwrap_or_else(|_| "An unusual script, not an address".into());

        if wallet.is_mine(out.script_pubkey.clone()) {
            ours.push(OwnOutput {
                address,
                sats: out.value.to_sat(),
                path: derivation_of(wallet, &out.script_pubkey),
            });
        } else {
            theirs.push((address, out.value.to_sat()));
        }
    }

    Outputs { ours, theirs }
}

/// Import a watch-only wallet from a descriptor or an extended public key.
///
/// No password, because there is nothing to protect: the descriptors are
/// public, and no vault is written. What this wallet cannot do is sign — that
/// belongs to whatever holds the key, over a PSBT.
pub fn import_descriptor(
    text: &str,
    paths: &Paths,
    network: Network,
    birthday: Checkpoint,
    name: Option<String>,
) -> Result<Summary> {
    import_descriptors(&[text.to_string()], paths, network, birthday, name)
}

/// The same, for a wallet described by several descriptors at once.
///
/// A hardware wallet holds a legacy, a nested, a native segwit and a taproot
/// account from one seed, exactly as a recovery phrase does — so a device
/// import watches all of them, for the same reason a seed import does. Which
/// path has the coins is not something the person importing should have to
/// know.
pub fn import_descriptors(
    texts: &[String],
    paths: &Paths,
    network: Network,
    birthday: Checkpoint,
    name: Option<String>,
) -> Result<Summary> {
    if texts.is_empty() {
        anyhow::bail!("there is nothing to import");
    }

    let dir = data_dir(paths);
    std::fs::create_dir_all(dir)?;

    let mut accounts = Vec::new();
    let mut script_types = Vec::new();
    for text in texts {
        let descriptors = watch::parse(text)?;
        let db = dir.join(descriptors.script_type.db_file());
        let mut account = accounts::Account::create_watching(
            &descriptors.external,
            &descriptors.internal,
            descriptors.script_type,
            &db,
            network,
        )?;
        account.persist()?;
        script_types.push(descriptors.script_type);
        accounts.push(account);
    }

    // Receiving on native segwit where the wallet has it: the most widely
    // accepted address kind, and what the other import paths choose.
    let primary = script_types
        .iter()
        .copied()
        .find(|s| *s == accounts::ScriptType::NativeSegwit)
        .unwrap_or(script_types[0]);

    let mut meta = Meta::watch_only(network, birthday, primary, name);
    meta.script_types = script_types;
    meta.save(paths)?;

    let mut portfolio = accounts::Portfolio { accounts, primary };
    Summary::from_portfolio(&mut portfolio)
}

/// Open a wallet that has no vault.
///
/// The counterpart to `unlock` for watch-only wallets: there is no password to
/// check and nothing to decrypt, so this only loads what is already public.
pub fn open_watch_only(paths: &Paths) -> Result<Summary> {
    let meta = Meta::load(paths).context("this wallet has no metadata file")?;
    let mut portfolio = accounts::Portfolio::load(
        data_dir(paths),
        &meta.script_types,
        meta.primary,
        meta.network(),
    )?;
    if portfolio.is_empty() {
        anyhow::bail!("this wallet's databases are missing, and it has no key to rebuild them");
    }
    Summary::from_portfolio(&mut portfolio)
}

/// What a watch-only lock seals. The value is irrelevant; being able to
/// decrypt it is the whole message.
const LOCK_TOKEN: &[u8] = b"sieve watch-only lock";

/// Whether this wallet asks for a password before it will open.
///
/// True for any wallet with a vault — the seed is in it — and for a watch-only
/// wallet that has been given a lock.
pub fn is_locked(paths: &Paths) -> bool {
    paths.vault.exists() || paths.lock.exists()
}

/// Give a watch-only wallet a password, or change the one it has.
///
/// **This locks the wallet inside Sieve. It does not encrypt anything on
/// disk.** A watch-only wallet's descriptors and history live in SQLite files
/// that BDK reads directly, and encrypting them would mean encrypting the
/// database the node writes to on every block. What this protects is somebody
/// opening Sieve at your machine and reading your balance; what it does not
/// protect is somebody who has the files. Every screen that offers it has to
/// say so, or it promises more than it delivers.
pub fn set_watch_only_password(paths: &Paths, password: &[u8]) -> Result<()> {
    let meta = Meta::load(paths).context("this wallet has no metadata file")?;
    if !meta.watch_only {
        anyhow::bail!("this wallet holds a key, and its password is the vault's");
    }
    let sealed = vault::seal(
        LOCK_TOKEN,
        password,
        &meta.network,
        vault::KdfParams::default(),
    )?;
    vault::write_atomic(&paths.lock, &sealed)?;
    restrict(&paths.lock)
}

/// Take the lock off, so the wallet opens without asking.
pub fn clear_watch_only_password(paths: &Paths) -> Result<()> {
    if !paths.lock.exists() {
        return Ok(());
    }
    std::fs::remove_file(&paths.lock)
        .with_context(|| format!("could not unlock {}", paths.lock.display()))
}

/// Open a watch-only wallet that has a lock on it.
///
/// The password is checked by decrypting the token: a wrong one fails the
/// AEAD tag, exactly as a wrong password fails a real vault, so there is no
/// second way for this to be wrong.
pub fn open_locked_watch_only(paths: &Paths, password: &[u8]) -> Result<Summary> {
    let blob = std::fs::read(&paths.lock)
        .with_context(|| format!("cannot read {}", paths.lock.display()))?;
    let opened = vault::open(&blob, password)?;
    if opened.as_slice() != LOCK_TOKEN {
        anyhow::bail!("that is not this wallet's password");
    }
    open_watch_only(paths)
}

/// Delete a wallet from this computer.
///
/// What goes: the encrypted vault, the watch-only databases, the metadata —
/// the whole directory. What does not: the coins. They are on the chain, and
/// the recovery phrase is what reaches them. That distinction is the entire
/// content of the warning this needs in front of it, because the file being
/// deleted is, for a wallet nobody wrote down, the only way back.
///
/// Refuses anything that is not a wallet directory of ours. A bug that passed
/// the wrong path would otherwise delete an arbitrary tree, and this is the
/// last place to catch it — `remove_dir_all` asks no questions.
/// Throw away everything this wallet has learned from the chain and set it up
/// to scan again from its birthday.
///
/// The databases hold public descriptors, transactions and checkpoints — no key
/// material, which is what makes this safe to do: the descriptors are read out,
/// the files are replaced, and the same descriptors go back in. A wallet with
/// no checkpoint scans with `ScanType::Recovery` from the birthday, so simply
/// having no chain data is what starts the rescan.
///
/// What is genuinely lost is local knowledge no peer will give back: a
/// broadcast transaction that has not been mined yet. Everything else is
/// re-derived from blocks.
///
/// The caller must have shut the node down first — these files are open while
/// a session is running.
pub fn rescan(paths: &Paths) -> Result<()> {
    let meta = Meta::load(paths).context("this wallet has no metadata file")?;
    let network = meta.network();
    let dir = data_dir(paths).to_path_buf();

    for script_type in &meta.script_types {
        let db = dir.join(script_type.db_file());
        let Some(account) = accounts::Account::load(*script_type, &db, network)? else {
            continue;
        };

        let external = account
            .wallet
            .public_descriptor(bdk_wallet::KeychainKind::External)
            .to_string();
        let internal = account
            .wallet
            .public_descriptor(bdk_wallet::KeychainKind::Internal)
            .to_string();
        // How far this wallet had handed out addresses. A fresh database
        // starts at nothing, and a recovery scan only checks the scripts it
        // knows about — so without this, a rescan would stop looking at
        // exactly the addresses this wallet has already given people.
        let revealed = [
            bdk_wallet::KeychainKind::External,
            bdk_wallet::KeychainKind::Internal,
        ]
        .map(|keychain| account.wallet.derivation_index(keychain));
        drop(account);

        // sqlite keeps its journal beside the database; leaving those would
        // have the new file inherit the old one's tail.
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let file = db.with_file_name(format!("{}{suffix}", script_type.db_file()));
            if file.exists() {
                std::fs::remove_file(&file)
                    .with_context(|| format!("could not clear {}", file.display()))?;
            }
        }

        // A wallet imported from a bare key has one descriptor and no change
        // chain; BDK refuses a pair that is the same descriptor twice.
        let mut fresh = if external == internal {
            accounts::Account::create_single(&external, *script_type, &db, network)?
        } else {
            accounts::Account::create_watching(&external, &internal, *script_type, &db, network)?
        };

        for (keychain, index) in [
            bdk_wallet::KeychainKind::External,
            bdk_wallet::KeychainKind::Internal,
        ]
        .into_iter()
        .zip(revealed)
        {
            if let Some(index) = index {
                let _ = fresh.wallet.reveal_addresses_to(keychain, index).count();
            }
        }
        fresh.persist()?;

        tracing::info!(
            path = %script_type,
            revealed = ?revealed,
            "cleared chain data for a rescan"
        );
    }

    Meta::forget_scan_progress(paths)?;
    Ok(())
}

pub fn remove(paths: &Paths) -> Result<()> {
    let root = wallets_root();
    let canonical_root = root.canonicalize().unwrap_or(root);
    let dir = paths
        .dir
        .canonicalize()
        .with_context(|| format!("{} is not there", paths.dir.display()))?;

    if !dir.starts_with(&canonical_root) || dir == canonical_root {
        anyhow::bail!("{} is not a wallet directory", dir.display());
    }
    // A wallet has a vault or metadata in it. A directory with neither is
    // something else, and not ours to delete.
    if !paths.vault.exists() && !paths.meta.exists() {
        anyhow::bail!("{} does not look like a wallet", dir.display());
    }

    std::fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))?;
    tracing::info!(wallet = %dir.display(), "removed a wallet");
    Ok(())
}

#[cfg(test)]
mod removal_tests {
    use super::*;

    /// The guard that stands between a wrong path and `remove_dir_all`.
    #[test]
    fn only_wallet_directories_can_be_removed() {
        let outside =
            std::env::temp_dir().join(format!("sieve-not-a-wallet-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("something-precious"), b"keep me").unwrap();

        let paths = Paths {
            vault: outside.join("vault.sieve"),
            db: outside.join("wallet.sqlite"),
            meta: outside.join("wallet.meta.json"),
            lock: outside.join("lock.sieve"),
            dir: outside.clone(),
        };

        // Outside the wallets directory: refused whatever it contains.
        assert!(remove(&paths).is_err());
        assert!(
            outside.join("something-precious").exists(),
            "it deleted it anyway"
        );

        // Inside, but with nothing that makes it a wallet: still refused.
        let empty = wallets_root().join(format!("sieve-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let paths = Paths {
            vault: empty.join("vault.sieve"),
            db: empty.join("wallet.sqlite"),
            meta: empty.join("wallet.meta.json"),
            lock: empty.join("lock.sieve"),
            dir: empty.clone(),
        };
        assert!(remove(&paths).is_err());
        assert!(empty.exists());

        // And with a vault in it, it goes.
        std::fs::write(&paths.vault, b"sealed").unwrap();
        remove(&paths).unwrap();
        assert!(!empty.exists());

        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A wallet that has already gone is an error, not a panic and not a
    /// silent success that leaves the interface claiming something happened.
    #[test]
    fn removing_what_is_not_there_is_an_error() {
        let paths = Paths::for_wallet("no-such-wallet-at-all");
        assert!(remove(&paths).is_err());
    }
}
