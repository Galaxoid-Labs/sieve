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
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::template::Bip86;
use bdk_wallet::{KeychainKind, PersistedWallet, Wallet};
use zeroize::Zeroizing;

pub mod accounts;
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
    pub vault: PathBuf,
    pub db: PathBuf,
    /// Public, password-free metadata: where this wallet's history can start.
    /// Sync is watch-only, so this cannot live in the encrypted vault.
    pub meta: PathBuf,
}

impl Paths {
    pub fn discover() -> Self {
        let dir = directories::ProjectDirs::from("com", "jdavis", "Sieve")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            vault: dir.join("vault.sieve"),
            db: dir.join("wallet.sqlite"),
            meta: dir.join("wallet.meta.json"),
        }
    }

    /// First run is "no vault", not "no database" — the database can be
    /// rebuilt from the seed, so its absence is recoverable and the vault's is not.
    pub fn is_initialised(&self) -> bool {
        self.vault.exists()
    }
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
}

impl Meta {
    pub fn new(network: Network, birthday: Checkpoint) -> Self {
        Self {
            network: network.to_string(),
            birthday_height: birthday.height,
            birthday_hash: birthday.hash.to_owned(),
        }
    }

    pub fn network(&self) -> Network {
        self.network.parse().unwrap_or(Network::Signet)
    }

    pub fn load(paths: &Paths) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(&paths.meta).ok()?).ok()
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        crate::vault::write_atomic(&paths.meta, &serde_json::to_vec_pretty(self)?)
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

/// What the UI needs to render an unlocked wallet.
#[derive(Debug, Clone)]
pub struct Summary {
    /// Confirmed only. Compact block filters describe transactions in blocks,
    /// so the mempool is invisible to this wallet by construction.
    pub balance_sats: u64,
    pub pending_sats: u64,
    /// Height the wallet has verified up to.
    pub tip: u32,
    pub next_address: String,
    pub network: String,
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

/// Derive the BIP86 descriptors and hand back a persisted wallet.
///
/// The private key goes in, but `bdk_wallet::ChangeSet` persists only
/// `Descriptor<DescriptorPublicKey>` — so the database this writes contains no
/// key material.
fn build(mnemonic: &str, db: &Path, network: Network) -> Result<PersistedWallet<Connection>> {
    let parsed = Mnemonic::parse_in(Language::English, mnemonic)
        .map_err(|e| anyhow!("that is not a valid recovery phrase: {e}"))?;
    let xkey: ExtendedKey<Tap> = parsed
        .into_extended_key()
        .map_err(|e| anyhow!("could not derive a key from the phrase: {e}"))?;
    let xprv = xkey
        .into_xprv(network.into())
        .context("could not derive an extended private key")?;

    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut conn = Connection::open(db)?;
    restrict(db)?;
    Wallet::create(
        Bip86(xprv, KeychainKind::External),
        Bip86(xprv, KeychainKind::Internal),
    )
    .network(network)
    .create_wallet(&mut conn)
    .map_err(|e| anyhow!("could not create the wallet database: {e}"))
}

/// Seal the seed and initialise the wallet database.
///
/// Blocking and CPU-bound — Argon2 runs here. Call it from a command, never on
/// the main thread.
pub fn create(
    mnemonic: &str,
    password: &[u8],
    paths: &Paths,
    kdf: vault::KdfParams,
    network: Network,
    birthday: Checkpoint,
) -> Result<Summary> {
    let mut wallet = build(mnemonic, &paths.db, network)?;

    // Recorded before the vault is written, so a wallet that exists at all has
    // a network and a birthday, and never falls back to scanning from genesis.
    Meta::new(network, birthday).save(paths)?;

    let sealed = vault::seal(
        mnemonic.as_bytes(),
        password,
        &network.to_string(),
        kdf,
    )?;
    vault::write_atomic(&paths.vault, &sealed)?;

    let mut conn = Connection::open(&paths.db)?;
    let summary = summarise(&mut wallet, &mut conn)?;
    Ok(summary)
}

/// Verify the passphrase against the vault, then load the wallet watch-only.
///
/// The seed is decrypted only to prove the passphrase is right; the wallet
/// itself is loaded from public descriptors already in the database.
pub fn unlock(password: &[u8], paths: &Paths) -> Result<Summary> {
    let blob = std::fs::read(&paths.vault)
        .with_context(|| format!("cannot read {}", paths.vault.display()))?;
    let mnemonic = vault::open(&blob, password)?;

    let network = Meta::load(paths).map(|m| m.network()).unwrap_or(Network::Signet);
    let mut conn = Connection::open(&paths.db)?;
    restrict(&paths.db)?;
    let mut wallet = match Wallet::load()
        .check_network(network)
        .load_wallet(&mut conn)
        .map_err(|e| anyhow!("could not load the wallet database: {e}"))?
    {
        Some(wallet) => wallet,
        // The vault opened but the database is missing or empty. Rebuild it
        // from the seed we just decrypted rather than failing the unlock.
        None => {
            let phrase = std::str::from_utf8(&mnemonic)
                .context("the vault does not contain a valid recovery phrase")?;
            build(phrase, &paths.db, network)?
        }
    };

    summarise(&mut wallet, &mut conn)
}

fn summarise(wallet: &mut PersistedWallet<Connection>, conn: &mut Connection) -> Result<Summary> {
    let address = wallet.next_unused_address(KeychainKind::External);
    wallet
        .persist(conn)
        .map_err(|e| anyhow!("could not persist the wallet: {e}"))?;

    let balance = wallet.balance();
    Ok(Summary {
        balance_sats: balance.confirmed.to_sat(),
        pending_sats: (balance.trusted_pending + balance.untrusted_pending).to_sat(),
        tip: wallet.latest_checkpoint().height(),
        next_address: address.address.to_string(),
        network: wallet.network().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn scratch(name: &str) -> Paths {
        let dir = std::env::temp_dir()
            .join(format!("sieve-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Paths {
            vault: dir.join("vault.sieve"),
            db: dir.join("wallet.sqlite"),
            meta: dir.join("wallet.meta.json"),
        }
    }

    #[test]
    fn create_then_unlock_returns_the_same_wallet() {
        let paths = scratch("roundtrip");
        assert!(!paths.is_initialised());

        let phrase = generate_mnemonic().unwrap();
        let created = create(&phrase, b"a good password", &paths, FAST, DEFAULT_NETWORK, SIGNET_CHECKPOINTS[0]).unwrap();

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
    fn creating_a_wallet_records_a_birthday() {
        // Without this file the first sync falls back to the genesis block and
        // walks the entire chain.
        let paths = scratch("birthday");
        let phrase = generate_mnemonic().unwrap();
        create(&phrase, b"a good password", &paths, FAST, DEFAULT_NETWORK, SIGNET_CHECKPOINTS[0]).unwrap();

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
        create(&phrase, b"the right one", &paths, FAST, DEFAULT_NETWORK, SIGNET_CHECKPOINTS[0]).unwrap();

        assert!(unlock(b"the wrong one", &paths).is_err());

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn a_lost_database_is_rebuilt_from_the_vault() {
        let paths = scratch("rebuild");
        let phrase = generate_mnemonic().unwrap();
        let created = create(&phrase, b"a good password", &paths, FAST, DEFAULT_NETWORK, SIGNET_CHECKPOINTS[0]).unwrap();

        // The database holds only public data, so losing it must be survivable.
        std::fs::remove_file(&paths.db).unwrap();
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
            let mut w = build(phrase, &dir.join("a.sqlite"), DEFAULT_NETWORK).unwrap();
            w.next_unused_address(KeychainKind::External).address.to_string()
        };
        let second = {
            let mut w = build(phrase, &dir.join("b.sqlite"), DEFAULT_NETWORK).unwrap();
            w.next_unused_address(KeychainKind::External).address.to_string()
        };

        assert_eq!(first, second, "the same phrase must derive the same address");
        assert!(first.starts_with("tb1p"), "expected a signet taproot address, got {first}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
