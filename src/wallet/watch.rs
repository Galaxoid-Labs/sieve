//! Watch-only wallets: descriptors in, no keys ever.
//!
//! This is what a hardware wallet needs. The device holds the keys and Sieve
//! holds only the public descriptors, which is enough to find the coins, build
//! a transaction and check what came back — everything except the signature.
//!
//! The work here is turning what someone pastes into the pair of descriptors
//! BDK wants: one for receiving, one for change. Devices and other wallets
//! export several shapes and all of them are common, so all of them are
//! accepted, and anything ambiguous is refused with a description of what to
//! paste instead rather than a guess.

use anyhow::{Result, anyhow, bail};

use super::accounts::ScriptType;

/// The receive and change descriptors a wallet is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptors {
    pub external: String,
    pub internal: String,
    /// Which standard path this describes, when it can be told from the key
    /// origin. Only used to label the account.
    pub script_type: ScriptType,
}

/// Read whatever was pasted into a receive/change pair.
///
/// Three shapes, all of which devices and other wallets really do produce:
///
/// - a multipath descriptor, `wpkh([ab12cd34/84h/0h/0h]xpub…/<0;1>/*)`
/// - a single-path descriptor ending `/0/*`, whose change chain is `/1/*`
/// - a bare extended key with its origin, `[ab12cd34/84h/0h/0h]xpub…`
///
/// A bare key with no origin is refused: the derivation path is what says
/// whether these are legacy, nested, native segwit or taproot addresses, and
/// guessing wrong produces a wallet that finds nothing and looks broken.
pub fn parse(text: &str) -> Result<Descriptors> {
    let text = text.trim();
    if text.is_empty() {
        bail!("paste a descriptor or an extended public key");
    }
    if text.contains("prv") {
        bail!(
            "that is a private key. A watch-only wallet takes the public half — \
             an xpub, or a descriptor built from one"
        );
    }

    // Strip a checksum: BDK computes its own, and a stale one is a hard error
    // for something nobody typed on purpose.
    let text = text.split('#').next().unwrap_or(text).trim();

    let script_type = script_type_of(text)?;

    // A descriptor is anything with a function around the key.
    if text.contains('(') {
        let (external, internal) = split_paths(text)?;
        return Ok(Descriptors { external, internal, script_type });
    }

    // A bare key, with an origin to say what it is.
    let external = wrap(text, script_type, 0)?;
    let internal = wrap(text, script_type, 1)?;
    Ok(Descriptors { external, internal, script_type })
}

/// Which standard path this key or descriptor belongs to.
///
/// From the purpose in the key origin — 84h is native segwit and so on —
/// because that is the only part of a descriptor that says so unambiguously,
/// and a script function alone (`wpkh`) does not distinguish a BIP84 wallet
/// from a bare key someone wrapped by hand.
fn script_type_of(text: &str) -> Result<ScriptType> {
    // Inside the origin brackets: [fingerprint/purpose'/coin'/account']
    let origin = text
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(origin, _)| origin);

    let purpose = origin.and_then(|origin| {
        origin
            .split('/')
            .nth(1)
            .map(|p| p.trim_end_matches(['h', '\'']).to_string())
    });

    match purpose.as_deref() {
        Some("44") => Ok(ScriptType::Legacy),
        Some("49") => Ok(ScriptType::NestedSegwit),
        Some("84") => Ok(ScriptType::NativeSegwit),
        Some("86") => Ok(ScriptType::Taproot),
        // A descriptor still says which script it builds, even without an
        // origin; only the bare-key case is genuinely ambiguous.
        _ if text.starts_with("tr(") => Ok(ScriptType::Taproot),
        _ if text.starts_with("wpkh(") => Ok(ScriptType::NativeSegwit),
        _ if text.starts_with("sh(wpkh(") => Ok(ScriptType::NestedSegwit),
        _ if text.starts_with("pkh(") => Ok(ScriptType::Legacy),
        _ => bail!(
            "this does not say which kind of addresses it makes. Paste a descriptor, \
             or an extended key with its derivation path — \
             [ab12cd34/84h/0h/0h]xpub… — so Sieve knows what to look for"
        ),
    }
}

/// Turn a bare key into a descriptor for one chain.
fn wrap(key: &str, script_type: ScriptType, chain: u8) -> Result<String> {
    let inner = format!("{key}/{chain}/*");
    Ok(match script_type {
        ScriptType::Legacy => format!("pkh({inner})"),
        ScriptType::NestedSegwit => format!("sh(wpkh({inner}))"),
        ScriptType::NativeSegwit => format!("wpkh({inner})"),
        ScriptType::Taproot => format!("tr({inner})"),
    })
}

/// Split a descriptor into its receive and change forms.
fn split_paths(descriptor: &str) -> Result<(String, String)> {
    // Multipath: <0;1> is the standard way of writing both chains at once.
    if let Some(start) = descriptor.find('<') {
        let end = descriptor[start..]
            .find('>')
            .map(|offset| start + offset)
            .ok_or_else(|| anyhow!("that descriptor has an unclosed <…>"))?;
        let choices: Vec<&str> = descriptor[start + 1..end].split(';').collect();
        if choices.len() != 2 {
            bail!("Sieve reads multipath descriptors with two chains, like <0;1>");
        }
        let head = &descriptor[..start];
        let tail = &descriptor[end + 1..];
        return Ok((
            format!("{head}{}{tail}", choices[0]),
            format!("{head}{}{tail}", choices[1]),
        ));
    }

    // Single path. The receive chain ends /0/*, and its change chain is /1/*.
    if descriptor.contains("/0/*") {
        return Ok((
            descriptor.to_string(),
            descriptor.replacen("/0/*", "/1/*", 1),
        ));
    }

    // A descriptor with no wildcard describes one address, not a wallet.
    if !descriptor.contains('*') {
        bail!(
            "that descriptor describes a single address. Sieve needs one with a \
             wildcard, ending /0/* or /<0;1>/*"
        );
    }

    bail!(
        "Sieve could not tell the receive chain from the change chain. Paste a \
         descriptor ending /0/* or /<0;1>/*"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const XPUB: &str = "xpub6BosfCnifzxcFwrSzQiqu2DBVTshkCXacvNsWGYJVVhhawA7d4R5WSWGFNbi8Aw6ZRc1brxMyWMzG3DSSSSoekkudhUd9yLb6qx39T9nMdj";

    #[test]
    fn a_multipath_descriptor_splits_into_two_chains() {
        let text = format!("wpkh([ab12cd34/84h/0h/0h]{XPUB}/<0;1>/*)");
        let parsed = parse(&text).unwrap();
        assert!(parsed.external.ends_with("/0/*)"), "{}", parsed.external);
        assert!(parsed.internal.ends_with("/1/*)"), "{}", parsed.internal);
        assert_eq!(parsed.script_type, ScriptType::NativeSegwit);
    }

    #[test]
    fn a_single_path_descriptor_gets_its_change_chain() {
        let text = format!("tr([ab12cd34/86h/0h/0h]{XPUB}/0/*)");
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed.external, text);
        assert!(parsed.internal.ends_with("/1/*)"), "{}", parsed.internal);
        assert_eq!(parsed.script_type, ScriptType::Taproot);
    }

    #[test]
    fn a_bare_key_with_an_origin_is_wrapped_by_its_purpose() {
        for (purpose, script_type, opening) in [
            ("44h", ScriptType::Legacy, "pkh("),
            ("49h", ScriptType::NestedSegwit, "sh(wpkh("),
            ("84h", ScriptType::NativeSegwit, "wpkh("),
            ("86h", ScriptType::Taproot, "tr("),
        ] {
            let text = format!("[ab12cd34/{purpose}/0h/0h]{XPUB}");
            let parsed = parse(&text).unwrap();
            assert_eq!(parsed.script_type, script_type, "{purpose}");
            assert!(parsed.external.starts_with(opening), "{}", parsed.external);
            // Nested segwit closes two brackets, so the chain is checked
            // rather than the tail.
            assert!(parsed.external.contains("/0/*"), "{}", parsed.external);
            assert!(parsed.internal.contains("/1/*"), "{}", parsed.internal);
        }
    }

    /// Apostrophes and h mean the same thing in a derivation path, and devices
    /// export both.
    #[test]
    fn hardened_notation_does_not_matter() {
        let with_h = parse(&format!("[ab12cd34/84h/0h/0h]{XPUB}")).unwrap();
        let with_tick = parse(&format!("[ab12cd34/84'/0'/0']{XPUB}")).unwrap();
        assert_eq!(with_h.script_type, with_tick.script_type);
    }

    #[test]
    fn a_private_key_is_refused_before_it_is_stored() {
        let xprv = "xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi";
        let error = parse(xprv).unwrap_err().to_string();
        assert!(error.contains("private key"), "{error}");
        // Even wrapped in a descriptor.
        assert!(parse(&format!("wpkh({xprv}/0/*)")).is_err());
    }

    #[test]
    fn what_cannot_be_read_is_refused_rather_than_guessed() {
        // No origin and no script function: nothing says which addresses.
        assert!(parse(XPUB).is_err());
        // One address, not a wallet.
        assert!(parse(&format!("wpkh([ab12cd34/84h/0h/0h]{XPUB}/0/5)")).is_err());
        // Three chains is not a shape Sieve reads.
        assert!(parse(&format!("wpkh([ab12cd34/84h/0h/0h]{XPUB}/<0;1;2>/*)")).is_err());
        assert!(parse("").is_err());
        assert!(parse("not a descriptor").is_err());
    }

    /// A checksum belongs to the text it was computed over; ours is computed
    /// fresh, and a stale one would be rejected for something nobody typed.
    #[test]
    fn a_checksum_is_dropped() {
        let text = format!("wpkh([ab12cd34/84h/0h/0h]{XPUB}/<0;1>/*)#abcdefgh");
        let parsed = parse(&text).unwrap();
        assert!(!parsed.external.contains('#'), "{}", parsed.external);
    }
}
