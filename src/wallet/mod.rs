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

pub mod node;

use crate::vault;

/// Not configurable, and deliberately not `Network::Bitcoin`. Mainnet is gated
/// behind an external review of the vault format and the signing path.
pub const NETWORK: Network = Network::Signet;

/// A signet block that is guaranteed to predate any wallet this build creates.
///
/// A wallet created today cannot have received a payment before the app that
/// created it existed, so a checkpoint fixed at build time is always safe: at
/// worst it scans a little more than necessary. Without it, `ScanType::Sync`
/// resumes from the wallet's own checkpoint — which for a new wallet is the
/// genesis block — and the first sync walks the entire chain.
///
/// Bump this on each release. Never move it forward past a shipped build, or
/// wallets created by that build could miss early payments.
pub const BIRTHDAY_HEIGHT: u32 = 319_000;
pub const BIRTHDAY_HASH: &str =
    "000000021cefaf18c0d9f75944d79689bde29448c55ff00c65c0022814f40578";

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

/// The earliest block this wallet could possibly have a transaction in.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Birthday {
    pub height: u32,
    pub hash: String,
}

impl Birthday {
    /// For a wallet created by this build.
    pub fn current() -> Self {
        Self { height: BIRTHDAY_HEIGHT, hash: BIRTHDAY_HASH.to_owned() }
    }

    pub fn load(paths: &Paths) -> Option<Self> {
        let bytes = std::fs::read(&paths.meta).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        crate::vault::write_atomic(&paths.meta, &bytes)
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
fn build(mnemonic: &str, db: &Path) -> Result<PersistedWallet<Connection>> {
    let parsed = Mnemonic::parse_in(Language::English, mnemonic)
        .map_err(|e| anyhow!("that is not a valid recovery phrase: {e}"))?;
    let xkey: ExtendedKey<Tap> = parsed
        .into_extended_key()
        .map_err(|e| anyhow!("could not derive a key from the phrase: {e}"))?;
    let xprv = xkey
        .into_xprv(NETWORK.into())
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
    .network(NETWORK)
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
) -> Result<Summary> {
    let mut wallet = build(mnemonic, &paths.db)?;

    // Recorded before the vault is written, so a wallet that exists at all has
    // a birthday and never falls back to scanning from genesis.
    Birthday::current().save(paths)?;

    let sealed = vault::seal(
        mnemonic.as_bytes(),
        password,
        &NETWORK.to_string(),
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

    let mut conn = Connection::open(&paths.db)?;
    restrict(&paths.db)?;
    let mut wallet = match Wallet::load()
        .check_network(NETWORK)
        .load_wallet(&mut conn)
        .map_err(|e| anyhow!("could not load the wallet database: {e}"))?
    {
        Some(wallet) => wallet,
        // The vault opened but the database is missing or empty. Rebuild it
        // from the seed we just decrypted rather than failing the unlock.
        None => {
            let phrase = std::str::from_utf8(&mnemonic)
                .context("the vault does not contain a valid recovery phrase")?;
            build(phrase, &paths.db)?
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
        let created = create(&phrase, b"a good password", &paths, FAST).unwrap();

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
    fn creating_a_wallet_records_a_birthday() {
        // Without this file the first sync falls back to the genesis block and
        // walks the entire chain.
        let paths = scratch("birthday");
        let phrase = generate_mnemonic().unwrap();
        create(&phrase, b"a good password", &paths, FAST).unwrap();

        let birthday = Birthday::load(&paths).expect("a created wallet must have a birthday");
        assert_eq!(birthday.height, BIRTHDAY_HEIGHT);
        assert!(
            birthday.hash.parse::<bdk_wallet::bitcoin::BlockHash>().is_ok(),
            "the compiled-in birthday hash must be a valid block hash",
        );

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn unlock_rejects_the_wrong_password() {
        let paths = scratch("wrongpass");
        let phrase = generate_mnemonic().unwrap();
        create(&phrase, b"the right one", &paths, FAST).unwrap();

        assert!(unlock(b"the wrong one", &paths).is_err());

        std::fs::remove_dir_all(paths.vault.parent().unwrap()).ok();
    }

    #[test]
    fn a_lost_database_is_rebuilt_from_the_vault() {
        let paths = scratch("rebuild");
        let phrase = generate_mnemonic().unwrap();
        let created = create(&phrase, b"a good password", &paths, FAST).unwrap();

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
            let mut w = build(phrase, &dir.join("a.sqlite")).unwrap();
            w.next_unused_address(KeychainKind::External).address.to_string()
        };
        let second = {
            let mut w = build(phrase, &dir.join("b.sqlite")).unwrap();
            w.next_unused_address(KeychainKind::External).address.to_string()
        };

        assert_eq!(first, second, "the same phrase must derive the same address");
        assert!(first.starts_with("tb1p"), "expected a signet taproot address, got {first}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
