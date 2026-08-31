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
use bdk_wallet::bitcoin::Network;
use bdk_wallet::bitcoin::address::NetworkUnchecked;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::bitcoin::{Address, Amount, FeeRate, Psbt, ScriptBuf};
use bdk_wallet::chain::DescriptorExt;
use bdk_wallet::keys::bip39::{Language, Mnemonic};
use bdk_wallet::template::{Bip44, Bip49, Bip84, Bip86};
use bdk_wallet::{KeychainKind, Wallet};

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
    /// Which coins to spend. Empty means "choose for me", which is what most
    /// payments should be; a non-empty list means exactly these and nothing
    /// else, because the point of choosing is that nothing is added behind
    /// your back.
    pub coins: Vec<bdk_wallet::bitcoin::OutPoint>,
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
    /// The payment this one replaces, when it is a fee bump. Both spend the
    /// same coins, so only one of them can ever confirm.
    pub replaces: Option<String>,
    /// What that payment was paying, so the increase can be shown rather than
    /// only the new figure.
    pub was_fee: Option<u64>,
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

    // A payment request is normally taken apart by the form as it is pasted.
    // Doing it again here means nothing can reach a signature with a URI in
    // the field — and the amount in one is never read from this path, so a
    // request cannot quietly change what is being sent.
    let unpacked = super::uri::parse(text)?.map(|payment| payment.address);
    let text = unpacked.as_deref().unwrap_or(text);

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
pub fn check_signer(
    signing: &Wallet,
    watching_descriptor_id: bdk_wallet::chain::DescriptorId,
) -> Result<()> {
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

/// Coins this wallet holds that are not in a block yet.
///
/// Excluded from coin selection. Everything unconfirmed here is a transaction
/// Sieve broadcast itself — a filter client cannot see anyone else's mempool —
/// so spending it means building on a transaction that could still be dropped
/// or replaced, which would invalidate the child along with it. Waiting for a
/// block is the honest default, and the balance shown as available already
/// counts only confirmed coins.
pub(super) fn unconfirmed_outpoints(wallet: &Wallet) -> Vec<bdk_wallet::bitcoin::OutPoint> {
    wallet
        .list_unspent()
        .filter(|utxo| !utxo.chain_position.is_confirmed())
        .map(|utxo| utxo.outpoint)
        .collect()
}

/// Roughly how big a payment will be, in virtual bytes.
///
/// For telling somebody whether their chosen coins cover a payment *including*
/// its fee, before anything is built. A selection that covers only the amount
/// fails at build time, because an exact-amount payment adds the fee on top —
/// so saying "covers it, before the fee" was true and useless.
///
/// An estimate, and labelled as one wherever it is shown. It assumes
/// single-signature spends and both outputs on this wallet's own script type,
/// which is what makes it cheap enough to recompute on every tick of a
/// checkbox. Sizes are the standard ones for each type.
pub fn estimated_vbytes(script_type: ScriptType, inputs: usize, outputs: usize) -> u64 {
    let (input, output) = match script_type {
        // Signature and public key in the scriptSig, all of it counted.
        ScriptType::Legacy => (148, 34),
        // Witness discount on the signature, redeem script still on-chain.
        ScriptType::NestedSegwit => (91, 32),
        ScriptType::NativeSegwit => (68, 31),
        // Key-path spend: one 64-byte signature, witness-discounted.
        ScriptType::Taproot => (58, 43),
    };
    // Version, locktime and the two counts, plus the segwit marker and flag
    // where there is a witness at all.
    let overhead = if matches!(script_type, ScriptType::Legacy) {
        10
    } else {
        11
    };
    overhead + input * inputs as u64 + output * outputs as u64
}

/// What that payment would cost at a given rate, rounded up: a fee estimate
/// that rounds down is an estimate that says yes and then fails.
pub fn estimated_fee(
    script_type: ScriptType,
    inputs: usize,
    outputs: usize,
    sats_per_vbyte: f64,
) -> u64 {
    (estimated_vbytes(script_type, inputs, outputs) as f64 * sats_per_vbyte).ceil() as u64
}

/// The same question for a transaction already built: who was paid, how much,
/// and what came back.
///
/// A fee bump starts from a payment somebody made earlier rather than from a
/// form, so the recipient has to be read back off it. The first output that is
/// not ours is the payment; ours is change.
pub(super) fn split_outputs_of(
    tx: &bdk_wallet::bitcoin::Transaction,
    wallet: &Wallet,
) -> ((String, Amount), Option<Amount>) {
    let network = wallet.network();
    let mut paid = None;
    let mut change = Amount::ZERO;

    for out in &tx.output {
        if wallet.is_mine(out.script_pubkey.clone()) {
            change += out.value;
            continue;
        }
        if paid.is_none() {
            let address = bdk_wallet::bitcoin::Address::from_script(&out.script_pubkey, network)
                .map(|address| address.to_string())
                .unwrap_or_else(|_| "an unusual script, not an address".into());
            paid = Some((address, out.value));
        }
    }

    (
        // A payment with no outputs of its own is a self-send; naming it that
        // way beats an empty string where an address belongs.
        paid.unwrap_or_else(|| ("yourself".to_string(), Amount::ZERO)),
        (change > Amount::ZERO).then_some(change),
    )
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

    const MAINNET: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

    #[test]
    fn a_payment_uri_is_accepted_where_an_address_is() {
        let uri = format!("bitcoin:{}?amount=0.1&label=Alice", MAINNET);
        let address = parse_address(&uri, Network::Bitcoin).unwrap();
        assert_eq!(address.to_string(), MAINNET);
    }

    #[test]
    fn a_uri_for_the_wrong_network_is_still_refused() {
        // The network check is the one that must not be softened by unpacking
        // a URI first.
        let uri = format!("bitcoin:{MAINNET}");
        let err = parse_address(&uri, Network::Signet)
            .unwrap_err()
            .to_string();
        assert!(err.contains("different network"), "{err}");
    }

    #[test]
    fn a_mainnet_address_is_refused_on_signet() {
        let err = parse_address(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
            Network::Signet,
        )
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
            check_signer(
                &fresh,
                wallet
                    .public_descriptor(KeychainKind::External)
                    .descriptor_id(),
            )
            .unwrap();
            assert!(
                sign(&fresh, &mut psbt).unwrap(),
                "{script_type} did not finalize"
            );
            psbt.clone()
                .extract_tx()
                .expect("could not extract the signed transaction");
        }
    }

    /// Change from a transaction still waiting for a block is not available
    /// to spend, however tempting the balance looks.
    /// Choosing coins has to mean *only* those coins. BDK will happily treat
    /// a manual selection as a starting point and add more to cover the
    /// amount, which would silently undo the one decision this screen exists
    /// to let somebody make — and link coins they had deliberately kept apart.
    #[test]
    fn a_fee_estimate_is_close_to_what_a_real_transaction_costs() {
        // One input, two outputs, on each script type. The numbers are the
        // standard sizes; what matters is that they are the right order and
        // never optimistic, since an estimate that rounds down says "enough"
        // and then fails to build.
        assert_eq!(estimated_vbytes(ScriptType::Taproot, 1, 2), 11 + 58 + 86);
        assert_eq!(
            estimated_vbytes(ScriptType::NativeSegwit, 1, 2),
            11 + 68 + 62
        );
        assert_eq!(estimated_vbytes(ScriptType::Legacy, 1, 2), 10 + 148 + 68);

        // Taproot spends smaller than native segwit, which spends smaller than
        // legacy. If that ever inverts, the constants are wrong.
        let taproot = estimated_vbytes(ScriptType::Taproot, 3, 2);
        let segwit = estimated_vbytes(ScriptType::NativeSegwit, 3, 2);
        let legacy = estimated_vbytes(ScriptType::Legacy, 3, 2);
        assert!(
            taproot < segwit && segwit < legacy,
            "{taproot} {segwit} {legacy}"
        );

        // Each extra coin costs another input, which is the whole reason
        // choosing more coins costs more money.
        let one = estimated_vbytes(ScriptType::Taproot, 1, 2);
        let two = estimated_vbytes(ScriptType::Taproot, 2, 2);
        assert_eq!(two - one, 58);

        // Rounded up, never down.
        assert_eq!(estimated_fee(ScriptType::Taproot, 1, 2, 1.0), 155);
        assert_eq!(
            estimated_fee(ScriptType::Taproot, 1, 2, 1.5),
            233,
            "232.5 rounds up"
        );
    }

    /// A replacement spends the same coins as the payment it replaces — that
    /// is what makes them mutually exclusive, and what makes it safe when the
    /// original wins the race.
    #[test]
    fn a_replacement_spends_the_same_coins_and_pays_more() {
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let mut wallet = hd_signer(xprv, ScriptType::Taproot, network).unwrap();

        let funded = wallet.reveal_next_address(KeychainKind::External).address;
        let funding = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
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
                value: Amount::from_sat(200_000),
                script_pubkey: funded.script_pubkey(),
            }],
        };
        wallet.apply_unconfirmed_txs([(funding, 0u64)]);

        let to = parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
        let mut builder = wallet.build_tx();
        builder.add_recipient(to.script_pubkey(), Amount::from_sat(50_000));
        builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
        let mut psbt = builder.finish().unwrap();
        assert!(wallet.sign(&mut psbt, Default::default()).unwrap());
        let original = psbt.extract_tx().unwrap();
        let original_txid = original.compute_txid();
        let original_fee = wallet.calculate_fee(&original).unwrap();
        let spent: Vec<OutPoint> = original
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        wallet.apply_unconfirmed_txs([(original, 0u64)]);

        // Every payment Sieve makes signals BIP-125, because that is BDK's
        // default sequence. If that ever changes, nothing can be bumped.
        assert!(
            wallet
                .get_tx(original_txid)
                .unwrap()
                .tx_node
                .tx
                .input
                .iter()
                .any(|i| i.sequence.is_rbf()),
            "payments must signal that they can be replaced"
        );

        let replacement = {
            let mut builder = wallet.build_fee_bump(original_txid).unwrap();
            builder.fee_rate(FeeRate::from_sat_per_vb(10).unwrap());
            builder.finish().unwrap()
        };

        let replaced: Vec<OutPoint> = replacement
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        assert_eq!(replaced, spent, "a replacement must spend the same coins");
        assert!(
            replacement.fee().unwrap() > original_fee,
            "a replacement must pay more than what it replaces"
        );
        assert_ne!(
            replacement.unsigned_tx.compute_txid(),
            original_txid,
            "the replacement is a different transaction"
        );
    }

    #[test]
    fn chosen_coins_are_the_only_ones_spent() {
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let mut wallet = hd_signer(xprv, ScriptType::Taproot, network).unwrap();

        // Three separate coins, on three separate addresses, as three
        // separate payments would arrive.
        let mut outpoints = Vec::new();
        for (index, sats) in [50_000u64, 40_000, 30_000].into_iter().enumerate() {
            let funded = wallet.reveal_next_address(KeychainKind::External).address;
            let funding = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(
                        format!("{:064x}", index + 1).parse::<Txid>().unwrap(),
                        0,
                    ),
                    ..Default::default()
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(sats),
                    script_pubkey: funded.script_pubkey(),
                }],
            };
            outpoints.push(OutPoint::new(funding.compute_txid(), 0));
            wallet.apply_unconfirmed_txs([(funding, index as u64)]);
        }

        let to = parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
        let chosen = vec![outpoints[1]];

        let mut builder = wallet.build_tx();
        builder.add_utxos(&chosen).unwrap();
        builder.manually_selected_only();
        builder.add_recipient(to.script_pubkey(), Amount::from_sat(20_000));
        builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
        let psbt = builder.finish().expect("40,000 covers 20,000 and the fee");

        let spent: Vec<OutPoint> = psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        assert_eq!(spent, chosen, "only the chosen coin may be spent");

        // And a selection that cannot cover the payment fails rather than
        // quietly reaching for another coin.
        let mut builder = wallet.build_tx();
        builder.add_utxos(&[outpoints[2]]).unwrap();
        builder.manually_selected_only();
        builder.add_recipient(to.script_pubkey(), Amount::from_sat(90_000));
        builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
        assert!(
            builder.finish().is_err(),
            "a short selection must fail, not top itself up from elsewhere"
        );
    }

    #[test]
    fn unconfirmed_coins_are_not_spent() {
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let mut wallet = hd_signer(xprv, ScriptType::Taproot, network).unwrap();

        let funded = wallet.reveal_next_address(KeychainKind::External).address;
        let funding = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
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

        let unconfirmed = unconfirmed_outpoints(&wallet);
        assert_eq!(unconfirmed.len(), 1, "the funding should be pending");

        let to = parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
        let mut builder = wallet.build_tx();
        builder.unspendable(unconfirmed);
        builder.add_recipient(to.script_pubkey(), Amount::from_sat(20_000));
        builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
        assert!(
            builder.finish().is_err(),
            "a pending coin was spent when it should not have been"
        );
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
            watching
                .public_descriptor(KeychainKind::External)
                .descriptor_id(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("passphrase"), "{err}");
    }
}
