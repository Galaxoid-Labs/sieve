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

/// SLIP-132 version bytes, and the BIP-32 ones they stand in for.
///
/// **These keys are byte-identical to an `xpub` apart from four bytes.**
/// Everything after the version — depth, parent fingerprint, child number,
/// chain code, key — is the same, so converting one is a prefix swap and
/// nothing else. `bitcoin`'s `base58` does the check-encoding either way.
///
/// The reason to bother: people paste what their wallet shows them, and a
/// wallet configured for native segwit shows a `zpub`. Refusing it sends
/// somebody to find a converter, which is a website they paste an extended
/// public key into — the single worst habit this program could encourage.
const SLIP132: &[(&str, [u8; 4], [u8; 4], ScriptType)] = &[
    // prefix, its version bytes, the BIP-32 equivalent, what it describes
    (
        "ypub",
        [0x04, 0x9D, 0x7C, 0xB2],
        [0x04, 0x88, 0xB2, 0x1E],
        ScriptType::NestedSegwit,
    ),
    (
        "zpub",
        [0x04, 0xB2, 0x47, 0x46],
        [0x04, 0x88, 0xB2, 0x1E],
        ScriptType::NativeSegwit,
    ),
    (
        "upub",
        [0x04, 0x4A, 0x52, 0x62],
        [0x04, 0x35, 0x87, 0xCF],
        ScriptType::NestedSegwit,
    ),
    (
        "vpub",
        [0x04, 0x5F, 0x1C, 0xF6],
        [0x04, 0x35, 0x87, 0xCF],
        ScriptType::NativeSegwit,
    ),
];

/// An extended public key rewritten as an `xpub`/`tpub`, and what it said.
///
/// `None` when this is not a SLIP-132 key, which includes every `xpub` — those
/// are already what BDK wants and are left exactly alone.
fn from_slip132(token: &str) -> Option<(String, ScriptType)> {
    let (_, version, standard, script_type) = SLIP132
        .iter()
        .find(|(prefix, ..)| token.starts_with(prefix))?;

    let mut bytes = bdk_wallet::bitcoin::base58::decode_check(token).ok()?;
    // A prefix match is not proof: base58 is not a prefix code, so check the
    // bytes actually say what the four characters claimed before trusting them.
    if bytes.len() < 4 || bytes[..4] != version[..] {
        return None;
    }
    bytes[..4].copy_from_slice(standard);
    Some((
        bdk_wallet::bitcoin::base58::encode_check(&bytes),
        *script_type,
    ))
}

/// Rewrite every SLIP-132 key in `text`, and report what they described.
///
/// The script type comes back because it is information an `xpub` does not
/// carry: `zpub` *means* BIP-84. See `script_type_of`.
fn normalise_slip132(text: &str) -> (String, Option<ScriptType>) {
    let mut found = None;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    // Split on the characters a key can be delimited by, keeping them, so a
    // key inside `wpkh([origin]zpub…/0/*)` is rewritten in place.
    while let Some(at) = rest.find(|c: char| c.is_ascii_alphanumeric()) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        let (token, tail) = rest.split_at(end);
        match from_slip132(token) {
            Some((converted, script_type)) => {
                out.push_str(&converted);
                found = found.or(Some(script_type));
            }
            None => out.push_str(token),
        }
        rest = tail;
    }
    out.push_str(rest);
    (out, found)
}

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

    // Strip a checksum: BDK computes its own, and a stale one is a hard error
    // for something nobody typed on purpose. Before the private-key check
    // rather than after, so eight characters of an alphabet that also holds
    // p, r and v cannot be mistaken for a key.
    let text = text.split('#').next().unwrap_or(text).trim();

    if holds_private_key(text) {
        bail!(
            "that is a private key. A watch-only wallet takes the public half — \
             an xpub, or a descriptor built from one"
        );
    }

    // After the private-key refusal, so a `zprv` is still turned away as a
    // private key rather than quietly converted into something.
    let (converted, from_prefix) = normalise_slip132(text);
    let text = converted.as_str();

    // A SLIP-132 prefix is itself an answer: `zpub` *means* BIP-84. An `xpub`
    // says nothing, which is why a bare one is refused — so this is the rule
    // applied to a key that carries the information, not an exception to it.
    // A labelled pair, which is what several wallets export rather than one
    // multipath descriptor — Bitkey's "current wallet descriptor" among them:
    //
    //     External: wsh(sortedmulti(2,…/0/*,…))
    //     Internal: wsh(sortedmulti(2,…/1/*,…))
    //
    // Taken as given rather than derived, because a wallet that writes both
    // chains down knows better than a rule that guesses the second from the
    // first.
    if let Some((external, internal)) = labelled_pair(text) {
        let script_type =
            script_type_of(&external).or_else(|e| normalise_slip132(&external).1.ok_or(e))?;
        return Ok(Descriptors {
            external,
            internal,
            script_type,
        });
    }

    let script_type = match script_type_of(text) {
        Ok(script_type) => script_type,
        Err(e) => from_prefix.ok_or(e)?,
    };

    // A descriptor is anything with a function around the key.
    if text.contains('(') {
        let (external, internal) = split_paths(text)?;
        return Ok(Descriptors {
            external,
            internal,
            script_type,
        });
    }

    // A bare key, with an origin to say what it is.
    let external = wrap(text, script_type, 0)?;
    let internal = wrap(text, script_type, 1)?;
    Ok(Descriptors {
        external,
        internal,
        script_type,
    })
}

/// Whether what was pasted carries an extended *private* key.
///
/// The test has to be on the key itself, not on the whole string. Searching
/// the text for "prv" looks equivalent and is not: base58 contains p, r and v,
/// so roughly one public descriptor in eighteen hundred holds "prv" somewhere
/// in the body of its xpub. Those were refused as private keys — a device's
/// own export, turned away with the one message guaranteed to persuade
/// somebody that they had exported the wrong thing.
///
/// Every version prefix says which half it is in the same place: xprv, tprv,
/// yprv, zprv, uprv, vprv and the capitalised multisig forms all read "prv" at
/// characters two to four, against "pub" for every public one. So that is what
/// is read, at the positions where a key can actually begin.
///
/// Extended keys only, which is what people paste by mistake. A WIF key inside
/// a descriptor is not caught here and never was; BDK refuses it downstream,
/// with a worse message.
fn holds_private_key(text: &str) -> bool {
    text.split(['(', ')', ',', ' ', '\t', '\n'])
        // A key follows its origin: [ab12cd34/84h/0h/0h]xprv…
        .map(|token| match token.split_once(']') {
            Some((_, after_origin)) => after_origin,
            None => token,
        })
        .any(|token| token.get(1..4) == Some("prv"))
}

/// Two descriptors written down under labels, if that is what this is.
///
/// Case-insensitive on the label and tolerant of the separator, because this
/// is a convention rather than a standard — wallets write `External:`,
/// `external =`, and worse. Anything that does not hold both labels is left
/// alone, so this cannot swallow an ordinary descriptor.
fn labelled_pair(text: &str) -> Option<(String, String)> {
    let mut external = None;
    let mut internal = None;

    for line in text.lines() {
        let line = line.trim();
        let (label, rest) = line.split_once([':', '='])?;
        let value = rest.trim();
        if value.is_empty() {
            return None;
        }
        // Strip a checksum here too: these arrive with one, and the pair is
        // returned without going back through the caller's stripping.
        let value = value.split('#').next().unwrap_or(value).trim().to_string();
        match label.trim().to_ascii_lowercase().as_str() {
            "external" | "receive" | "receiving" => external = Some(value),
            "internal" | "change" => internal = Some(value),
            _ => return None,
        }
    }

    let (external, internal) = (external?, internal?);
    // Both must be descriptors. Two labels around something else is not this.
    (external.contains('(') && internal.contains('(')).then_some((external, internal))
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
        // origin; only the bare-key case is genuinely ambiguous. A descriptor
        // carries its own chain — `/0/*` — so unlike a bare key it needs no
        // origin to be complete, and an origin is only there for signing.
        _ if text.starts_with("tr(") => Ok(ScriptType::Taproot),
        _ if text.starts_with("wpkh(") => Ok(ScriptType::NativeSegwit),
        _ if text.starts_with("sh(wpkh(") => Ok(ScriptType::NestedSegwit),
        _ if text.starts_with("pkh(") => Ok(ScriptType::Legacy),
        // A script hash — a multisig, usually. The label is approximate: a
        // P2WSH output is bech32 like a P2WPKH one but is not BIP-84, and
        // `ScriptType` here only names the account and its database file. What
        // governs the addresses is the descriptor, which is stored and used
        // exactly as written. Refusing these instead would refuse a wallet
        // Sieve can watch perfectly well.
        _ if text.starts_with("wsh(") => Ok(ScriptType::NativeSegwit),
        _ if text.starts_with("sh(wsh(") => Ok(ScriptType::NestedSegwit),
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
    //
    // **Every group, not the first.** A multisig descriptor writes one per
    // cosigner — `wsh(sortedmulti(2,A/<0;1>/*,B/<0;1>/*,C/<0;1>/*))` — and
    // substituting only the first left the others as literal `<0;1>`.
    if descriptor.contains('<') {
        let mut external = String::with_capacity(descriptor.len());
        let mut internal = String::with_capacity(descriptor.len());
        let mut rest = descriptor;
        while let Some(start) = rest.find('<') {
            let end = rest[start..]
                .find('>')
                .map(|offset| start + offset)
                .ok_or_else(|| anyhow!("that descriptor has an unclosed <…>"))?;
            let choices: Vec<&str> = rest[start + 1..end].split(';').collect();
            if choices.len() != 2 {
                bail!("Sieve reads multipath descriptors with two chains, like <0;1>");
            }
            external.push_str(&rest[..start]);
            external.push_str(choices[0]);
            internal.push_str(&rest[..start]);
            internal.push_str(choices[1]);
            rest = &rest[end + 1..];
        }
        external.push_str(rest);
        internal.push_str(rest);
        return Ok((external, internal));
    }

    // Single path. The receive chain ends /0/*, and its change chain is /1/*.
    //
    // **All of them**, for the same reason: with one key per cosigner, moving
    // only the first produced a change descriptor describing a *different*
    // multisig — one signer on the change chain and the rest still on the
    // receive one. It parsed, it derived addresses, and they were not this
    // wallet's change addresses. Change would simply not have been seen.
    if descriptor.contains("/0/*") {
        return Ok((descriptor.to_string(), descriptor.replace("/0/*", "/1/*")));
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

    /// BIP-84's own zpub, and BIP-84's own first address.
    ///
    /// **Checked against something outside this program**, because a wrong
    /// version-byte swap produces a key that parses perfectly and describes a
    /// wallet somewhere else entirely: no error, no coins, an empty screen. A
    /// round-trip test would agree with itself and prove none of that. These
    /// two strings are from the BIP-84 specification, for the phrase
    /// `abandon … about`.
    #[test]
    fn a_zpub_derives_the_address_bip84_says_it_should() {
        use std::str::FromStr;

        const ZPUB: &str = "zpub6rFR7y4Q2AijBEqTUquhVz398htDFrtymD9xYYfG1m4wAcvPhXNfE3EfH1r1ADqtfSdVCToUG868RvUUkgDKf31mGDtKsAYz2oz2AGutZYs";
        const FIRST: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";

        // Bare, with no origin — which is what a wallet's information screen
        // shows, and what an xpub would rightly be refused for.
        let parsed = parse(ZPUB).expect("a zpub says which addresses it makes");
        assert_eq!(parsed.script_type, ScriptType::NativeSegwit);
        assert!(parsed.external.starts_with("wpkh("), "{}", parsed.external);
        assert!(
            parsed.external.contains("xpub"),
            "the key must be rewritten as an xpub: {}",
            parsed.external
        );

        let descriptor = bdk_wallet::miniscript::Descriptor::<
            bdk_wallet::miniscript::DescriptorPublicKey,
        >::from_str(&parsed.external)
        .expect("a descriptor BDK can read");
        let address = descriptor
            .at_derivation_index(0)
            .unwrap()
            .address(bdk_wallet::bitcoin::Network::Bitcoin)
            .unwrap();
        assert_eq!(
            address.to_string(),
            FIRST,
            "the conversion moved the wallet"
        );
    }

    /// An xpub is left exactly alone.
    #[test]
    fn an_ordinary_key_is_not_rewritten() {
        let (text, prefix) = normalise_slip132(XPUB);
        assert_eq!(text, XPUB);
        assert_eq!(prefix, None);

        // And a bare one is still refused, because it still says nothing.
        assert!(parse(XPUB).is_err());
    }

    /// A key inside a descriptor is converted in place.
    #[test]
    fn a_slip132_key_inside_a_descriptor_is_rewritten_where_it_stands() {
        const ZPUB: &str = "zpub6rFR7y4Q2AijBEqTUquhVz398htDFrtymD9xYYfG1m4wAcvPhXNfE3EfH1r1ADqtfSdVCToUG868RvUUkgDKf31mGDtKsAYz2oz2AGutZYs";
        let text = format!("wpkh([ab12cd34/84h/0h/0h]{ZPUB}/<0;1>/*)");
        let parsed = parse(&text).unwrap();
        assert!(parsed.external.contains("xpub"), "{}", parsed.external);
        assert!(!parsed.external.contains("zpub"), "{}", parsed.external);
        // The origin still wins for the script type; it agrees here anyway.
        assert_eq!(parsed.script_type, ScriptType::NativeSegwit);
        assert!(parsed.external.contains("[ab12cd34/84h/0h/0h]"));
    }

    /// The private halves stay refused, and are not converted on the way.
    #[test]
    fn a_slip132_private_key_is_still_a_private_key() {
        const ZPRV: &str = "zprvAWgYBBk7JR8Gjrh4UJQ2uJdG1r3WNRRfURiABBE3RvMXYSrRJL62XuezvGdPvG6GFBZduosCc1YP5wixPox7zhZLfiUm8aunE96BBa4Kei5";
        let err = parse(ZPRV).unwrap_err().to_string();
        assert!(err.contains("private key"), "{err}");
    }

    const B: &str = "xpub6ERApfZwUNrhLCkDtcHTcxd75RbzS1ed54G1LkBUHQVHQKqhMkhgbmJbZRkrgZw4koxb5JaHWkY4ALHY2grBGRjaDMzQLcgJvLJuZZvRcEL";
    const C: &str = "xpub6ELHKXNimKbxMCytPh7EdC2QXx46T9qLDJWGnTraz1H9kMMFdcduoU69wh9cxP12wDxqAAfbaESWGYt5rREsX1J8iR2TEunvzvddduAPYcY";

    /// Every cosigner moves to the change chain, not only the first.
    ///
    /// **This was wrong, and silently.** `replacen(…, 1)` is correct for a
    /// descriptor with one key and wrong for every multisig: it produced a
    /// change descriptor describing a *different* wallet — one signer on the
    /// change chain and the rest still on the receive one. It parsed, it
    /// derived perfectly good addresses, and they were not this wallet's
    /// change addresses, so change would never have been seen. An understated
    /// balance with nothing on screen to explain it.
    #[test]
    fn every_cosigner_moves_to_the_change_chain() {
        let external = format!(
            "wsh(sortedmulti(2,[aaaaaaaa/84h/0h/0h]{XPUB}/0/*,\
             [bbbbbbbb/84h/0h/0h]{B}/0/*,[cccccccc/84h/0h/0h]{C}/0/*))"
        );
        let parsed = parse(&external).unwrap();

        assert_eq!(parsed.external.matches("/0/*").count(), 3);
        assert_eq!(
            parsed.internal.matches("/1/*").count(),
            3,
            "every key must be on the change chain: {}",
            parsed.internal
        );
        assert_eq!(
            parsed.internal.matches("/0/*").count(),
            0,
            "no key may be left on the receive chain: {}",
            parsed.internal
        );
    }

    /// The same, written the multipath way.
    #[test]
    fn every_multipath_group_is_substituted() {
        let text = format!("wsh(sortedmulti(2,{XPUB}/<0;1>/*,{B}/<0;1>/*,{C}/<0;1>/*))");
        let parsed = parse(&text).unwrap();
        assert!(
            !parsed.external.contains('<') && !parsed.internal.contains('<'),
            "a group was left unsubstituted: {} / {}",
            parsed.external,
            parsed.internal
        );
        assert_eq!(parsed.external.matches("/0/*").count(), 3);
        assert_eq!(parsed.internal.matches("/1/*").count(), 3);
    }

    /// A wallet that writes both chains down is believed rather than guessed at.
    #[test]
    fn a_labelled_pair_is_taken_as_given() {
        let external =
            format!("wsh(sortedmulti(2,[aaaaaaaa/84h/0h/0h]{XPUB}/0/*,{B}/0/*,{C}/0/*))");
        let internal =
            format!("wsh(sortedmulti(2,[aaaaaaaa/84h/0h/0h]{XPUB}/1/*,{B}/1/*,{C}/1/*))");
        let bundle = format!("External: {external}\nInternal: {internal}");

        let parsed = parse(&bundle).unwrap();
        assert_eq!(parsed.external, external);
        assert_eq!(parsed.internal, internal);
        assert_eq!(parsed.script_type, ScriptType::NativeSegwit);

        // Labels are a convention, not a standard.
        let loose = format!("receive = {external}\nchange = {internal}");
        assert_eq!(parse(&loose).unwrap().external, external);

        // An ordinary descriptor is not mistaken for a pair.
        let plain = format!("wpkh([ab12cd34/84h/0h/0h]{XPUB}/0/*)");
        assert_eq!(parse(&plain).unwrap().external, plain);
    }

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
        // Behind an origin, which is the shape a device would export.
        assert!(parse(&format!("wpkh([ab12cd34/84h/0h/0h]{xprv}/<0;1>/*)")).is_err());
        // And behind a checksum, which is stripped before the key is read.
        assert!(parse(&format!("wpkh({xprv}/0/*)#abcdefgh")).is_err());

        // Every SLIP-132 private prefix, since each names the same half.
        for prefix in [
            "xprv", "tprv", "yprv", "zprv", "uprv", "vprv", "Yprv", "Zprv",
        ] {
            let key = format!("{prefix}{}", &xprv[4..]);
            let text = format!("wpkh([ab12cd34/84h/0h/0h]{key}/<0;1>/*)");
            assert!(parse(&text).is_err(), "{prefix}");
        }
    }

    /// The refusal above used to be a substring search over the whole text,
    /// which turned away public descriptors: base58 holds p, r and v, so about
    /// one xpub in eighteen hundred contains "prv" and nothing about it is
    /// private. A device's own export, refused as a private key.
    #[test]
    fn a_public_key_that_happens_to_contain_prv_is_still_public() {
        // The real constant with three characters of its body replaced, which
        // is exactly what the unlucky ones look like.
        let unlucky = format!("{}prv{}", &XPUB[..40], &XPUB[43..]);
        assert_eq!(unlucky.len(), XPUB.len());
        assert!(unlucky.contains("prv"));

        let parsed = parse(&format!("wpkh([ab12cd34/84h/0h/0h]{unlucky}/<0;1>/*)"))
            .expect("a public descriptor is not a private key");
        assert!(parsed.external.contains(&unlucky));
        assert_eq!(parsed.script_type, ScriptType::NativeSegwit);

        // And a checksum from an alphabet that also holds p, r and v.
        let text = format!("wpkh([ab12cd34/84h/0h/0h]{XPUB}/<0;1>/*)#aprvcdef");
        assert!(parse(&text).is_ok(), "a checksum is not a key");
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
