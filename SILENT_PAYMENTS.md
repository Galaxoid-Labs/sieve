# Silent payments (BIP-352)

Research, not a plan. Nothing here exists.

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

The work:

- **`silentpayments` 0.6.0** on crates.io, MIT, [cygnet3/spdk](https://github.com/cygnet3/spdk).
- **`send::parse_address` learns `sp1…`**, alongside the BIP-21 unpacking it
  already does.
- **Ordering changes.** The output cannot be derived until the inputs are
  chosen, because the shared secret depends on which coins are spent. Today
  `Sending::Exact` hands BDK a `script_pubkey` before selection happens. Coin
  control makes that ordering visible rather than hidden, which helps: with
  coins chosen the inputs are known before anything is built.
- **Not every input qualifies.** BIP-352 counts P2TR, P2WPKH, P2SH-P2WPKH and
  P2PKH; a transaction spending a SegWit v>1 input is excluded entirely. Sieve
  watches exactly the four standard paths, so the common case is fine, and the
  uncommon one needs a clear refusal rather than a silent failure.
- **Hardware signing needs BIP-376**, the PSBTv2 fields for tweak data. Not a
  concern until device signing exists at all.

This would put Sieve alongside Electrum's sender plugin, and ahead of most
wallets.

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

**Build sending when somebody wants it.** Contained, no server, no change to
the privacy claims, and it means Sieve can pay anyone publishing an SP address.

**Leave receiving.** Not because it is hard — the arithmetic is a small crate
and the filters already work — but because the only way to get the tweak data
today is a server, and that trade is bad while almost nobody can pay you at a
silent payment address anyway. Revisit when Core's index lands *and* there is
a way to serve it over P2P; at that point receiving becomes as private as the
rest of the wallet and is worth building properly.

**Who supports it today**: Sparrow receives (2.5.0), Electrum sends via a
plugin, Nunchuk and BitBox02 have support. Sending-only would be ordinary
company rather than a gap.

## Sources

- [BIP-352](https://github.com/bitcoin/bips/blob/master/bip-0352.mediawiki)
- [Light client specification](https://github.com/setavenger/BIP0352-light-client-specification)
- [BlindBit Oracle](https://github.com/setavenger/blindbit-oracle)
- [Bitcoin Optech: silent payments](https://bitcoinops.org/en/topics/silent-payments/)
- [`silentpayments` crate](https://crates.io/crates/silentpayments)
