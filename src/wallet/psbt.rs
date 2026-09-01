//! Partially signed transactions, on their way to and from other software.
//!
//! PSBT exists so that the thing which *builds* a payment and the thing which
//! *signs* it can be different programs on different machines. Sieve is on both
//! sides of that at different moments: it builds watch-only and hands the file
//! to a signer it will never meet, and it takes a file back from one.
//!
//! **Interoperability is the whole point, so this file follows BIP-174 and adds
//! nothing.** There is no Sieve format, no wrapper, no header of our own. A file
//! written here opens in Sparrow, Electrum, Coldcard and Bitcoin Core, and
//! theirs open here — which is the only reason any of this is worth building.
//!
//! Three decisions, all of them about meeting other wallets where they are:
//!
//! - **Read binary and base64, sniffed rather than asked about.** BIP-174 gives
//!   the binary serialisation the magic `psbt\xff`, and RFC 4648 base64 is the
//!   text form it defines. Coldcard writes binary to an SD card; a great deal of
//!   the internet passes base64 around in messages. Refusing either is an
//!   arbitrary obstacle at the exact moment somebody is trying to move money,
//!   and telling them apart costs five bytes.
//! - **Write binary.** It is what a hardware wallet reading a card expects, and
//!   base64 of it is one function call away for anybody who wants to paste it.
//! - **`.psbt`, always.** The de facto extension everywhere, and what a signing
//!   device looks for on a card.

use anyhow::{Context, Result, bail};
use bdk_wallet::bitcoin::Psbt;

/// The extension every other wallet uses, and what a device scanning a card
/// looks for.
pub const EXTENSION: &str = "psbt";

/// BIP-174's magic: `psbt` and a `0xff` separator. Five bytes, and the only
/// thing needed to tell a binary file from a base64 one.
const MAGIC: &[u8] = b"psbt\xff";

/// Serialise for writing to a file, in the binary form BIP-174 defines.
pub fn to_bytes(psbt: &Psbt) -> Vec<u8> {
    psbt.serialize()
}

/// The base64 text form, for pasting somewhere that takes text.
///
/// `Psbt`'s own `Display`, which is RFC 4648 base64 of exactly the bytes above —
/// so this and `to_bytes` are two spellings of one file, not two formats.
pub fn to_base64(psbt: &Psbt) -> String {
    psbt.to_string()
}

/// Read a PSBT from whatever a file happens to contain.
///
/// Binary when it starts with the magic, base64 otherwise. Base64 is tried on
/// the trimmed text because a file that has been through a chat window, an
/// email or a copy-paste arrives wrapped in whitespace, and refusing it for
/// that would be refusing it for nothing.
pub fn from_bytes(bytes: &[u8]) -> Result<Psbt> {
    if bytes.starts_with(MAGIC) {
        return Psbt::deserialize(bytes).context("that file is not a valid payment file");
    }

    let text = std::str::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("that file is neither a payment file nor text"))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("that file is empty");
    }
    trimmed
        .parse::<Psbt>()
        .context("that file is not a payment file Sieve can read")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk_wallet::bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
        absolute::LockTime, transaction::Version,
    };

    /// An unsigned payment, as `plan()` produces before anything is signed:
    /// one input, one output, no signatures. Built rather than transcribed, so
    /// the test exercises the shape Sieve actually writes.
    fn unsigned() -> Psbt {
        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(50_000),
                script_pubkey: ScriptBuf::new(),
            }],
        };
        Psbt::from_unsigned_tx(tx).expect("an unsigned transaction is a valid PSBT")
    }

    /// The two forms are one file. A wallet handed either must see the same
    /// payment, because "binary or base64" is a transport detail and not a
    /// difference anybody should have to care about.
    #[test]
    fn binary_and_base64_are_two_spellings_of_one_file() {
        let psbt = unsigned();

        let binary = to_bytes(&psbt);
        assert!(
            binary.starts_with(MAGIC),
            "BIP-174's magic is what every other wallet sniffs for"
        );

        let text = to_base64(&psbt);
        assert!(
            text.starts_with("cHNidP8"),
            "base64 of the magic is how the text form is recognised: {text}"
        );

        let from_binary = from_bytes(&binary).expect("binary");
        let from_text = from_bytes(text.as_bytes()).expect("base64");
        assert_eq!(from_binary, from_text);
        assert_eq!(from_binary, psbt, "a round trip changed the payment");
    }

    /// Whitespace is what a file picks up passing through a chat window, an
    /// email or a clipboard. Refusing it would be refusing a payment for
    /// nothing.
    #[test]
    fn text_survives_the_journey_it_will_actually_take() {
        let text = to_base64(&unsigned());
        for messy in [
            format!("\n  {text}  \n\n"),
            format!("{text}\n"),
            format!("   {text}"),
        ] {
            assert!(from_bytes(messy.as_bytes()).is_ok(), "{messy:?}");
        }
    }

    #[test]
    fn what_is_not_a_payment_file_is_refused_and_says_so() {
        for rubbish in [
            &b""[..],
            &b"   "[..],
            &b"not a psbt at all"[..],
            // The magic, then nothing BIP-174 would recognise.
            &b"psbt\xffgarbage"[..],
            // Valid base64 of something that is not a PSBT.
            &b"aGVsbG8gd29ybGQ="[..],
            // Valid UTF-8 that is not base64 either.
            "a payment, honestly".as_bytes(),
        ] {
            let error = from_bytes(rubbish).unwrap_err().to_string();
            assert!(
                error.contains("file"),
                "a refusal should name what was wrong with the file: {error}"
            );
        }
    }

    /// Not decoration: a signing device scanning a card looks for this, and it
    /// is what every other wallet writes.
    #[test]
    fn the_extension_is_the_one_everybody_else_uses() {
        assert_eq!(EXTENSION, "psbt");
    }
}
