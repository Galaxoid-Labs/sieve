//! Encrypted storage for the wallet seed.
//!
//! Layout on disk:
//!
//! ```text
//! magic "SIEVE\x01" | header_len u16 LE | header JSON | salt 16 |
//! nonce 24 | wrapped DEK 48 | nonce 24 | ciphertext
//! ```
//!
//! The passphrase derives a key-encryption key (Argon2id) which wraps a random
//! data-encryption key; the DEK encrypts the seed. That indirection means a
//! passphrase change rewraps 32 bytes instead of re-encrypting the payload, and
//! leaves room to wrap the same DEK with a second factor later.
//!
//! The header is authenticated as AEAD associated data. Without that, an
//! attacker who can write the file could downgrade the KDF cost and brute-force
//! the passphrase cheaply.

// `seal` and `write_atomic` are exercised by tests and will be called by the
// first-run onboarding flow, which is not built yet.
#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

mod atomic;
#[allow(unused_imports)]
pub use atomic::write_atomic;

const MAGIC: &[u8; 6] = b"SIEVE\x01";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
/// 32-byte key plus the 16-byte Poly1305 tag.
const WRAPPED_LEN: usize = KEY_LEN + 16;

/// Argon2id cost parameters, stored in the header so a file sealed with older
/// settings still opens.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Iterations.
    pub t_cost: u32,
    /// Parallelism.
    pub p_cost: u32,
}

impl Default for KdfParams {
    /// Measured on desktop hardware (see the `kdf_cost` test): 256 MiB and 3
    /// passes costs ~0.7s, where 512 MiB and 4 passes cost 2.1s. Both are far
    /// above the OWASP floor; 0.7s is the most cost we can buy before an unlock
    /// starts feeling broken. Deliberately not tunable from the UI.
    ///
    /// Changing this does not strand existing wallets — the parameters used to
    /// seal a file travel in its header.
    fn default() -> Self {
        Self {
            m_cost: 256 * 1024,
            t_cost: 3,
            p_cost: 4,
        }
    }
}

/// The most memory a vault header may ask Argon2 for, in KiB.
///
/// One gibibyte, four times the default. The parameters that sealed a file
/// travel in its header so that raising the defaults does not strand existing
/// wallets — which means the header decides an allocation, and it is read
/// *before* anything is authenticated. Authentication cannot come first: the
/// header is the associated data, and the key needed to check the tag is what
/// these parameters derive.
///
/// So a file somebody else wrote could ask for terabytes and take the process
/// down with it. The AAD binding already stops the attack that matters —
/// weakening `m_cost` on an existing file to brute-force the password, which
/// changes the AAD and fails the tag — and this closes the one it does not.
const MAX_M_COST: u32 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct Header {
    kdf: KdfParams,
    network: String,
}

impl KdfParams {
    /// Refuse parameters no honest sealer would have written.
    fn sane(&self) -> Result<()> {
        if self.m_cost > MAX_M_COST {
            bail!(
                "this wallet file asks for {} MiB to open, which is more than Sieve will \
                 allocate. The file is damaged or was not written by Sieve.",
                self.m_cost / 1024
            );
        }
        Ok(())
    }
}

fn random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).context("no entropy available from the OS")
}

fn derive_kek(passphrase: &[u8], salt: &[u8], p: KdfParams) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let params = argon2::Params::new(p.m_cost, p.t_cost, p.p_cost, Some(KEY_LEN))
        .map_err(|e| anyhow::anyhow!("invalid Argon2 parameters: {e}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon
        .hash_password_into(passphrase, salt, key.as_mut())
        .map_err(|e| anyhow::anyhow!("key derivation failed: {e}"))?;
    Ok(key)
}

/// Encrypt `secret` under `passphrase`.
pub fn seal(secret: &[u8], passphrase: &[u8], network: &str, kdf: KdfParams) -> Result<Vec<u8>> {
    let header = serde_json::to_vec(&Header {
        kdf,
        network: network.to_owned(),
    })?;
    let header_len = u16::try_from(header.len()).context("header too large")?;

    let mut salt = [0u8; SALT_LEN];
    let mut nonce_kek = [0u8; NONCE_LEN];
    let mut nonce_dek = [0u8; NONCE_LEN];
    let mut dek = Zeroizing::new([0u8; KEY_LEN]);
    random(&mut salt)?;
    random(&mut nonce_kek)?;
    random(&mut nonce_dek)?;
    random(dek.as_mut())?;

    // Everything before the ciphertext is authenticated, so the KDF cost and
    // network cannot be swapped without the tag failing.
    let mut aad = Vec::with_capacity(MAGIC.len() + 2 + header.len());
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(&header_len.to_le_bytes());
    aad.extend_from_slice(&header);

    let kek = derive_kek(passphrase, &salt, kdf)?;
    let wrapped = XChaCha20Poly1305::new((&*kek).into())
        .encrypt(
            &XNonce::from(nonce_kek),
            Payload {
                msg: dek.as_ref(),
                aad: &aad,
            },
        )
        .map_err(|_| anyhow::anyhow!("failed to wrap data key"))?;

    let body = XChaCha20Poly1305::new((&*dek).into())
        .encrypt(
            &XNonce::from(nonce_dek),
            Payload {
                msg: secret,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow::anyhow!("failed to encrypt secret"))?;

    let mut out = aad;
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_kek);
    out.extend_from_slice(&wrapped);
    out.extend_from_slice(&nonce_dek);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Read `n` bytes at `cursor` and advance it.
fn take<'a>(blob: &'a [u8], cursor: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = cursor.checked_add(n).context("vault file is truncated")?;
    let slice = blob.get(*cursor..end).context("vault file is truncated")?;
    *cursor = end;
    Ok(slice)
}

/// Decrypt a blob produced by [`seal`].
///
/// A wrong password and a damaged file are indistinguishable by design — both
/// surface as an authentication failure, so the message names both.
pub fn open(blob: &[u8], passphrase: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut cursor = 0usize;

    if take(blob, &mut cursor, MAGIC.len())? != MAGIC {
        bail!("not a Sieve vault file");
    }
    let header_len = u16::from_le_bytes(take(blob, &mut cursor, 2)?.try_into().unwrap()) as usize;
    let header: Header = serde_json::from_slice(take(blob, &mut cursor, header_len)?)
        .context("vault header is malformed")?;

    // Everything consumed so far is the authenticated header.
    let aad = &blob[..cursor];

    let salt = take(blob, &mut cursor, SALT_LEN)?.to_vec();
    let nonce_kek: [u8; NONCE_LEN] = take(blob, &mut cursor, NONCE_LEN)?.try_into().unwrap();
    let wrapped = take(blob, &mut cursor, WRAPPED_LEN)?.to_vec();
    let nonce_dek: [u8; NONCE_LEN] = take(blob, &mut cursor, NONCE_LEN)?.try_into().unwrap();
    let body = &blob[cursor..];

    header.kdf.sane()?;
    let kek = derive_kek(passphrase, &salt, header.kdf)?;
    // `Zeroizing` rather than a bare `Vec` and a `zeroize()` at the end. The
    // call at the end only runs when everything after it succeeded, so the two
    // failure paths below — a wrapped key of the wrong length, and a body that
    // does not authenticate — were handing the data key back to the allocator
    // with the key still in it. A guard that only fires on success is not a
    // guard.
    let dek = Zeroizing::new(
        XChaCha20Poly1305::new((&*kek).into())
            .decrypt(&XNonce::from(nonce_kek), Payload { msg: &wrapped, aad })
            .map_err(|_| {
                anyhow::anyhow!("Incorrect password, or the wallet file has been damaged.")
            })?,
    );

    let dek_key: &[u8; KEY_LEN] = dek
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("wrapped data key has the wrong length"))?;
    let plaintext = XChaCha20Poly1305::new(dek_key.into())
        .decrypt(&XNonce::from(nonce_dek), Payload { msg: body, aad })
        .map_err(|_| anyhow::anyhow!("vault contents failed authentication"))?;

    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap parameters so the suite stays fast; production uses the defaults.
    const FAST: KdfParams = KdfParams {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };

    /// A vault header cannot ask for unlimited memory.
    ///
    /// The parameters that sealed a file travel in its header, and they are
    /// read before anything can be authenticated — the header *is* the
    /// associated data, and checking the tag needs the key these parameters
    /// derive. So a file somebody else wrote decides an allocation, and
    /// without a ceiling it could ask for terabytes and take the process with
    /// it.
    #[test]
    fn a_header_cannot_ask_for_unlimited_memory() {
        let sealed = seal(b"seed", b"password", "bitcoin", FAST).unwrap();
        // Sanity: the file opens as written.
        assert_eq!(&*open(&sealed, b"password").unwrap(), b"seed");

        // Now rewrite the header to demand every byte Argon2 will take. The
        // AAD binding means it cannot decrypt afterwards — that is the point,
        // and is why the *only* thing this can achieve is exhausting memory.
        let greedy = KdfParams {
            m_cost: u32::MAX,
            t_cost: 1,
            p_cost: 1,
        };
        assert!(greedy.sane().is_err(), "an absurd cost must be refused");
        assert!(FAST.sane().is_ok(), "ordinary parameters must be accepted");
        assert!(
            KdfParams::default().sane().is_ok(),
            "the shipping defaults must be able to open their own files"
        );

        // A real file carrying it is refused before any allocation happens,
        // and the message says the file is wrong rather than the password.
        let mut forged = sealed.clone();
        let header_len =
            u16::from_le_bytes([forged[MAGIC.len()], forged[MAGIC.len() + 1]]) as usize;
        let start = MAGIC.len() + 2;
        let mut header: serde_json::Value =
            serde_json::from_slice(&forged[start..start + header_len]).unwrap();
        header["kdf"]["m_cost"] = serde_json::json!(u32::MAX);
        let rewritten = serde_json::to_vec(&header).unwrap();
        let len = u16::try_from(rewritten.len()).unwrap();
        let mut rebuilt = forged[..MAGIC.len()].to_vec();
        rebuilt.extend_from_slice(&len.to_le_bytes());
        rebuilt.extend_from_slice(&rewritten);
        rebuilt.extend_from_slice(&forged.split_off(start + header_len));

        let error = open(&rebuilt, b"password").unwrap_err().to_string();
        assert!(error.contains("more than Sieve will allocate"), "{error}");
    }

    /// Not run by default — it exists to measure the KDF cost on real hardware
    /// before the parameters are locked into shipped vault files.
    ///
    ///     cargo test --release -- --ignored --nocapture kdf_cost
    #[test]
    #[ignore]
    fn kdf_cost() {
        // Cost is roughly m_cost * t_cost; lanes only buy parallelism.
        let candidates = [
            KdfParams {
                m_cost: 512 * 1024,
                t_cost: 4,
                p_cost: 4,
            },
            KdfParams {
                m_cost: 512 * 1024,
                t_cost: 2,
                p_cost: 4,
            },
            KdfParams {
                m_cost: 256 * 1024,
                t_cost: 3,
                p_cost: 4,
            },
            KdfParams {
                m_cost: 256 * 1024,
                t_cost: 2,
                p_cost: 4,
            },
            KdfParams {
                m_cost: 128 * 1024,
                t_cost: 3,
                p_cost: 4,
            },
            KdfParams {
                m_cost: 64 * 1024,
                t_cost: 3,
                p_cost: 4,
            },
        ];

        for params in candidates {
            let start = std::time::Instant::now();
            let sealed = seal(b"seed", b"passphrase", "signet", params).unwrap();
            let elapsed = start.elapsed();
            open(&sealed, b"passphrase").unwrap();
            println!(
                "m={:>4} MiB t={} p={}  ->  {:.2}s",
                params.m_cost / 1024,
                params.t_cost,
                params.p_cost,
                elapsed.as_secs_f64(),
            );
        }
    }

    #[test]
    fn roundtrip() {
        let sealed = seal(b"correct horse battery staple", b"hunter2", "bitcoin", FAST).unwrap();
        let opened = open(&sealed, b"hunter2").unwrap();
        assert_eq!(opened.as_slice(), b"correct horse battery staple");
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let sealed = seal(b"seed", b"hunter2", "bitcoin", FAST).unwrap();
        assert!(open(&sealed, b"hunter3").is_err());
    }

    #[test]
    fn tampered_header_is_rejected() {
        let mut sealed = seal(b"seed", b"hunter2", "bitcoin", FAST).unwrap();
        // Flip a byte inside the JSON header; the AAD binding must catch it.
        let at = MAGIC.len() + 2 + 4;
        sealed[at] ^= 0x01;
        assert!(open(&sealed, b"hunter2").is_err());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let mut sealed = seal(b"seed", b"hunter2", "bitcoin", FAST).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(open(&sealed, b"hunter2").is_err());
    }

    #[test]
    fn truncated_file_is_rejected() {
        let sealed = seal(b"seed", b"hunter2", "bitcoin", FAST).unwrap();
        assert!(open(&sealed[..sealed.len() / 2], b"hunter2").is_err());
        assert!(open(b"", b"hunter2").is_err());
    }

    #[test]
    fn params_are_read_from_the_header_not_the_default() {
        // A file sealed with old, expensive parameters must still open after
        // the default is retuned. This is what makes the default safe to change.
        let old = KdfParams {
            m_cost: 32,
            t_cost: 7,
            p_cost: 1,
        };
        assert_ne!(old.t_cost, KdfParams::default().t_cost);

        let sealed = seal(b"seed", b"pw", "signet", old).unwrap();
        assert_eq!(open(&sealed, b"pw").unwrap().as_slice(), b"seed");
    }

    #[test]
    fn each_seal_is_unique() {
        // Fresh salt, DEK and nonces every time, so identical input must not
        // produce identical output.
        let a = seal(b"seed", b"pw", "bitcoin", FAST).unwrap();
        let b = seal(b"seed", b"pw", "bitcoin", FAST).unwrap();
        assert_ne!(a, b);
    }
}
