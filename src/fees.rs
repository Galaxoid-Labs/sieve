//! Fee rates from mempool.space, when asked for.
//!
//! Optional, off by default, and disclosed where it is switched on — the same
//! treatment the price fetch gets, for the same reason. It carries no wallet
//! data, but it tells a server this machine's IP address and, worse than the
//! price lookup, *when you are about to send a payment*. A request for fee
//! rates is a good predictor of a transaction appearing on the network a
//! minute later.
//!
//! Sieve's own estimate comes from the chain instead: the average fee rate of
//! the block at the tip, computed from its coinbase. That costs a block
//! download and no disclosure at all. This is the alternative for people who
//! would rather have the better number.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Anything above this is a misread response rather than a fee market.
const IMPLAUSIBLE: f64 = 5_000.0;

/// What mempool.space suggests, in sat/vB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recommended {
    pub fastest: f64,
    pub half_hour: f64,
    pub hour: f64,
    pub economy: f64,
    pub minimum: f64,
}

impl Recommended {
    /// The one Sieve fills in: the next few blocks, not the next one.
    ///
    /// Paying for the very next block is rarely what someone sending from a
    /// desktop wallet wants, and the gap between the two is where fees are
    /// wasted.
    pub fn suggested(&self) -> f64 {
        self.half_hour
    }

    /// The line under the fee field, so the number has a provenance.
    pub fn summary(&self) -> String {
        format!(
            "mempool.space: {:.0} next block · {:.0} in ~30 min · {:.0} economy",
            self.fastest, self.half_hour, self.economy
        )
    }
}

/// Where to ask, for the networks mempool.space serves.
///
/// Regtest is a chain on this machine and signet fees are meaningless, but
/// signet is served and asking is harmless, so the shape stays the same as the
/// explorer links.
pub fn endpoint(network: &str) -> Option<String> {
    match network {
        "bitcoin" => Some("https://mempool.space/api/v1/fees/recommended".into()),
        "signet" | "testnet" | "testnet4" => {
            Some(format!("https://mempool.space/{network}/api/v1/fees/recommended"))
        }
        _ => None,
    }
}

/// Fetch the current recommendation.
///
/// Blocking: call it from a command, never on the main thread.
pub fn fetch(network: &str) -> Result<Recommended> {
    let url = endpoint(network)
        .ok_or_else(|| anyhow!("mempool.space does not serve {network}"))?;

    let body = ureq::get(&url)
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .call()
        .context("could not reach mempool.space")?
        .body_mut()
        .read_to_string()
        .context("could not read the fee response")?;

    parse(&body)
}

fn parse(body: &str) -> Result<Recommended> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        fastest_fee: f64,
        half_hour_fee: f64,
        hour_fee: f64,
        economy_fee: f64,
        minimum_fee: f64,
    }

    let response: Response =
        serde_json::from_str(body).context("mempool.space returned something unexpected")?;

    let rates = Recommended {
        fastest: response.fastest_fee,
        half_hour: response.half_hour_fee,
        hour: response.hour_fee,
        economy: response.economy_fee,
        minimum: response.minimum_fee,
    };

    // A wrong number here is money, so an implausible one is refused rather
    // than shown. Sieve's local estimate still works.
    for rate in [rates.fastest, rates.half_hour, rates.hour, rates.economy, rates.minimum] {
        if !rate.is_finite() || rate <= 0.0 || rate > IMPLAUSIBLE {
            return Err(anyhow!("mempool.space returned an implausible fee rate: {rate}"));
        }
    }

    Ok(rates)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real response, from the day the send path was first exercised.
    const BODY: &str = r#"{"fastestFee":3,"halfHourFee":2,"hourFee":1,
                           "economyFee":1,"minimumFee":1}"#;

    #[test]
    fn reads_every_tier() {
        let rates = parse(BODY).unwrap();
        assert_eq!(rates.fastest, 3.0);
        assert_eq!(rates.half_hour, 2.0);
        assert_eq!(rates.economy, 1.0);
        // Not the fastest: paying for the next block is rarely what a desktop
        // wallet's user is actually asking for.
        assert_eq!(rates.suggested(), 2.0);
    }

    #[test]
    fn implausible_rates_are_refused() {
        assert!(parse(r#"{"fastestFee":0,"halfHourFee":1,"hourFee":1,"economyFee":1,"minimumFee":1}"#).is_err());
        assert!(parse(r#"{"fastestFee":9000000,"halfHourFee":1,"hourFee":1,"economyFee":1,"minimumFee":1}"#).is_err());
        assert!(parse("not json").is_err());
        assert!(parse("{}").is_err());
    }

    #[test]
    fn each_network_asks_the_right_host() {
        assert_eq!(
            endpoint("bitcoin").as_deref(),
            Some("https://mempool.space/api/v1/fees/recommended")
        );
        assert_eq!(
            endpoint("signet").as_deref(),
            Some("https://mempool.space/signet/api/v1/fees/recommended")
        );
        // No public server can see a chain running on this machine.
        assert_eq!(endpoint("regtest"), None);
    }
}
