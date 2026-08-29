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
use zeroize::{Zeroize, Zeroizing};

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
    /// Desktop-class defaults: 512 MiB and 4 passes. Well above the OWASP
    /// floor, because this gates a seed phrase and a ~0.5s unlock is fine in a
    /// GUI. Deliberately not tunable from the UI.
    fn default() -> Self {
        Self { m_cost: 512 * 1024, t_cost: 4, p_cost: 4 }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Header {
    kdf: KdfParams,
    network: String,
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
    let header = serde_json::to_vec(&Header { kdf, network: network.to_owned() })?;
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
        .encrypt(&XNonce::from(nonce_kek), Payload { msg: dek.as_ref(), aad: &aad })
        .map_err(|_| anyhow::anyhow!("failed to wrap data key"))?;

    let body = XChaCha20Poly1305::new((&*dek).into())
        .encrypt(&XNonce::from(nonce_dek), Payload { msg: secret, aad: &aad })
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
/// A wrong passphrase and a corrupt file are indistinguishable by design — both
/// surface as an authentication failure.
pub fn open(blob: &[u8], passphrase: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut cursor = 0usize;

    if take(blob, &mut cursor, MAGIC.len())? != MAGIC {
        bail!("not a Sieve vault file");
    }
    let header_len =
        u16::from_le_bytes(take(blob, &mut cursor, 2)?.try_into().unwrap()) as usize;
    let header: Header = serde_json::from_slice(take(blob, &mut cursor, header_len)?)
        .context("vault header is malformed")?;

    // Everything consumed so far is the authenticated header.
    let aad = &blob[..cursor];

    let salt = take(blob, &mut cursor, SALT_LEN)?.to_vec();
    let nonce_kek: [u8; NONCE_LEN] = take(blob, &mut cursor, NONCE_LEN)?.try_into().unwrap();
    let wrapped = take(blob, &mut cursor, WRAPPED_LEN)?.to_vec();
    let nonce_dek: [u8; NONCE_LEN] = take(blob, &mut cursor, NONCE_LEN)?.try_into().unwrap();
    let body = &blob[cursor..];

    let kek = derive_kek(passphrase, &salt, header.kdf)?;
    let mut dek = XChaCha20Poly1305::new((&*kek).into())
        .decrypt(&XNonce::from(nonce_kek), Payload { msg: &wrapped, aad })
        .map_err(|_| anyhow::anyhow!("incorrect passphrase, or the vault file was modified"))?;

    let dek_key: &[u8; KEY_LEN] = dek
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("wrapped data key has the wrong length"))?;
    let plaintext = XChaCha20Poly1305::new(dek_key.into())
        .decrypt(&XNonce::from(nonce_dek), Payload { msg: body, aad })
        .map_err(|_| anyhow::anyhow!("vault contents failed authentication"))?;
    dek.zeroize();

    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap parameters so the suite stays fast; production uses the defaults.
    const FAST: KdfParams = KdfParams { m_cost: 8, t_cost: 1, p_cost: 1 };

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
    fn each_seal_is_unique() {
        // Fresh salt, DEK and nonces every time, so identical input must not
        // produce identical output.
        let a = seal(b"seed", b"pw", "bitcoin", FAST).unwrap();
        let b = seal(b"seed", b"pw", "bitcoin", FAST).unwrap();
        assert_ne!(a, b);
    }
}
