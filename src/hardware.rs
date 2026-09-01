//! Hardware signers.
//!
//! The seam is a byte buffer: BDK's output is a PSBT and a device's input is a
//! PSBT, so nothing in `wallet` ever talks to a device and nothing here knows
//! what a wallet is. This module owns the USB side of that seam.
//!
//! What a device gives us to *start* with is an extended public key and the
//! fingerprint of the seed it came from. Those two, plus the derivation path
//! they were taken at, are a watch-only descriptor — which is why importing a
//! hardware wallet and importing a descriptor end up in the same place.
//!
//! Backed by `async-hwi`, which is pure Rust: no Python, no HWI install, no
//! daemon. Linux still needs udev rules for the USB device itself, and that is
//! the commonest reason a plugged-in device is not seen — said plainly in the
//! interface rather than left as an empty list.

use anyhow::{Result, anyhow, bail};
use async_hwi::HWI;
use bdk_wallet::bitcoin::Network;
use bdk_wallet::bitcoin::bip32::DerivationPath;

use crate::wallet::accounts::ScriptType;

/// The devices Sieve can talk to over USB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ledger,
    Coldcard,
    Specter,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Ledger => "Ledger",
            Kind::Coldcard => "Coldcard",
            Kind::Specter => "Specter",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A device that answered.
#[derive(Debug, Clone)]
pub struct Found {
    pub kind: Kind,
    /// What to show in the list: the device and, where it can be had, which
    /// one — two Ledgers plugged in are otherwise indistinguishable.
    pub label: String,
}

/// Everything currently plugged in and awake.
///
/// Each backend is asked separately and a failure in one is logged, not
/// returned: a machine with no serial ports must not report "no devices"
/// because the Specter probe failed, when there is a Ledger sitting right
/// there.
pub async fn enumerate() -> Vec<Found> {
    let mut found = Vec::new();

    match ledgers() {
        Ok(devices) => found.extend(devices),
        Err(e) => tracing::debug!(%e, "no Ledger"),
    }
    match coldcards() {
        Ok(devices) => found.extend(devices),
        Err(e) => tracing::debug!(%e, "no Coldcard"),
    }
    match specters().await {
        Ok(devices) => found.extend(devices),
        Err(e) => tracing::debug!(%e, "no Specter"),
    }

    found
}

fn ledgers() -> Result<Vec<Found>> {
    use async_hwi::ledger::{HidApi, Ledger, TransportHID};

    let api = HidApi::new().map_err(|e| anyhow!("{e}"))?;
    Ok(Ledger::<TransportHID>::enumerate(&api)
        .map(|device| Found {
            kind: Kind::Ledger,
            label: match device.serial_number() {
                Some(serial) if !serial.is_empty() => format!("Ledger · {serial}"),
                _ => "Ledger".into(),
            },
        })
        .collect())
}

fn coldcards() -> Result<Vec<Found>> {
    let mut api = async_hwi::coldcard::api::Api::new().map_err(|e| anyhow!("{e}"))?;
    Ok(api
        .detect()
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .map(|serial| Found {
            kind: Kind::Coldcard,
            label: format!("Coldcard · {}", serial.as_ref() as &str),
        })
        .collect())
}

async fn specters() -> Result<Vec<Found>> {
    use async_hwi::specter::Specter;

    let devices = Specter::enumerate().await.map_err(|e| anyhow!("{e:?}"))?;
    Ok(devices
        .into_iter()
        .map(|_| Found {
            kind: Kind::Specter,
            label: "Specter".into(),
        })
        .collect())
}

/// Open the first device of a kind.
async fn connect(kind: Kind) -> Result<Box<dyn HWI + Send>> {
    match kind {
        Kind::Ledger => {
            let ledger = async_hwi::ledger::Ledger::try_connect_hid()
                .map_err(|e| anyhow!("could not open the Ledger: {e}"))?;
            // Read the accounts without asking for a button press each. Four
            // paths would otherwise be four confirmations for something that
            // discloses nothing — the xpubs are public.
            let ledger = ledger
                .display_xpub(false)
                .map_err(|e| anyhow!("could not set up the Ledger: {}", explain(&e)))?;
            Ok(Box::new(ledger))
        }
        Kind::Coldcard => {
            let mut api = async_hwi::coldcard::api::Api::new().map_err(|e| anyhow!("{e}"))?;
            let serial = api
                .detect()
                .map_err(|e| anyhow!("{e}"))?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("no Coldcard is connected"))?;
            let (device, _) = api
                .open(&serial, None)
                .map_err(|e| anyhow!("could not open the Coldcard: {e}"))?;
            Ok(Box::new(async_hwi::coldcard::Coldcard::from(device)))
        }
        Kind::Specter => {
            let specter = async_hwi::specter::Specter::try_connect()
                .await
                .map_err(|e| anyhow!("could not open the Specter: {e:?}"))?;
            Ok(Box::new(specter))
        }
    }
}

/// The standard account path for a script type on a network.
///
/// `m/purpose'/coin'/0'` — the first account. Coin type 0 is mainnet and 1 is
/// every test network, which is what every wallet and every device agrees on.
pub fn account_path(script_type: ScriptType, network: Network) -> Result<DerivationPath> {
    let coin = if network == Network::Bitcoin { 0 } else { 1 };
    format!("m/{}h/{coin}h/0h", script_type.purpose())
        .parse()
        .map_err(|e| anyhow!("could not build the derivation path: {e}"))
}

/// Ask a device for every standard account it holds.
///
/// This is the whole of importing a hardware wallet: the fingerprint says
/// which seed, each path says which account, and the extended public keys are
/// enough to find every address the device will ever hand out. No secret
/// crosses the USB cable in this direction, and none ever crosses it in the
/// other.
///
/// All four paths, for the same reason a seed import searches all four: one
/// device holds a legacy, a nested, a native segwit and a taproot account at
/// once, and which one has the coins is not something the person importing
/// should have to know. Guessing wrong shows an empty wallet, which reads as
/// lost money.
///
/// One connection and one fingerprint, then a key per path. A path the device
/// refuses is skipped rather than failing the import — an older firmware with
/// no taproot support should still give up its segwit account.
pub async fn account_descriptors(
    kind: Kind,
    network: Network,
) -> Result<Vec<(ScriptType, String)>> {
    let device = connect(kind).await?;

    // Asked for first and logged, because it is the thing that explains most
    // refusals: async-hwi speaks the current command set, and an older app
    // answers "not supported" to everything while looking perfectly healthy.
    let version = match device.get_version().await {
        Ok(version) => {
            tracing::info!(%kind, %version, "device version");
            Some(version.to_string())
        }
        Err(e) => {
            tracing::debug!(%kind, error = %explain(&e), "device would not give its version");
            None
        }
    };

    let fingerprint = device
        .get_master_fingerprint()
        .await
        .map_err(|e| anyhow!("{}", explain(&e)))?;

    let mut found = Vec::new();
    let mut refusals = Vec::new();
    for script_type in ScriptType::ALL {
        let path = account_path(script_type, network)?;
        match device.get_extended_pubkey(&path).await {
            Ok(xpub) => {
                tracing::info!(%script_type, %path, "read an account from the device");
                found.push((
                    script_type,
                    descriptor(script_type, fingerprint, &path, &xpub),
                ));
            }
            Err(e) => {
                tracing::warn!(%script_type, error = %explain(&e), "the device would not give this path");
                refusals.push(format!("{script_type}: {}", explain(&e)));
            }
        }
    }

    if found.is_empty() {
        // Every path refused, which is a different problem from one path
        // refused. The device answered two questions already, so it is neither
        // locked nor asleep — and the commonest cause by far is a network
        // mismatch: test networks derive under coin type 1, and a Ledger's
        // Bitcoin app knows only coin type 0. It says "not supported" to every
        // path and nothing about why.
        let detail = refusals.first().cloned().unwrap_or_default();
        let version = version
            .map(|v| format!(" (version {v})"))
            .unwrap_or_default();

        if network != Network::Bitcoin {
            bail!(
                "{kind}{version} would not give any {network} account. Test networks derive \
                 under a different path than Bitcoin does, and a device's Bitcoin app knows \
                 only Bitcoin's — on a Ledger, open the Bitcoin Test app for {network}, or \
                 choose Bitcoin as the network. The device said: {detail}"
            );
        }
        bail!(
            "{kind}{version} answered, but would not give any account. Its app may be older \
             than Sieve can talk to — a Ledger needs its Bitcoin app at 2.1.2 or newer. \
             The device said: {detail}"
        );
    }
    Ok(found)
}

/// Assemble the descriptor a device's key describes.
///
/// Written out here rather than inside the async call so it can be tested
/// without a device on the desk.
pub fn descriptor(
    script_type: ScriptType,
    fingerprint: bdk_wallet::bitcoin::bip32::Fingerprint,
    path: &DerivationPath,
    xpub: &bdk_wallet::bitcoin::bip32::Xpub,
) -> String {
    // Both chains at once: receiving and change, which is the form every
    // modern wallet exports and the one `wallet::watch` reads.
    let inner = format!(
        "[{fingerprint}/{}]{xpub}/<0;1>/*",
        path.to_string().trim_start_matches("m/")
    );
    match script_type {
        ScriptType::Legacy => format!("pkh({inner})"),
        ScriptType::NestedSegwit => format!("sh(wpkh({inner}))"),
        ScriptType::NativeSegwit => format!("wpkh({inner})"),
        ScriptType::Taproot => format!("tr({inner})"),
    }
}

/// The same account, written the way a Ledger wants it for signing.
///
/// **A "default" wallet policy, which needs no registration.** The Ledger app
/// divides policies in two: standard single-signature ones it recognises on
/// sight, and everything else — multisig, custom miniscript — which must be
/// registered on the device once, confirmed by hand, and thereafter presented
/// with the HMAC that registration returns. The four BIP-44/49/84/86 accounts
/// are all in the first group, so signing needs no setup step at all.
///
/// Two details carry that, and both are easy to get wrong:
///
/// - **The name must be empty.** It is what marks the policy default. A named
///   policy is a registered one, and the device will ask for an HMAC Sieve does
///   not have.
/// - **`/**` rather than `/<0;1>/*`.** The same statement — both chains of the
///   account — in the notation the device's policy language uses. `descriptor`
///   above writes the other form because that is what BDK and other wallets
///   read; this is the same account said to a different listener.
pub fn policy(
    script_type: ScriptType,
    fingerprint: bdk_wallet::bitcoin::bip32::Fingerprint,
    path: &DerivationPath,
    xpub: &bdk_wallet::bitcoin::bip32::Xpub,
) -> String {
    let key = format!(
        "[{fingerprint}/{}]{xpub}/**",
        path.to_string().trim_start_matches("m/")
    );
    match script_type {
        ScriptType::Legacy => format!("pkh({key})"),
        ScriptType::NestedSegwit => format!("sh(wpkh({key}))"),
        ScriptType::NativeSegwit => format!("wpkh({key})"),
        ScriptType::Taproot => format!("tr({key})"),
    }
}

/// Turn a device error into something a person can act on.
fn explain(error: &async_hwi::Error) -> String {
    // The device answered and refused. Repeating "check it is unlocked" at
    // something that has already replied is the kind of advice that sends
    // people to look in the wrong place.
    if let async_hwi::Error::Device(detail) = error
        && detail.contains("NotSupported")
    {
        return format!(
            "the device does not support this command — its app is probably older than \
             Sieve can talk to ({detail})"
        );
    }

    match error {
        async_hwi::Error::DeviceNotFound => {
            "the device is no longer there — check the cable".into()
        }
        async_hwi::Error::DeviceDisconnected => "the device disconnected part way through".into(),
        async_hwi::Error::UnimplementedMethod => "this device cannot do that yet".into(),
        async_hwi::Error::UnsupportedVersion => {
            "this device's firmware is too old for Sieve to talk to".into()
        }
        // Ledger will not answer at all until its Bitcoin app is open, and
        // that is by far the commonest reason for a device that is plugged in,
        // unlocked, and still silent.
        other => format!(
            "{other} — check the device is unlocked, and on a Ledger that the \
             Bitcoin app is open"
        ),
    }
}

/// Whether anything could ever be found: on Linux a device is invisible
/// without udev rules, and an empty list is the same shape as "no permission".
pub fn udev_hint() -> &'static str {
    "If a device is plugged in and unlocked but not listed, Linux may not be letting \
     anything but root see it. A packaged build of Sieve installs the rules that fix \
     that; a build run from source does not, and the file is in packaging/udev. After \
     installing it, unplug the device and plug it in again."
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules a packaged build installs, so a device is visible to the
    /// person at the machine rather than only to root.
    ///
    /// Read here so the test below can hold it against the devices Sieve
    /// claims to support: a signer added to `Kind` with no rule beside it will
    /// not be found on most Linux machines, and nothing else would notice.
    const UDEV_RULES: &str = include_str!("../packaging/udev/51-sieve-hardware.rules");

    #[test]
    fn every_device_sieve_supports_has_a_udev_rule() {
        // The identifiers here are the ones the linked libraries use. If a
        // device is added to `Kind` without a rule, it is invisible on a
        // machine that has not been hand-configured, and the failure looks
        // like "nothing is plugged in".
        for (kind, vendor) in [
            (Kind::Ledger, "2c97"),
            (Kind::Coldcard, "d13e"),
            (Kind::Specter, "f055"),
        ] {
            assert!(
                UDEV_RULES.contains(&format!(r#"ATTRS{{idVendor}}=="{vendor}""#)),
                "no udev rule for {} ({vendor})",
                kind.label()
            );
        }

        // uaccess is the point: it grants the person at the machine, and
        // nobody else, and takes it back at logout. A rule that only sets a
        // mode leaves the device root-only.
        assert!(UDEV_RULES.contains(r#"TAG+="uaccess""#));
    }
    use bdk_wallet::bitcoin::bip32::{Fingerprint, Xpub};
    use std::str::FromStr;

    const XPUB: &str = "xpub6BosfCnifzxcFwrSzQiqu2DBVTshkCXacvNsWGYJVVhhawA7d4R5WSWGFNbi8Aw6ZRc1brxMyWMzG3DSSSSoekkudhUd9yLb6qx39T9nMdj";

    #[test]
    fn the_account_path_follows_the_standards() {
        assert_eq!(
            account_path(ScriptType::NativeSegwit, Network::Bitcoin)
                .unwrap()
                .to_string(),
            "84'/0'/0'"
        );
        assert_eq!(
            account_path(ScriptType::Taproot, Network::Bitcoin)
                .unwrap()
                .to_string(),
            "86'/0'/0'"
        );
        // Every test network is coin type 1, which is what devices expect.
        assert_eq!(
            account_path(ScriptType::NativeSegwit, Network::Signet)
                .unwrap()
                .to_string(),
            "84'/1'/0'"
        );
        assert_eq!(
            account_path(ScriptType::Legacy, Network::Testnet)
                .unwrap()
                .to_string(),
            "44'/1'/0'"
        );
    }

    /// The signing policy and the descriptor describe the same account, and
    /// the *only* differences allowed between them are the two the device's
    /// policy language requires. Everything else matching is what makes it the
    /// same wallet on both sides of the USB cable.
    ///
    /// Without a device on the desk this is the whole of what can be checked
    /// about signing, so it checks it precisely.
    #[test]
    fn the_signing_policy_is_the_same_account_as_the_descriptor() {
        let fingerprint = Fingerprint::from_str("ab12cd34").unwrap();
        let xpub = Xpub::from_str(XPUB).unwrap();

        for script_type in ScriptType::ALL {
            let path = account_path(script_type, Network::Bitcoin).unwrap();
            let policy = policy(script_type, fingerprint, &path, &xpub);
            let descriptor = descriptor(script_type, fingerprint, &path, &xpub);

            // Both chains, in the notation each listener reads. Nested
            // segwit closes two brackets, so the tail is not a fixed string.
            assert!(policy.contains("/**)"), "{script_type}: {policy}");
            assert!(!policy.contains("<0;1>"), "{script_type}: {policy}");
            assert!(descriptor.contains("<0;1>"), "{script_type}: {descriptor}");

            // The same key, origin and script function on both sides.
            assert_eq!(
                policy.replace("/**", "/<0;1>/*"),
                descriptor,
                "{script_type}: the policy and the descriptor are different accounts"
            );
            assert!(policy.contains(&format!("[{fingerprint}/")), "{policy}");
            assert!(policy.contains(XPUB), "{policy}");
        }
    }

    /// The descriptor a device's key becomes has to be one the importer reads
    /// back — these two are the halves of the same seam, and a mismatch would
    /// only show up with a device on the desk.
    #[test]
    fn a_devices_descriptor_is_one_sieve_can_import() {
        let fingerprint = Fingerprint::from_str("ab12cd34").unwrap();
        let xpub = Xpub::from_str(XPUB).unwrap();

        for script_type in ScriptType::ALL {
            let path = account_path(script_type, Network::Bitcoin).unwrap();
            let text = descriptor(script_type, fingerprint, &path, &xpub);

            let parsed = crate::wallet::watch::parse(&text)
                .unwrap_or_else(|e| panic!("{script_type}: {e} — {text}"));
            assert_eq!(parsed.script_type, script_type, "{text}");
            assert!(parsed.external.contains("/0/*"), "{}", parsed.external);
            assert!(parsed.internal.contains("/1/*"), "{}", parsed.internal);
            // The fingerprint has to survive: it is what says which seed, and
            // signing later checks it.
            assert!(parsed.external.contains("ab12cd34"), "{}", parsed.external);
        }
    }
}
