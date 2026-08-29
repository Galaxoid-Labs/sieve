//! Spending: building a transaction, signing it, and handing it to the network.
//!
//! Split from the node deliberately. Everything here is pure with respect to
//! the chain — it takes a wallet, a recipient and a fee rate and produces a
//! PSBT, or takes a PSBT and a secret and produces a signed transaction — so
//! the money-critical parts can be tested offline, without peers, without a
//! chain, and without a running node.
//!
//! Two rules shape this module:
//!
//! 1. **Signing derives a fresh wallet from the vault and throws it away.**
//!    Nothing holds a decrypted key between transactions.
//! 2. **A signer that does not derive the same addresses is refused before it
//!    can sign anything.** The commonest cause is a BIP-39 passphrase used at
//!    import and not given here, and the failure mode without this check is a
//!    transaction that silently does not finalize.

use anyhow::{Result, anyhow, bail};
use bdk_wallet::bitcoin::{Address, Amount, FeeRate, Psbt, ScriptBuf};
use bdk_wallet::bitcoin::address::NetworkUnchecked;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::chain::DescriptorExt;
use bdk_wallet::template::{Bip44, Bip49, Bip84, Bip86};
use bdk_wallet::{KeychainKind, Wallet};
use bdk_wallet::bitcoin::Network;
use bdk_wallet::keys::bip39::{Language, Mnemonic};

use super::accounts::ScriptType;

/// How much to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sending {
    Exact(Amount),
    /// Empty the account: no change output, and the fee comes out of the
    /// amount rather than being added to it.
    Everything,
}

/// What the person filling in the form has asked for.
#[derive(Debug, Clone)]
pub struct Draft {
    /// Which derivation path the coins come from. Each path is its own BDK
    /// wallet with its own UTXOs, so a transaction is built from exactly one.
    pub from: ScriptType,
    pub to: Address,
    pub amount: Sending,
    pub fee_rate: FeeRate,
}

/// A built, unsigned transaction, with the numbers to show before signing.
#[derive(Debug, Clone)]
pub struct Plan {
    pub psbt: Psbt,
    pub from: ScriptType,
    pub to: String,
    /// What the recipient receives. Not the same as what was typed when
    /// sending everything, since the fee comes out of it.
    pub spend: Amount,
    pub fee: Amount,
    /// Returning to this wallet on the change chain, if there is any.
    pub change: Option<Amount>,
}

impl Plan {
    /// What leaves the wallet: the payment plus the fee.
    pub fn total(&self) -> Amount {
        self.spend + self.fee
    }

    /// The rate actually achieved, for showing back to the person who chose it.
    pub fn fee_rate(&self) -> Option<FeeRate> {
        let vsize = self.psbt.unsigned_tx.vsize() as u64;
        FeeRate::from_sat_per_vb(self.fee.to_sat().checked_div(vsize)?)
    }
}

/// Parse a recipient address, insisting on this wallet's network.
///
/// The wrong-network case gets its own message. An address is the one string
/// where being nearly right is worse than being wrong: a testnet address on
/// mainnet is a valid-looking way to burn money, and bech32's HRP is a single
/// character of difference on screen.
pub fn parse_address(text: &str, network: Network) -> Result<Address> {
    let text = text.trim();
    if text.is_empty() {
        bail!("enter an address to send to");
    }

    let parsed: Address<NetworkUnchecked> = text
        .parse()
        .map_err(|_| anyhow!("that is not a Bitcoin address"))?;

    parsed.require_network(network).map_err(|_| {
        anyhow!("that address is for a different network — this wallet is on {network}")
    })
}

/// Build the wallet that can sign for one account, from the vault's contents.
///
/// In memory only: `create_wallet_no_persist` never touches the database, so
/// the private descriptors this holds are gone when it is dropped. The
/// watch-only copy on disk stays watch-only.
pub fn signer(
    secret: &str,
    script_type: ScriptType,
    network: Network,
    bip39_passphrase: Option<&str>,
) -> Result<Wallet> {
    let secret = secret.trim();

    // A recovery phrase and an extended key are the same thing written two
    // ways; a WIF is a single key with no derivation at all.
    if Mnemonic::parse_in(Language::English, secret).is_ok() {
        let xprv = super::xprv_from_mnemonic(secret, bip39_passphrase, network)?;
        return hd_signer(xprv, script_type, network);
    }
    if let Ok(xprv) = secret.parse::<Xpriv>() {
        return hd_signer(xprv, script_type, network);
    }

    Wallet::create_single(script_type.single_key_descriptor(secret))
        .network(network)
        .create_wallet_no_persist()
        .map_err(|e| anyhow!("the key in this wallet's file cannot sign: {e}"))
}

fn hd_signer(xprv: Xpriv, script_type: ScriptType, network: Network) -> Result<Wallet> {
    let params = match script_type {
        ScriptType::Legacy => Wallet::create(
            Bip44(xprv, KeychainKind::External),
            Bip44(xprv, KeychainKind::Internal),
        ),
        ScriptType::NestedSegwit => Wallet::create(
            Bip49(xprv, KeychainKind::External),
            Bip49(xprv, KeychainKind::Internal),
        ),
        ScriptType::NativeSegwit => Wallet::create(
            Bip84(xprv, KeychainKind::External),
            Bip84(xprv, KeychainKind::Internal),
        ),
        ScriptType::Taproot => Wallet::create(
            Bip86(xprv, KeychainKind::External),
            Bip86(xprv, KeychainKind::Internal),
        ),
    };

    params
        .network(network)
        .create_wallet_no_persist()
        .map_err(|e| anyhow!("could not derive the signing key: {e}"))
}

/// Refuse a signer that derives a different wallet than the one being spent.
///
/// Both wallets are asked for the same thing — the external descriptor's id —
/// and a mismatch means the key from the vault belongs to some other wallet.
/// In practice that is a BIP-39 passphrase: it is part of the seed, so a
/// missing one derives a valid, different, empty wallet rather than an error.
pub fn check_signer(signing: &Wallet, watching_descriptor_id: bdk_wallet::chain::DescriptorId) -> Result<()> {
    let derived = signing
        .public_descriptor(KeychainKind::External)
        .descriptor_id();
    if derived != watching_descriptor_id {
        bail!(
            "the key in this wallet's file does not derive these addresses. \
             If this wallet was imported with a BIP-39 passphrase, that passphrase \
             is part of the key and signing needs it too."
        );
    }
    Ok(())
}

/// Sign a plan in place. `true` when every input is finalized and the
/// transaction can be extracted.
pub fn sign(signing: &Wallet, psbt: &mut Psbt) -> Result<bool> {
    signing
        .sign(psbt, bdk_wallet::SignOptions::default())
        .map_err(|e| anyhow!("signing failed: {e}"))
}

/// Which output is the recipient's, and which is change.
pub(super) fn split_outputs(psbt: &Psbt, to: &ScriptBuf) -> (Amount, Option<Amount>) {
    let mut spend = Amount::ZERO;
    let mut change = Amount::ZERO;
    for out in &psbt.unsigned_tx.output {
        if &out.script_pubkey == to {
            spend += out.value;
        } else {
            change += out.value;
        }
    }
    (spend, (change > Amount::ZERO).then_some(change))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk_wallet::bitcoin::{
        Network, OutPoint, Transaction, TxIn, TxOut, Txid, absolute, transaction,
    };

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon \
                          abandon abandon abandon abandon abandon about";

    #[test]
    fn a_mainnet_address_is_refused_on_signet() {
        let err = parse_address("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", Network::Signet)
            .unwrap_err()
            .to_string();
        assert!(err.contains("different network"), "{err}");
    }

    #[test]
    fn nonsense_is_not_an_address() {
        assert!(parse_address("not an address", Network::Bitcoin).is_err());
        assert!(parse_address("   ", Network::Bitcoin).is_err());
    }

    #[test]
    fn a_signet_address_parses_on_signet() {
        let address = parse_address(
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            Network::Signet,
        );
        assert!(address.is_ok(), "{address:?}");
    }

    /// The whole spending path, offline: fund a wallet with a fabricated
    /// transaction, build a spend from it, sign it with a wallet derived the
    /// way `signer` derives one, and check it finalizes.
    ///
    /// This is the test that proves a Sieve-built transaction is spendable. It
    /// needs no peers and no chain, so it runs everywhere.
    #[test]
    fn a_built_transaction_signs_and_finalizes() {
        for script_type in ScriptType::ALL {
            let network = Network::Signet;
            let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
            let mut wallet = hd_signer(xprv, script_type, network).unwrap();

            // Pay 100,000 sats into the wallet's first address from thin air.
            let funded = wallet.reveal_next_address(KeychainKind::External).address;
            let funding = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                // A real-looking previous output: an input with a null
                // prevout is a coinbase, and the graph refuses to see one of
                // those in the mempool.
                input: vec![TxIn {
                    previous_output: OutPoint::new(
                        "0000000000000000000000000000000000000000000000000000000000000001"
                            .parse::<Txid>()
                            .unwrap(),
                        0,
                    ),
                    ..Default::default()
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: funded.script_pubkey(),
                }],
            };
            wallet.apply_unconfirmed_txs([(funding, 0u64)]);
            assert_eq!(
                wallet.balance().total(),
                Amount::from_sat(100_000),
                "{script_type} did not see its own funding"
            );

            let to = parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
            let mut psbt = {
                let mut builder = wallet.build_tx();
                builder.add_recipient(to.script_pubkey(), Amount::from_sat(20_000));
                builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
                builder.finish().expect("could not build the transaction")
            };

            let (spend, change) = split_outputs(&psbt, &to.script_pubkey());
            assert_eq!(spend, Amount::from_sat(20_000));
            assert!(change.is_some(), "{script_type} kept no change");

            // Signed by a wallet derived from the phrase, not the one that
            // built it — which is what happens for real, where the builder is
            // watch-only and the signer comes out of the vault.
            let fresh = signer(PHRASE, script_type, network, None).unwrap();
            check_signer(&fresh, wallet.public_descriptor(KeychainKind::External).descriptor_id())
                .unwrap();
            assert!(
                sign(&fresh, &mut psbt).unwrap(),
                "{script_type} did not finalize"
            );
            psbt.clone().extract_tx().expect("could not extract the signed transaction");
        }
    }

    /// A signer derived with a different BIP-39 passphrase is a different
    /// wallet. Caught before signing rather than after broadcasting nothing.
    #[test]
    fn a_signer_for_another_wallet_is_refused() {
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let watching = hd_signer(xprv, ScriptType::Taproot, network).unwrap();

        let other = signer(PHRASE, ScriptType::Taproot, network, Some("a passphrase")).unwrap();
        let err = check_signer(
            &other,
            watching.public_descriptor(KeychainKind::External).descriptor_id(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("passphrase"), "{err}");
    }
}
