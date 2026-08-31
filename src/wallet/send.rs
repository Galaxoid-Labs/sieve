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
    /// Who is being paid. One is the ordinary case; more than one is a single
    /// transaction with several outputs, which costs less in fees than the
    /// same payments made separately and tells anybody reading the chain that
    /// the same person made all of them.
    pub payees: Vec<Payee>,
    /// Bytes to publish in an `OP_RETURN` output, if any. Exactly what was
    /// typed, as UTF-8 — not trimmed, not normalised. Altering a payload
    /// somebody is about to make permanent is the one thing this must not do.
    ///
    /// One, not a list. More than one data output is standard under Core 30's
    /// relaxed policy and is refused by everything older, so building a second
    /// would be offering somebody a transaction that might never relay.
    pub data: Option<Vec<u8>>,
    pub fee_rate: FeeRate,
    /// Which coins to spend. Empty means "choose for me", which is what most
    /// payments should be; a non-empty list means exactly these and nothing
    /// else, because the point of choosing is that nothing is added behind
    /// your back.
    pub coins: Vec<bdk_wallet::bitcoin::OutPoint>,
}

/// The most data an `OP_RETURN` can carry and still relay everywhere.
///
/// Not a consensus rule, and Bitcoin Core 30 relaxed its own default well past
/// it — but the network is mixed, Knots restricts data carriage by default and
/// older nodes still enforce the old cap. Eighty is what relays everywhere.
/// Above it is a bet on which peers you happen to have, and losing that bet is
/// silent: a node that will not relay simply does not.
///
/// It cannot be discovered rather than assumed, which is worth knowing before
/// anybody tries to make it dynamic. Relay policy is not gossiped: the version
/// handshake carries services, protocol version, user agent and the relay
/// flag, and the only policy value a node volunteers anywhere is its minimum
/// relay fee, through BIP-133 `feefilter`. Guessing from a peer's software
/// would need its user agent, and `bip157` surfaces only `ServiceFlags`. Nor
/// can it be learned by trying, since BIP-61 `reject` messages are long gone
/// from Core and an over-limit transaction simply produces silence.
pub const MAX_DATA: usize = 80;

/// Everything a transaction publishes, in the order the outputs appear.
///
/// A list, though Sieve only ever builds one: this reads transactions other
/// software made, and Bitcoin Core 30 relaxed the rule that allowed only a
/// single data output. Showing the first and dropping the rest would be a
/// quiet half-truth on a screen whose job is saying what a transaction did.
pub(super) fn data_in(tx: &bdk_wallet::bitcoin::Transaction) -> Vec<Vec<u8>> {
    tx.output
        .iter()
        .filter(|out| out.script_pubkey.is_op_return())
        .filter_map(|out| {
            out.script_pubkey
                .instructions()
                .flatten()
                .find_map(|instruction| match instruction {
                    bdk_wallet::bitcoin::script::Instruction::PushBytes(bytes) => {
                        Some(bytes.as_bytes().to_vec())
                    }
                    _ => None,
                })
        })
        .collect()
}

/// One recipient of a payment.
#[derive(Debug, Clone)]
pub struct Payee {
    pub to: Address,
    pub amount: Sending,
}

/// A built, unsigned transaction, with the numbers to show before signing.
#[derive(Debug, Clone)]
pub struct Plan {
    pub psbt: Psbt,
    pub from: ScriptType,
    /// Who is paid and what each receives. Not the same as what was typed
    /// when sending everything, since the fee comes out of it.
    pub payees: Vec<(String, Amount)>,
    pub fee: Amount,
    /// Returning to this wallet on the change chain, if there is any.
    pub change: Option<Amount>,
    /// The payment this one replaces, when it is a fee bump. Both spend the
    /// same coins, so only one of them can ever confirm.
    pub replaces: Option<String>,
    /// What that payment was paying, so the increase can be shown rather than
    /// only the new figure.
    pub was_fee: Option<u64>,
    /// What this transaction publishes. Kept so the review dialog can show
    /// it: it is about to become permanent and public, and this is the last
    /// screen on which it can be read before that. A list because a
    /// replacement rebuilds whatever the original carried, which Sieve did not
    /// necessarily build.
    pub data: Vec<Vec<u8>>,
    /// Whether this replacement pays nobody — the same coins back to this
    /// wallet, to call the original off. Every screen has to say so: the
    /// numbers alone read as a payment to yourself, and `total()` would
    /// otherwise claim money is leaving when only the fee is.
    pub cancels: bool,
}

impl Plan {
    /// What everybody being paid receives, together.
    pub fn spend(&self) -> Amount {
        self.payees
            .iter()
            .map(|(_, amount)| *amount)
            .fold(Amount::ZERO, |total, amount| total + amount)
    }

    /// Who to name when there is only room for one — a toast, a row, a
    /// sentence. With several, the count is the honest summary: naming the
    /// first and silently dropping the rest would be worse than not naming
    /// anybody.
    pub fn to(&self) -> String {
        match self.payees.as_slice() {
            [(address, _)] => address.clone(),
            [] => "nobody".into(),
            many => format!("{} recipients", many.len()),
        }
    }

    /// What leaves the wallet: the payments plus the fee.
    ///
    /// A cancellation pays nobody, so the fee is the whole of it.
    pub fn total(&self) -> Amount {
        if self.cancels {
            self.fee
        } else {
            self.spend() + self.fee
        }
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

/// names the internal state rather than what happened.
pub(super) fn explain_bump(error: bdk_wallet::error::BuildFeeBumpError) -> String {
    use bdk_wallet::error::BuildFeeBumpError as E;
    match error {
        E::TransactionConfirmed(_) => {
            "that payment is already in a block, so it cannot be replaced".into()
        }
        E::IrreplaceableTransaction(_) => {
            "that payment did not signal that it could be replaced, so the network will not \
             take a replacement for it"
                .into()
        }
        E::TransactionNotFound(_) => {
            "this wallet does not hold that payment, so it cannot rebuild it".into()
        }
        E::FeeRateUnavailable => "the original payment's fee cannot be worked out".into(),
        other => format!("that payment cannot be replaced: {other}"),
    }
}

/// Rebuild an unconfirmed payment: at a higher fee, or paying nobody.
///
/// `back` is what separates the two. `None` keeps the original's recipients
/// and only raises the fee; `Some(script)` drops them and drains everything to
/// that script, which is a cancellation — the same coins, so the two conflict,
/// and only one can ever confirm.
///
/// The rate asked for is a floor, not the answer. BDK enforces only that a
/// replacement's *rate* beats the original's by a satoshi per virtual byte,
/// but the network's rule is about the *absolute* fee: a replacement must pay
/// what the original paid plus a satoshi for every virtual byte of its own
/// size. Those come to the same thing when the replacement is the same size,
/// which is why a fee bump has never tripped over it — and not when it is
/// smaller, which a cancellation always is, having one output where the
/// original had two. Paying under that is not an error anybody sees: every
/// node drops the transaction, and it looks exactly like being ignored. So the
/// rate is raised here until the fee clears the rule.
pub(super) fn build_replacement(
    wallet: &mut Wallet,
    txid: bdk_wallet::bitcoin::Txid,
    fee_rate: FeeRate,
    previous_fee: Amount,
    back: Option<ScriptBuf>,
) -> Result<Psbt> {
    let what = match back {
        Some(_) => "cancellation",
        None => "replacement",
    };
    let mut rate = fee_rate;

    // Twice is enough: raising the rate does not change the size, so the
    // second attempt pays what the first one worked out it was short of. The
    // extra rounds are slack, not a search.
    for _ in 0..4 {
        let psbt = {
            let mut builder = wallet
                .build_fee_bump(txid)
                .map_err(|e| anyhow!("{}", explain_bump(e)))?;
            if let Some(back) = back.clone() {
                // The original's outputs go, its inputs stay. BDK permits a
                // transaction with no recipients in exactly this case: a drain
                // address, and inputs that must be spent.
                builder.set_recipients(Vec::new());
                builder.drain_to(back);
            }
            builder.fee_rate(rate);
            match builder.finish() {
                Ok(psbt) => psbt,
                // BDK works the original's rate out to a fraction of a
                // satoshi, which the screen that offered a floor rounded. Ask
                // for a rate a hundredth under what it wants and it refuses,
                // having just said what it wants — so take it rather than
                // handing somebody an arithmetic complaint about a number they
                // did not choose.
                Err(bdk_wallet::error::CreateTxError::FeeRateTooLow { required })
                    if required > rate =>
                {
                    rate = required;
                    continue;
                }
                Err(e) => bail!("the {what} could not be built: {e}"),
            }
        };

        let fee = psbt
            .fee()
            .map_err(|e| anyhow!("the {what} has no readable fee: {e}"))?;
        let vsize = psbt.unsigned_tx.vsize() as u64;
        // What the network asks: the original's fee, plus a satoshi per
        // virtual byte of the replacement.
        let required = previous_fee + Amount::from_sat(vsize);
        if fee >= required {
            return Ok(psbt);
        }

        let needed = required.to_sat().div_ceil(vsize) + 1;
        let Some(higher) = FeeRate::from_sat_per_vb(needed) else {
            break;
        };
        if higher <= rate {
            break;
        }
        rate = higher;
    }

    bail!("the {what} cannot be built at a fee this network would relay")
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
) -> (Vec<(String, Amount)>, Option<Amount>) {
    let network = wallet.network();
    let mut paid = Vec::new();
    let mut change = Amount::ZERO;

    for out in &tx.output {
        // A data output is not a recipient. Left in, it reaches the review
        // dialog — the one screen whose job is letting somebody check what
        // they are signing — as "an unusual script, not an address" being paid
        // nothing.
        if out.script_pubkey.is_op_return() {
            continue;
        }
        if wallet.is_mine(out.script_pubkey.clone()) {
            change += out.value;
            continue;
        }
        let address = bdk_wallet::bitcoin::Address::from_script(&out.script_pubkey, network)
            .map(|address| address.to_string())
            .unwrap_or_else(|_| "an unusual script, not an address".into());
        paid.push((address, out.value));
    }

    // A payment with no outputs of its own is a self-send; naming it that way
    // beats an empty list where a recipient belongs.
    if paid.is_empty() {
        paid.push(("yourself".to_string(), Amount::ZERO));
    }
    (paid, (change > Amount::ZERO).then_some(change))
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

            let (payees, change) = split_outputs_of(&psbt.unsigned_tx, &wallet);
            assert_eq!(payees.len(), 1, "{script_type}: {payees:?}");
            assert_eq!(payees[0].1, Amount::from_sat(20_000));
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
    /// The claim a cancellation makes: the same coins, none of them going
    /// anywhere but back, and the original's recipient paid nothing.
    #[test]
    fn a_cancellation_pays_nobody_and_spends_the_same_coins() {
        let network = Network::Signet;
        let script_type = ScriptType::NativeSegwit;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let mut wallet = hd_signer(xprv, script_type, network).unwrap();

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

        // A payment out, unconfirmed, of the kind somebody would want back.
        let stranger =
            parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
        let payment = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(stranger.script_pubkey(), Amount::from_sat(40_000));
            builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
            builder.finish().unwrap().unsigned_tx
        };
        let original = payment.compute_txid();
        let spent: Vec<OutPoint> = payment.input.iter().map(|i| i.previous_output).collect();
        wallet.apply_unconfirmed_txs([(payment, 1u64)]);

        let back = wallet
            .reveal_next_address(KeychainKind::Internal)
            .address
            .script_pubkey();
        let original_fee = wallet
            .calculate_fee(&wallet.get_tx(original).unwrap().tx_node.tx)
            .unwrap();
        let psbt = build_replacement(
            &mut wallet,
            original,
            FeeRate::from_sat_per_vb(10).unwrap(),
            original_fee,
            Some(back.clone()),
        )
        .expect("a cancellation must build");
        let fee = psbt.fee().unwrap();
        let cancellation = psbt.unsigned_tx;

        // The same coins, which is the only reason the two conflict at all.
        let now: Vec<OutPoint> = cancellation
            .input
            .iter()
            .map(|i| i.previous_output)
            .collect();
        assert_eq!(now, spent, "a cancellation must spend the original's coins");

        // And nothing paid to the person the original was paying.
        assert!(
            !cancellation
                .output
                .iter()
                .any(|out| out.script_pubkey == stranger.script_pubkey()),
            "the original's recipient must be paid nothing"
        );
        assert!(
            cancellation
                .output
                .iter()
                .all(|out| wallet.is_mine(out.script_pubkey.clone())),
            "every output must come back to this wallet"
        );

        // The fee is the whole cost: everything else returns.
        let put_in = Amount::from_sat(100_000);
        let came_back: Amount = cancellation.output.iter().map(|out| out.value).sum();
        assert_eq!(came_back + fee, put_in, "only the fee may leave");
        assert!(
            fee > Amount::from_sat(0),
            "a replacement has to outbid the original"
        );
    }

    #[test]
    fn a_cancellation_pays_what_the_network_asks_even_at_a_low_rate() {
        // BDK enforces only that the replacement's *rate* beats the
        // original's by a satoshi per virtual byte. The network's rule is
        // about the absolute fee, and a cancellation is smaller than what it
        // replaces — one output where there were two — so the rate that
        // satisfies BDK can still leave the fee short. Nothing reports that:
        // every node drops the transaction and it looks like being ignored.
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let mut wallet = hd_signer(xprv, ScriptType::NativeSegwit, network).unwrap();

        let funded = wallet.reveal_next_address(KeychainKind::External).address;
        wallet.apply_unconfirmed_txs([(
            Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(
                        "0000000000000000000000000000000000000000000000000000000000000002"
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
            },
            0u64,
        )]);

        let stranger =
            parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
        let payment = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(stranger.script_pubkey(), Amount::from_sat(40_000));
            builder.fee_rate(FeeRate::from_sat_per_vb(5).unwrap());
            builder.finish().unwrap().unsigned_tx
        };
        let original = payment.compute_txid();
        wallet.apply_unconfirmed_txs([(payment, 1u64)]);
        let was = wallet
            .calculate_fee(&wallet.get_tx(original).unwrap().tx_node.tx)
            .unwrap();

        let back = wallet
            .reveal_next_address(KeychainKind::Internal)
            .address
            .script_pubkey();

        // One satoshi per virtual byte over the original: exactly what BDK
        // accepts, and on a smaller transaction not necessarily enough.
        let psbt = build_replacement(
            &mut wallet,
            original,
            FeeRate::from_sat_per_vb(6).unwrap(),
            was,
            Some(back),
        )
        .expect("a cancellation must build");

        let fee = psbt.fee().unwrap();
        let vsize = psbt.unsigned_tx.vsize() as u64;
        assert!(
            fee >= was + Amount::from_sat(vsize),
            "a replacement paying {fee} against an original paying {was} over {vsize} vB \
             would be dropped by every node"
        );
    }

    #[test]
    fn every_recipient_is_read_back_off_the_transaction() {
        // The screens are built from what the transaction actually pays, not
        // from what was typed. Keeping only the first recipient would show a
        // payment to three people as a payment to one — and it is the review
        // screen that would be lying.
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let mut wallet = hd_signer(xprv, ScriptType::NativeSegwit, network).unwrap();

        let funded = wallet.reveal_next_address(KeychainKind::External).address;
        wallet.apply_unconfirmed_txs([(
            Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(
                        "0000000000000000000000000000000000000000000000000000000000000003"
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
            },
            0u64,
        )]);

        let one = parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
        // Derived rather than written down, so it is certainly valid and
        // certainly not this wallet's: a different passphrase is a different
        // seed.
        let two = {
            let theirs =
                super::super::xprv_from_mnemonic(PHRASE, Some("someone else"), network).unwrap();
            hd_signer(theirs, ScriptType::NativeSegwit, network)
                .unwrap()
                .peek_address(KeychainKind::External, 0)
                .address
        };
        let psbt = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(one.script_pubkey(), Amount::from_sat(20_000));
            builder.add_recipient(two.script_pubkey(), Amount::from_sat(30_000));
            builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
            builder.finish().unwrap()
        };

        let (payees, change) = split_outputs_of(&psbt.unsigned_tx, &wallet);
        assert_eq!(payees.len(), 2, "{payees:?}");
        let paid: Amount = payees
            .iter()
            .map(|(_, amount)| *amount)
            .fold(Amount::ZERO, |a, b| a + b);
        assert_eq!(paid, Amount::from_sat(50_000));
        assert!(payees.iter().any(|(a, _)| *a == one.to_string()));
        assert!(payees.iter().any(|(a, _)| *a == two.to_string()));
        assert!(change.is_some(), "this payment kept change");
    }

    #[test]
    fn one_line_names_a_recipient_or_counts_them() {
        // Naming the first of several and dropping the rest silently would be
        // worse than not naming anybody: it reads as a payment to one person.
        let plan = |payees: Vec<(String, Amount)>| Plan {
            psbt: Psbt::from_unsigned_tx(Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: Vec::new(),
                output: Vec::new(),
            })
            .unwrap(),
            from: ScriptType::NativeSegwit,
            payees,
            fee: Amount::from_sat(500),
            change: None,
            data: Vec::new(),
            replaces: None,
            was_fee: None,
            cancels: false,
        };

        let one = plan(vec![("bc1qsomeaddress".into(), Amount::from_sat(1_000))]);
        assert_eq!(one.to(), "bc1qsomeaddress");
        assert_eq!(one.spend(), Amount::from_sat(1_000));

        let many = plan(vec![
            ("bc1qone".into(), Amount::from_sat(1_000)),
            ("bc1qtwo".into(), Amount::from_sat(2_500)),
        ]);
        assert_eq!(many.to(), "2 recipients");
        assert_eq!(many.spend(), Amount::from_sat(3_500));
        assert_eq!(many.total(), Amount::from_sat(4_000));
    }

    #[test]
    fn a_data_output_is_not_a_recipient() {
        // Left in the payee split it reaches the review dialog as "an unusual
        // script, not an address" being paid nothing — on the one screen whose
        // whole job is letting somebody check what they are signing.
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let mut wallet = hd_signer(xprv, ScriptType::NativeSegwit, network).unwrap();
        let funded = wallet.reveal_next_address(KeychainKind::External).address;
        wallet.apply_unconfirmed_txs([(
            Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(
                        "0000000000000000000000000000000000000000000000000000000000000004"
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
            },
            0u64,
        )]);

        let message = b"sieve was here";
        let bytes: &bdk_wallet::bitcoin::script::PushBytes = message.as_slice().try_into().unwrap();

        // Data and nobody paid: the change is the only other output.
        let psbt = {
            let mut builder = wallet.build_tx();
            builder.add_data(&bytes);
            builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
            builder.finish().unwrap()
        };
        let (payees, change) = split_outputs_of(&psbt.unsigned_tx, &wallet);
        assert_eq!(
            payees,
            vec![("yourself".to_string(), Amount::ZERO)],
            "a data output must not be reported as somebody being paid"
        );
        assert!(change.is_some(), "the money came back");
        assert_eq!(data_in(&psbt.unsigned_tx), vec![message.to_vec()]);

        // And with somebody paid, they are the only payee.
        let stranger =
            parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network).unwrap();
        let psbt = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(stranger.script_pubkey(), Amount::from_sat(20_000));
            builder.add_data(&bytes);
            builder.fee_rate(FeeRate::from_sat_per_vb(2).unwrap());
            builder.finish().unwrap()
        };
        let (payees, _) = split_outputs_of(&psbt.unsigned_tx, &wallet);
        assert_eq!(payees.len(), 1, "{payees:?}");
        assert_eq!(payees[0].0, stranger.to_string());
        assert_eq!(payees[0].1, Amount::from_sat(20_000));
    }

    #[test]
    fn every_data_output_is_read_not_just_the_first() {
        // Sieve builds one, because a second is refused by everything older
        // than Core 30. But this reads transactions other software made, and
        // showing the first while dropping the rest is a quiet half-truth on
        // the screen that says what a transaction did.
        let script = |bytes: &[u8]| {
            let push: &bdk_wallet::bitcoin::script::PushBytes = bytes.try_into().unwrap();
            ScriptBuf::new_op_return(push)
        };
        let carrying_two = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: script(b"first"),
                },
                TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: parse_address(
                        "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
                        Network::Signet,
                    )
                    .unwrap()
                    .script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: script(b"second"),
                },
            ],
        };

        assert_eq!(
            data_in(&carrying_two),
            vec![b"first".to_vec(), b"second".to_vec()],
            "both, in the order the outputs appear"
        );
    }

    #[test]
    fn a_transaction_carrying_nothing_reports_no_data() {
        let network = Network::Signet;
        let xprv = super::super::xprv_from_mnemonic(PHRASE, None, network).unwrap();
        let wallet = hd_signer(xprv, ScriptType::NativeSegwit, network).unwrap();
        let _ = &wallet;
        let plain = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![TxOut {
                value: Amount::from_sat(1),
                script_pubkey: parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", network)
                    .unwrap()
                    .script_pubkey(),
            }],
        };
        assert!(data_in(&plain).is_empty());
    }

    #[test]
    fn a_cancellation_costs_only_its_fee() {
        // `total()` is what the screens call "leaving the wallet". For an
        // ordinary payment that is the amount plus the fee; for a
        // cancellation nothing is being paid, so claiming the amount leaves
        // would say the opposite of what is happening.
        let plan = |cancels: bool| Plan {
            psbt: Psbt::from_unsigned_tx(Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: Vec::new(),
                output: Vec::new(),
            })
            .unwrap(),
            from: ScriptType::NativeSegwit,
            payees: vec![(
                "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx".into(),
                Amount::from_sat(40_000),
            )],
            fee: Amount::from_sat(900),
            change: Some(Amount::from_sat(59_100)),
            data: Vec::new(),
            replaces: Some("an id".into()),
            was_fee: Some(300),
            cancels,
        };

        assert_eq!(plan(false).total(), Amount::from_sat(40_900));
        assert_eq!(plan(true).total(), Amount::from_sat(900));
    }

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
