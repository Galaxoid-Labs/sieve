//! Onion addresses, written down and read back.
//!
//! A v3 onion address is a 32-byte ed25519 public key, a two-byte checksum and
//! a version byte, base32-encoded — 56 characters and `.onion`. kyoto hands
//! peers over as the raw key and dials them the same way, so this is only
//! needed at the two edges: showing an address to a person, and remembering
//! one between runs.
//!
//! The checksum is not decoration. Without checking it, a typo or a corrupted
//! file becomes a peer address that cannot exist, and the node spends its
//! connection attempts on nothing.

use sha3::{Digest, Sha3_256};

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const SALT: &[u8] = b".onion checksum";
const VERSION: u8 = 3;
/// 32 bytes of key, 2 of checksum, 1 of version, base32-encoded.
const ENCODED_LEN: usize = 56;

/// The `.onion` hostname for a public key.
pub fn encode(pubkey: &[u8; 32]) -> String {
    let mut buffer = [0u8; 35];
    buffer[..32].copy_from_slice(pubkey);
    let checksum = checksum(pubkey);
    buffer[32] = checksum[0];
    buffer[33] = checksum[1];
    buffer[34] = VERSION;

    let mut address = base32_encode(&buffer);
    address.push_str(".onion");
    address
}

/// The public key inside a `.onion` hostname, if it is one and it checks out.
pub fn decode(address: &str) -> Option<[u8; 32]> {
    let address = address.trim().to_ascii_lowercase();
    let body = address.strip_suffix(".onion")?;
    if body.len() != ENCODED_LEN {
        return None;
    }

    let decoded = base32_decode(body)?;
    if decoded.len() != 35 || decoded[34] != VERSION {
        return None;
    }

    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&decoded[..32]);

    // The two bytes that say this address was not invented.
    let expected = checksum(&pubkey);
    if decoded[32] != expected[0] || decoded[33] != expected[1] {
        return None;
    }
    Some(pubkey)
}

/// Is this text an onion address at all? Cheaper than decoding when the answer
/// is only used to decide which kind of peer is being read.
pub fn looks_like_onion(text: &str) -> bool {
    text.trim().to_ascii_lowercase().ends_with(".onion")
}

fn checksum(pubkey: &[u8; 32]) -> [u8; 2] {
    let mut hasher = Sha3_256::new();
    hasher.update(SALT);
    hasher.update(pubkey);
    hasher.update([VERSION]);
    let digest = hasher.finalize();
    [digest[0], digest[1]]
}

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for byte in data {
        buffer = (buffer << 8) | u64::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let value = ALPHABET.iter().position(|a| *a == c)? as u64;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Tor Project's own address, which is published widely enough to be a
    /// fair external check: decoding it validates the checksum against a real
    /// one, and re-encoding proves both directions agree with the spec rather
    /// than merely with each other.
    const REAL: &str = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";

    #[test]
    fn a_real_address_decodes_and_comes_back_the_same() {
        let pubkey = decode(REAL).expect("a published onion address did not validate");
        assert_eq!(encode(&pubkey), REAL);
    }

    #[test]
    fn a_changed_character_is_refused() {
        // One character different: the key is still 32 valid bytes, and only
        // the checksum says it is not an address anybody can reach.
        let broken = REAL.replacen("2gzyxa5", "2gzyxa6", 1);
        assert_ne!(broken, REAL);
        assert!(decode(&broken).is_none(), "a bad checksum was accepted");
    }

    #[test]
    fn things_that_are_not_onion_addresses_are_refused() {
        assert!(decode("192.168.1.1").is_none());
        assert!(decode("").is_none());
        assert!(decode(".onion").is_none());
        assert!(decode("short.onion").is_none());
        // v2 addresses are 16 characters and long dead.
        assert!(decode("expyuzz4wqqyqhjn.onion").is_none());
    }

    #[test]
    fn every_key_round_trips() {
        for seed in 0u8..8 {
            let pubkey = [seed.wrapping_mul(37).wrapping_add(11); 32];
            let address = encode(&pubkey);
            assert!(address.ends_with(".onion"));
            assert_eq!(address.len(), ENCODED_LEN + ".onion".len());
            assert_eq!(decode(&address), Some(pubkey));
        }
    }

    #[test]
    fn onion_addresses_are_recognised_without_decoding() {
        assert!(looks_like_onion(REAL));
        assert!(looks_like_onion("  SOMETHING.ONION "));
        assert!(!looks_like_onion("1.2.3.4"));
    }
}
