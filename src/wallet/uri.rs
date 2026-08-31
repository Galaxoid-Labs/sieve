//! BIP-21 payment URIs — `bitcoin:bc1q…?amount=0.001&label=Alice`.
//!
//! This is what a payment request actually looks like in the wild: it is what
//! a QR code holds, what a "pay me" link opens, and what an invoice page puts
//! on the clipboard. Sieve already *writes* them on the receive screen, so
//! refusing to read one was a wallet that could not pay itself.
//!
//! Nothing here validates the address — the network check belongs to
//! `send::parse_address`, and is the one check that must not be softened by
//! being done in two places.

use anyhow::{Result, bail};

/// A payment request, as far as BIP-21 describes one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Payment {
    /// Exactly as written in the URI. Still to be checked against the network.
    pub address: String,
    /// In satoshis. BIP-21 writes the amount in bitcoin.
    pub amount_sats: Option<u64>,
    /// Who is being paid, according to whoever wrote the URI.
    pub label: Option<String>,
    /// What for, according to the same.
    pub message: Option<String>,
}

/// Read a BIP-21 URI, or say plainly why it cannot be honoured.
///
/// `Ok(None)` means "this is not a URI at all" — a bare address, which the
/// caller should go on treating as one. `Err` is a URI that *is* one and must
/// not be paid: a malformed amount, or a `req-` parameter this wallet does not
/// understand. BIP-21 requires that second refusal, and it matters: a `req-`
/// parameter is the writer saying the payment is wrong without it.
pub fn parse(text: &str) -> Result<Option<Payment>> {
    let text = text.trim();
    // The scheme is case-insensitive, and QR codes are routinely uppercase
    // because uppercase alphanumeric encodes smaller.
    let Some(rest) = strip_scheme(text) else {
        return Ok(None);
    };

    let (address, query) = match rest.split_once('?') {
        Some((address, query)) => (address, query),
        None => (rest, ""),
    };
    let address = address.trim();
    if address.is_empty() {
        bail!("that link has no address in it");
    }

    let mut payment = Payment { address: address.to_owned(), ..Default::default() };

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key).to_lowercase();
        let value = decode(value);

        match key.as_str() {
            "amount" => payment.amount_sats = Some(amount_to_sats(&value)?),
            "label" if !value.is_empty() => payment.label = Some(value),
            "message" if !value.is_empty() => payment.message = Some(value),
            // Anything else is optional and ignorable — unless it is marked
            // required, in which case paying without understanding it is
            // paying the wrong thing.
            other if other.starts_with("req-") => {
                bail!("that payment request needs something Sieve does not support ({other})")
            }
            _ => {}
        }
    }

    Ok(Some(payment))
}

/// `bitcoin:` in any case, with or without the `//` some writers add.
fn strip_scheme(text: &str) -> Option<&str> {
    let scheme = text.get(..8)?;
    if !scheme.eq_ignore_ascii_case("bitcoin:") {
        return None;
    }
    let rest = &text[8..];
    Some(rest.strip_prefix("//").unwrap_or(rest))
}

/// BIP-21 amounts are in bitcoin, decimal, and are money.
///
/// Read with integer arithmetic for the same reason typed amounts are: 0.1 is
/// not representable in binary, and a satoshi lost to rounding here is a
/// satoshi somebody is not paid.
fn amount_to_sats(text: &str) -> Result<u64> {
    let text = text.trim();
    if text.is_empty() {
        bail!("that link has an empty amount");
    }
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if fraction.len() > 8 {
        bail!("that link asks for a fraction of a satoshi");
    }
    if !whole.chars().all(|c| c.is_ascii_digit())
        || !fraction.chars().all(|c| c.is_ascii_digit())
        || whole.is_empty()
    {
        bail!("that link has an amount Sieve cannot read: {text}");
    }

    let whole: u64 = whole.parse().map_err(|_| anyhow::anyhow!("that amount is too large"))?;
    let padded = format!("{fraction:0<8}");
    let fraction: u64 = padded.parse().unwrap_or(0);

    whole
        .checked_mul(100_000_000)
        .and_then(|sats| sats.checked_add(fraction))
        .ok_or_else(|| anyhow::anyhow!("that amount is too large"))
}

/// Percent-decoding, plus `+` for a space as query strings have always meant.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not an escape after all; a bare percent sign.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";

    #[test]
    fn a_bare_address_is_not_a_uri() {
        assert_eq!(parse(ADDRESS).unwrap(), None);
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn an_address_only_uri_yields_the_address() {
        let payment = parse(&format!("bitcoin:{ADDRESS}")).unwrap().unwrap();
        assert_eq!(payment.address, ADDRESS);
        assert_eq!(payment.amount_sats, None);
    }

    #[test]
    fn the_scheme_is_case_insensitive() {
        // QR codes are often uppercase: it encodes smaller.
        let payment = parse(&format!("BITCOIN:{ADDRESS}?amount=0.001")).unwrap().unwrap();
        assert_eq!(payment.amount_sats, Some(100_000));
    }

    #[test]
    fn amounts_are_bitcoin_and_land_on_the_satoshi() {
        let cases = [
            ("1", 100_000_000),
            ("0.1", 10_000_000),
            ("0.00000001", 1),
            ("20.3", 2_030_000_000),
            ("0.00100000", 100_000),
        ];
        for (written, sats) in cases {
            let payment =
                parse(&format!("bitcoin:{ADDRESS}?amount={written}")).unwrap().unwrap();
            assert_eq!(payment.amount_sats, Some(sats), "amount={written}");
        }
    }

    #[test]
    fn an_unreadable_amount_is_refused_rather_than_guessed() {
        for bad in ["amount=", "amount=abc", "amount=0.000000001", "amount=-1", "amount=1,5"] {
            assert!(
                parse(&format!("bitcoin:{ADDRESS}?{bad}")).is_err(),
                "{bad} should not parse"
            );
        }
    }

    #[test]
    fn label_and_message_are_percent_decoded() {
        let payment = parse(&format!(
            "bitcoin:{ADDRESS}?label=Luke-Jr&message=Donation%20for%20project%20xyz"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(payment.label.as_deref(), Some("Luke-Jr"));
        assert_eq!(payment.message.as_deref(), Some("Donation for project xyz"));
    }

    #[test]
    fn unknown_parameters_are_ignored_but_required_ones_are_refused() {
        // BIP-21 is explicit: a `req-` element that is not understood makes the
        // whole URI invalid. Paying anyway would pay something other than what
        // was asked for.
        let fine = parse(&format!("bitcoin:{ADDRESS}?somethingyoudontunderstand=50"))
            .unwrap()
            .unwrap();
        assert_eq!(fine.address, ADDRESS);

        assert!(parse(&format!("bitcoin:{ADDRESS}?req-somethingyoudontget=1")).is_err());
    }

    #[test]
    fn the_examples_from_bip_21_parse() {
        let payment = parse(&format!(
            "bitcoin:{ADDRESS}?amount=50&label=Luke-Jr&message=Donation%20for%20project%20xyz"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(payment.address, ADDRESS);
        assert_eq!(payment.amount_sats, Some(5_000_000_000));
        assert_eq!(payment.label.as_deref(), Some("Luke-Jr"));
    }

    #[test]
    fn a_uri_with_no_address_is_refused() {
        assert!(parse("bitcoin:").is_err());
        assert!(parse("bitcoin:?amount=1").is_err());
    }
}
