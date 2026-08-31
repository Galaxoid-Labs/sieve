//! Exchange rate, fetched only when asked for.
//!
//! This is the only connection Sieve makes that is not Bitcoin peer-to-peer.
//! It carries no wallet data — no addresses, no balance, no transaction — but
//! it does tell the exchange this machine's IP address and when the wallet was
//! opened. That is a real disclosure for a wallet whose whole point is not
//! making them, so it is off unless switched on, and never happens on a test
//! network where the number would be meaningless anyway.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};

/// Bitfinex's public ticker. No key, no account, no wallet data in the request.
const TICKER: &str = "https://api-pub.bitfinex.com/v2/ticker/tBTCUSD";
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
pub struct Price {
    pub usd: f64,
}

impl Price {
    /// Convert an amount, for display beside the real balance.
    pub fn value_of(&self, sats: u64) -> f64 {
        sats as f64 / 100_000_000.0 * self.usd
    }
}

/// Money written the way money is written: grouped to the thousand, cut to
/// cents. A five-figure balance shown as a bare run of digits has to be
/// counted rather than read.
pub fn usd(amount: f64) -> String {
    let negative = amount < 0.0;
    let cents = (amount.abs() * 100.0).round() as u64;
    let whole = cents / 100;

    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }

    format!(
        "{}{grouped}.{:02}",
        if negative { "-" } else { "" },
        cents % 100
    )
}

/// Fetch the last traded price.
///
/// Blocking: call it from a command, never on the main thread.
pub fn fetch(proxy: Option<crate::tor::Proxy>) -> Result<Price> {
    let body = ureq::get(TICKER)
        .config()
        .timeout_global(Some(TIMEOUT))
        .proxy(crate::tor::ureq_proxy(proxy)?)
        .build()
        .call()
        .context("could not reach the price service")?
        .body_mut()
        .read_to_string()
        .context("could not read the price response")?;

    parse(&body)
}

/// Bitfinex returns a bare array, so the fields are positional:
/// `[BID, BID_SIZE, ASK, ASK_SIZE, DAILY_CHANGE, DAILY_CHANGE_RELATIVE,
///   LAST_PRICE, VOLUME, HIGH, LOW, ...]`
fn parse(body: &str) -> Result<Price> {
    let fields: Vec<f64> =
        serde_json::from_str(body).context("the price service returned something unexpected")?;

    let usd = *fields
        .get(6)
        .context("the price response had no last price")?;
    if !usd.is_finite() || usd <= 0.0 {
        return Err(anyhow!(
            "the price service returned an implausible price: {usd}"
        ));
    }

    Ok(Price { usd })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_are_grouped() {
        assert_eq!(usd(0.0), "0.00");
        assert_eq!(usd(9.5), "9.50");
        assert_eq!(usd(1234.567), "1,234.57");
        assert_eq!(usd(1_234_567.0), "1,234,567.00");
        assert_eq!(usd(-42.0), "-42.00");
    }

    #[test]
    fn reads_the_last_price_not_the_bid() {
        // A real response. Index 6 is the last price; index 0 is the bid, and
        // taking the wrong one would be wrong by the spread and never noticed.
        let body = "[78137,1.4311155,78142,1.26189486,699,0.00902658,\
                    78137,405.09223597,78351,77254,1358182043000]";
        let price = parse(body).unwrap();
        assert_eq!(price.usd, 78137.0);
    }

    #[test]
    fn implausible_prices_are_refused() {
        // Better no number than a wrong one beside somebody's balance.
        assert!(parse("[1,2,3,4,5,6,0,8,9,10]").is_err());
        assert!(parse("[1,2,3,4,5,6,-5,8,9,10]").is_err());
        assert!(parse("[1,2,3]").is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn converts_sats_to_value() {
        let price = Price { usd: 100_000.0 };
        assert!((price.value_of(100_000_000) - 100_000.0).abs() < 0.01);
        assert!((price.value_of(53_713) - 53.713).abs() < 0.01);
    }
}
