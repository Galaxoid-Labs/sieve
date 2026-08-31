# Data outputs

A design, not an implementation. Nothing in this file exists yet, and the
privacy section below is a real argument against building it at all.

An `OP_RETURN` output carries up to eighty bytes of arbitrary data and can
never be spent. It is how a transaction says something rather than pays
somebody: a timestamp, a reference, a proof that a document existed on a date,
a protocol's own marker. It is the polite way to do it — an unspendable output
is not kept in any node's UTXO set, unlike the fake addresses people used to
stuff data into — but *not kept in memory* is not *goes away*. It is in the
block forever.

## Two shapes, one control

The distinction is not a mode to choose. It falls out of what the form holds:

- A recipient **and** data — a payment that carries a message.
- Data and **no** recipient — a transaction that pays nobody, with the change
  coming back and the fee as its whole cost.

One expander on the send form, one field, no switch. A separate "data-only"
screen would be the send form again with ninety per cent shared, and the two
would eventually disagree about fees or coin control.

## Privacy, which is the reason to think twice

Both shapes leak what every transaction leaks: the coins spent together are
declared as one owner's, and the change comes back to you. Coin control already
answers the first. What a data output adds is worse than it looks, and the
data-only shape is worse than the paying one — which is the opposite of what
anybody assumes.

**Both shapes** put a permanent, searchable index into your wallet on the
chain. Normally somebody needs an address or a transaction id to find your
transaction; every explorer indexes `OP_RETURN` data, so a string puts it
within reach of anyone who knows the string. If the bytes are anything a
counterparty also holds — an invoice number, an order reference, a hash whose
preimage they know — that person can find the transaction, and from it the
change output and the whole input cluster, having never learned one of your
addresses. A hash is not a hedge when somebody else can compute it.

They also make the transaction an unusual shape. Chain analysis clusters on
form — version, locktime, sequence, output types, ordering — and payments
carrying a data output are uncommon, so the presence of one shrinks the crowd
even when the bytes are random.

**A payment that carries data** at least keeps the change ambiguous. A third
party looking at two outputs has to guess which is yours, and that guess is a
heuristic that is sometimes wrong. The recipient always knew, of course: they
can see their own output, so the other is yours.

**A transaction that pays nobody removes the guess.** An `OP_RETURN` is
provably unspendable, so if it is the only output that is not change, then
every other output is certainly yours. Not probably. A data-only transaction is
a perfect change-address discloser, and if it spends several inputs it proves
in one step that those inputs and those outputs are one person — an inference
replaced by a proof. On top of that it is *authenticated*, since publishing
this way demonstrates control of the coins that paid for it, and it has no
cover story: a payment has an obvious reason to exist, while a transaction that
pays nobody exists only to carry its message, so it correlates cleanly with
whatever prompted it. Done repeatedly, it is a public, self-attested timeline
of one wallet's activity.

Two levers already exist and are the real mitigation:

- **Coin control.** Publishing should be a deliberate choice of which coin pays
  for it — one already fit to be associated with whatever the message carries.
- **Path isolation.** The four derivation paths are separate BDK wallets.
  Spending only from one keeps a data transaction off the others, and the
  change returns to the same path. Same seed, but nothing on chain joins them
  unless they are later spent together, which is what coin control prevents.

The warning on the screen should say the specific thing, and should change with
the form: with a recipient, that the data is permanent and public; without one,
that the transaction proves which outputs are yours. "Data on the blockchain is
permanent" is a sentence people skim. The other one they can act on.

## What the network will carry

Eighty bytes. Not because it is a consensus rule — it is not, and Bitcoin Core
30 relaxed its own default considerably — but because the network is mixed.
Knots restricts data carriage by default and older nodes still enforce the old
cap, so eighty is what relays everywhere and anything above it is a bet on
which peers you happen to have.

**The limit cannot be discovered from peers, and it is worth writing down why**
so nobody tries. Relay policy is not gossiped. The `version` handshake carries
services, protocol version, user agent and the relay flag, and nothing about
data carriage; the single policy value a node volunteers anywhere in the
protocol is its minimum relay fee, through BIP-133 `feefilter`. Even the weak
proxy is out of reach: guessing from a peer's software would need its user
agent, and `bip157` sets only *our* own — all Sieve is given per peer is
`ServiceFlags`.

Nor can it be learned by trying. A node that will not relay a transaction
simply does not; BIP-61 `reject` messages are long gone from Core. An
over-limit transaction produces silence, which looks exactly like being
ignored — the same failure shape as an underpaid replacement, and it took a
test to find that one.

So: cap at eighty and the question stops mattering.

## What BDK gives us

`TxBuilder::add_data` builds `OP_RETURN <data>` at zero value and adds it as a
recipient. The dust check exempts it explicitly — `!script_pubkey.is_op_return()`
in `create_tx` — so a zero-value output goes through unremarked. That is the
whole of the construction.

## What it touches

Built offline against a funded wallet, a data output with no payee at all:

```
inputs 1  outputs 2  vsize 107  fee 269 sat  (at 2 sat/vB)
  99731 sat  op_return=false  mine=true   OP_0 OP_PUSHBYTES_20 2f34aa1c…
      0 sat  op_return=true   mine=false  OP_RETURN OP_PUSHBYTES_14 …
```

Fourteen bytes cost about 25 vB. The rest comes straight back as change.

**`split_outputs_of` is the first thing to fix, not the last.** It treats every
output that is not the wallet's as somebody being paid, so the same run
reported:

```
payees=[("an unusual script, not an address", 0 SAT)] change=Some(99731 SAT)
```

That text would appear on the review dialog — the screen whose only job is
letting somebody check what they are about to sign — and in the fee-bump and
cancellation dialogs, which read the same function. Data outputs have to be
filtered out of the payee split before anything else is built.

Then:

- **The readiness rule.** `why_not_ready` demands a recipient and an amount.
  With data present, only the "nothing at all" case changes: paying nobody
  stops being a reason to block. A *half*-filled recipient still blocks, since
  an address with no amount is somebody mid-thought rather than a data-only
  transaction, and that difference is worth keeping.
- **Max needs a recipient.** "Everything" has nowhere to go without one, so it
  greys out and says why — the same shape as the rule for several recipients.
- **The review dialog** shows the data as text *and* as hex. Text alone hides
  what is really being written; hex alone is unreadable.
- **The transaction detail** should show the message on a transaction that
  carries one. It is the nicest part of the feature, and it is also the only
  place the data is ever readable again.
- **The activity row** will call a data-only transaction a self-send, because
  once the data output is filtered every remaining output is change and the
  split returns `("yourself", 0)`. Not wrong, but not what happened; the row
  should say the transaction carried data.

## Deferred

- **More than eighty bytes**, and more than one data output. Both are standard
  under Core 30's relaxed default and neither relays reliably across a mixed
  network. Revisit when the network is not mixed.
- **Reading data from other people's transactions.** Sieve only ever downloads
  blocks its filters matched, so it sees almost none of them, and a wallet is
  not an explorer.

## What already exists to build on

| Piece | Where | Note |
|---|---|---|
| Zero-value data output | `TxBuilder::add_data` | Dust check already exempts it |
| Several outputs in one payment | `Draft.payees`, `Plan.payees` | A data output is one more output |
| Splitting outputs for the screens | `wallet::send::split_outputs_of` | Needs the `OP_RETURN` filter |
| Choosing which coin pays | Coin control | The mitigation, already built |
| Warning copy that changes with the form | `ask_bump` does this for cancellations | Same pattern |
