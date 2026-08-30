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
pub mod watch;
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
    ) -> Self {
        Self {
            name,
            network: network.to_string(),
            birthday_height: birthday.height,
            birthday_hash: birthday.hash.to_owned(),
            script_types,
            primary,
            scanned_to: None,
            scanned_hash: None,
            watch_only: false,
        }
    }

    /// Record how far a scan has verified, if it is further than before.
    ///
    /// Never backwards: a fresh scan of a wallet that has already been scanned
    /// starts where it left off, and a lower figure would throw that away.
    pub fn record_scanned_to(paths: &Paths, height: u32, hash: &str) {
        let Some(mut meta) = Self::load(paths) else { return };
        if meta.scanned_to.is_some_and(|already| already >= height) {
            return;
        }
        meta.scanned_to = Some(height);
        meta.scanned_hash = Some(hash.to_owned());
        if let Err(e) = meta.save(paths) {
            tracing::debug!(%e, "could not record scan progress");
        }
    }

    /// The same, for a wallet whose keys live somewhere else.
    pub fn watch_only(
        network: Network,
        birthday: Checkpoint,
        script_type: accounts::ScriptType,
        name: Option<String>,
    ) -> Self {
        Self { watch_only: true, ..Self::new(network, birthday, vec![script_type], script_type, name) }
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
            scanned_to: None,
            scanned_hash: None,
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
    pub paid_to_self: Vec<(String, u64)>,
    /// Whether any input signals it may be replaced while unconfirmed.
    pub replaceable: bool,
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
        self.paid_to_self.iter().map(|(_, sats)| sats).sum()
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
        // How many times each address has been paid across the whole wallet.
        // Counted here because it takes the walk we are already doing, and
        // reuse is what ties one payment to another for anyone watching.
        let mut paid: std::collections::HashMap<String, usize> =
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
                for (address, _) in split.ours.iter().chain(split.theirs.iter()) {
                    *paid.entry(address.clone()).or_insert(0usize) += 1;
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
                    // BIP-125: any input below the final sequence number says
                    // this may still be replaced.
                    replaceable: tx.input.iter().any(|i| i.sequence.is_rbf()),
                    reused_address: false,
                    block_hash: block_hash.map(|hash| hash.to_string()),
                });
            }
        }

        // Now that every transaction has been seen, say which of them touched
        // an address that has been paid more than once.
        for tx in &mut summary.transactions {
            tx.reused_address = tx
                .paid_to
                .iter()
                .chain(tx.paid_to_self.iter())
                .any(|(address, _)| paid.get(address).is_some_and(|count| *count > 1));
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
        // The floor is the oldest checkpoint offered, not taproot activation:
        // a wallet imported from a device set up in 2018 has coins that no
        // later starting point can find, and taproot's floor belongs to the
        // taproot *account* rather than to the wallet.
        let floor = checkpoint_at_or_before(Network::Bitcoin, 1);
        assert_eq!(floor.height, 0, "the last resort is the whole chain");

        // And the list stays in order, newest first, or the search above
        // returns the wrong one.
        let heights: Vec<u32> = checkpoints(Network::Bitcoin).iter().map(|c| c.height).collect();
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

/// A transaction's outputs, sorted by who they belong to.
struct Outputs {
    /// Ours: change on a payment, or the payment itself on a receive.
    ours: Vec<(String, u64)>,
    /// Somebody else's: where a payment actually went.
    theirs: Vec<(String, u64)>,
}

/// Split a transaction's outputs into ours and theirs.
///
/// Addresses rather than scripts, because an address is the thing a person can
/// compare against what they were given. A script that is not a standard
/// address — a bare multisig, an `OP_RETURN` — gets said plainly rather than
/// rendered as hex nobody can act on.
fn split_outputs(
    tx: &bdk_wallet::bitcoin::Transaction,
    wallet: &bdk_wallet::PersistedWallet<bdk_wallet::rusqlite::Connection>,
    network: Network,
) -> Outputs {
    let mut ours = Vec::new();
    let mut theirs = Vec::new();

    for out in &tx.output {
        let address = bdk_wallet::bitcoin::Address::from_script(&out.script_pubkey, network)
            .map(|address| address.to_string())
            .unwrap_or_else(|_| {
                if out.script_pubkey.is_op_return() {
                    "Data, not an address (OP_RETURN)".into()
                } else {
                    "An unusual script, not an address".into()
                }
            });

        if wallet.is_mine(out.script_pubkey.clone()) {
            ours.push((address, out.value.to_sat()));
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

    std::fs::remove_dir_all(&dir)
        .with_context(|| format!("could not remove {}", dir.display()))?;
    tracing::info!(wallet = %dir.display(), "removed a wallet");
    Ok(())
}

#[cfg(test)]
mod removal_tests {
    use super::*;

    /// The guard that stands between a wrong path and `remove_dir_all`.
    #[test]
    fn only_wallet_directories_can_be_removed() {
        let outside = std::env::temp_dir().join(format!("sieve-not-a-wallet-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("something-precious"), b"keep me").unwrap();

        let paths = Paths {
            vault: outside.join("vault.sieve"),
            db: outside.join("wallet.sqlite"),
            meta: outside.join("wallet.meta.json"),
            dir: outside.clone(),
        };

        // Outside the wallets directory: refused whatever it contains.
        assert!(remove(&paths).is_err());
        assert!(outside.join("something-precious").exists(), "it deleted it anyway");

        // Inside, but with nothing that makes it a wallet: still refused.
        let empty = wallets_root().join(format!("sieve-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let paths = Paths {
            vault: empty.join("vault.sieve"),
            db: empty.join("wallet.sqlite"),
            meta: empty.join("wallet.meta.json"),
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
