# Silent payments (BIP-352)

Sending has a plan — "Implementing sending" below, decided and not yet built.
Receiving is still research, and the reason is at the bottom. Nothing here
exists in the code yet.

A silent payment address is published once — on a website, in an email
signature, on a business card — and everybody who pays it creates a different
on-chain output. No interaction with the recipient, no address reuse, and
nothing on the chain linking the payments to each other or to the published
address.

For a wallet built to avoid linking coins, that is the right shape. **Sending
fits Sieve's design and could be built. Receiving cannot, and the reason is
structural rather than a matter of effort.**

## How it works, briefly

The sender sums the private keys of the inputs being spent, and combines that
with the receiver's scan key:

```
input_hash          = hash(smallest_outpoint || A)     where A = a·G
ecdh_shared_secret  = input_hash · a · B_scan
P_k                 = B_spend + hash(ecdh_shared_secret || k)·G
```

`P_k` is an ordinary BIP-341 taproot output. Nothing about the transaction
announces itself; a silent payment is indistinguishable from any other taproot
spend.

The receiver runs the same arithmetic from the other side, using their scan
*private* key and the sum of the transaction's input *public* keys:

```
ecdh_shared_secret  = input_hash · b_scan · A
```

Which is where the trouble starts.

## Sending — buildable

Everything the sender needs, Sieve already has at the moment of signing: the
private keys of the chosen inputs, the recipient's address, and the smallest
outpoint. No server is involved at any point, and the resulting transaction is
an ordinary taproot payment.

This would put Sieve alongside Electrum's sender plugin, and ahead of most
wallets. What it takes is below.

## Implementing sending

Decided, not built. Written down because the ordering is the whole difficulty
and it is not obvious from the outside.

### The problem, stated exactly

`P_k` depends on the **private** keys of the inputs being spent. Sieve builds
payments watch-only — `node::plan` holds no key material and asks for no
password, and CLAUDE.md says why: *browsing balances and building PSBTs must
not require an unlock.* Every other payment type gets its output script from
the address, before a single coin is selected; a silent payment cannot.

So the output script is not known when the transaction is built, and the
transaction cannot be built after the output is known. That circle has to be
cut somewhere.

### How it is cut: a placeholder, swapped before signing

**A silent payment output is always P2TR**, and a P2TR output is a fixed 34
bytes whatever key is in it. So its *size* is known even when its *content* is
not — which means the fee, the coin selection and the change are all
computable without any key material.

**Stage one, in `node::plan`, watch-only as today.** A silent destination adds
a placeholder P2TR recipient of the correct size. Selection, fee and change
proceed exactly as for any other taproot payment, and the review screen is
honest about every number on it because none of them depend on `P_k`.

**Stage two, at signing, where the seed is already open.** `AppMsg::SendNow`
decrypts the vault to sign. Before it signs: read the inputs *from the PSBT*,
derive their private keys, compute `P_k`, and replace the placeholder script.
Then sign. The signature commits to the real output, and nothing before the
signature is trusted.

### The invariant, because the failure is unrecoverable

A wrong `P_k` is not a failed payment. It is a valid taproot output that
**nobody has the key for** — the coins are gone, the transaction confirms, and
nothing anywhere reports a problem. That is a different class of bug from the
rest of the send path, and it is the reason for each of these:

1. **Derive from the PSBT, never from the `Draft`.** The shared secret depends
   on which coins are actually spent. The `Draft` is what was *asked for*; the
   PSBT is what was *built*. When automatic selection is used those differ, and
   using the wrong one produces a plausible, unspendable output.
2. **Exactly one placeholder, checked.** Zero means the swap already happened
   or the PSBT is not the one that was planned. More than one is ambiguous.
   Either is a refusal, not a guess.
3. **No placeholder survives to broadcast.** `finalize_and_send` refuses a
   transaction still carrying one. That gate exists anyway as the last thing
   before the network, and it is the right place for a backstop that must never
   fire.
4. **The fee does not move**, because the placeholder and the real output are
   the same size. If a future change makes them differ, the fee shown on the
   review screen stops being the fee paid — so the sizes being equal is an
   assertion rather than an observation.

### Input eligibility, refused early

BIP-352 counts P2TR, P2WPKH, P2SH-P2WPKH and P2PKH. A transaction spending any
SegWit v>1 input is excluded outright. Sieve watches exactly the four standard
paths, so the ordinary case is fine.

**Checked at plan time, not at signing.** After `builder.finish()` the input
set is known, which is before the review screen — so an ineligible coin is
refused while somebody can still change the selection, rather than after they
have typed their password. The message names the coin.

### Hardware wallets are refused, and it is not a gap in this work

A device cannot compute `P_k`: it needs the sum of the input private keys,
which is exactly what a hardware wallet exists not to give up. **BIP-376**
defines the PSBT fields that let a device do the derivation itself, and
`async-hwi` does not implement them.

So a device-backed wallet refuses a silent payment **before the form is
drawn**, naming BIP-376 — the same shape as every other refusal in the send
flow, which says what is missing rather than failing at the last step. This is
new since the paragraph above was first written: device signing did not exist
then, and "not a concern until it does" has expired.

### The pieces

- **`silentpayments` 0.7.0** on crates.io, MIT,
  [cygnet3/spdk](https://github.com/cygnet3/spdk) — features `sending` and
  `encode` only. (0.6.0 in an earlier draft of this file; the API split into
  per-direction features since.)
- **A `Destination` rather than an `Address`.** `send::parse_address` returns
  on-chain or silent, and `Payee` carries that. Two kinds of destination in one
  `Address` field is how the placeholder would end up somewhere it should not
  be.
- **Network is in the address.** `sp1…` is mainnet, `tsp1…` is a test chain, so
  a wrong-network silent address is refused by the same rule that already
  refuses a wrong-network `bc1…`.
- **BIP-21.** `uri::parse` already unpacks payment requests; a silent address
  can arrive inside one.
- **`k = 0`.** One payee per address means the first output index. Paying the
  same silent address twice in one transaction would need `k` to increment —
  not built, and worth refusing explicitly rather than silently producing two
  identical outputs.

### Tests

- **BIP-352's own vectors** for the derivation. This is arithmetic where
  reviewing the code proves nothing and matching the vectors proves everything.
- **A PSBT still holding the placeholder is refused** — by the signer, and
  again by `finalize_and_send`.
- **Mutating the `Draft` after planning changes nothing**, which is what pins
  rule 1 above: the derivation reads the PSBT.
- **Placeholder and real output are the same size**, so the reviewed fee is the
  paid fee.
- **An ineligible input is refused at plan time**, naming the coin.

### Not in this work

Receiving — see below, and it is not a matter of effort. Signing on a device.
More than one silent payee in a transaction. Labelled addresses, which are a
receiving-side feature.

## Receiving — the wall

The receiver needs `A`, the sum of the input public keys, for **every eligible
transaction in every block**. A light client cannot compute it: the input
public keys live in the *previous* transactions' outputs, and a wallet that
syncs by compact block filters has no prevouts. This is the one thing BIP-352
needs that BIP-157/158 structurally cannot provide.

So the 33 bytes per eligible transaction — the "tweak data" — must come from
somewhere else, and there are exactly two somewheres:

**A full node with a silent-payments index.** Bitcoin Core PR #28241, still
unmerged as of this writing, and even when it lands there is no P2P message to
serve it. A node could compute it for its own wallet; it could not hand it to
Sieve over the network the way it hands over filters.

**A dedicated oracle**, such as [BlindBit](https://github.com/setavenger/blindbit-oracle),
over HTTP. The [light client specification](https://github.com/setavenger/BIP0352-light-client-specification)
is written around exactly this.

### What an oracle would cost

Less than it sounds, and still too much:

- **It never learns which outputs are yours.** Tweaks are fetched for every
  block regardless of what is in them. The spec puts it as leaking "which
  blocks you care about, not which coins you own" — a far better position than
  an Electrum server, which is told your addresses outright.
- **But it is a server**, over HTTP, not a Bitcoin peer. It learns your IP,
  when you sync, and that you use silent payments. Tor would hide the first.
- **And the README says "no server is ever asked."** That sentence is the
  clearest claim this wallet makes. Adding an oracle would mean qualifying it,
  and a qualified version of that claim is worth much less than the plain one.

### One thing that is easier than expected

The light-client spec asks for **taproot-only** BIP-158 filters, which are a
different filter type from the ones kyoto downloads. They are not required:
a basic filter contains every output `scriptPubKey`, so a computed `P_k`
matches one. Taproot-only filters are smaller, not necessary. **The filters
Sieve already has would work — only the tweaks are missing.**

### And one that is harder

An ECDH multiplication per eligible transaction per block, across the whole
scan range. A wallet with a 2019 birthday is several hundred thousand blocks
of elliptic curve arithmetic before it can show a balance. The spec's dust
limit trims up to 85% of it, which makes it tractable rather than cheap.

## Where this leaves it

**Sending is decided and planned** — "Implementing sending" above. Contained,
no server, no change to the privacy claims, and it means Sieve can pay anyone
publishing an SP address. The one thing it is not is a small change to the send
path: the output cannot be derived until the coins are chosen, and it cannot be
derived at all without the seed, so it is the first payment type whose script
is filled in between building and signing.

**Leave receiving.** Not because it is hard — the arithmetic is a small crate
and the filters already work — but because the only way to get the tweak data
today is a server, and that trade is bad while almost nobody can pay you at a
silent payment address anyway. Revisit when Core's index lands *and* there is
a way to serve it over P2P; at that point receiving becomes as private as the
rest of the wallet and is worth building properly.

**Who supports it today**: Sparrow receives (2.5.0), Electrum sends via a
plugin, Nunchuk and BitBox02 have support. Sending-only is ordinary company
rather than a gap.

Two facts in this file have already gone stale once — the crate version, and
"hardware signing is not a concern yet" — so anything here about what other
software does is worth rechecking rather than quoting.

## Sources

- [BIP-352](https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki)
- [Light client specification](https://github.com/setavenger/BIP0352-light-client-specification)
- [BlindBit Oracle](https://github.com/setavenger/blindbit-oracle)
- [Bitcoin Optech: silent payments](https://bitcoinops.org/en/topics/silent-payments/)
- [`silentpayments` crate](https://crates.io/crates/silentpayments)
