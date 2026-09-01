# Partially signed transactions

**Half built.** Export exists; import does not, and is not being built yet — see
"Where this stopped" at the end for why.

PSBT is not primarily about multisig, which is the easy assumption to make about
it. It is about *the keys being somewhere else*, and for Sieve today that means
hardware wallets.

PSBT (BIP-174) is the format for *this payment needs a signature from somewhere
else*. Sieve has two reasons to want it, one now and one later:

- **Air-gapped signing.** A Coldcard on an SD card, a laptop that never touches
  the network, a Jade over QR. Today a wallet imported from a device can receive
  forever and never spend, and the send form stops at a page explaining why.
- **Multisig, eventually.** The import → sign → export loop below *is* a
  signing round. Nothing here should preclude it; see the last section.

The groundwork is already done and was not built for this: `plan()` constructs
a PSBT entirely watch-only, because building a payment needs public descriptors
and UTXOs and nothing else. Every number the send form shows — amount, fee,
change — is computed before any key is involved. What is missing is only the
part where the signature comes from elsewhere.

## The three flows

### Export unsigned — **built**

Build a payment in the send form exactly as now, then at the review step:
**Save unsigned…** writes the file, and **Copy as text** puts the same payment
on the clipboard as base64.

**Only on a watch-only wallet, where it replaces Send.** This was originally
written as "replaces Send rather than sitting beside it", and the first
implementation did both — replacing it on watch-only and offering it alongside
on a wallet that holds keys. That was wrong twice over. An unsigned payment file
is worth something exactly when something *else* holds the keys; on a wallet that
can sign it is a file somebody could have signed instead. And it turned the one
dialog that must be read correctly into four buttons.

Files are named `sieve-<txid>.psbt`. Signing a segwit or taproot input adds
witness data, which is not part of the txid — so for everything Sieve hands out
an address on, that name is what the payment will be called once broadcast, and
a card holding several says which is which.

**The file is read back and compared before Sieve reports success.** Not
ceremony: this file goes to a signer that is not this program, often on a card
carried to a machine with no way to ask for another copy. A short write, a full
disk or a card pulled early all produce a file that exists and will not parse.
Finding that out here costs milliseconds; finding it out there costs the trip.

### Import signed — not built

The file comes back with signatures on it. Sieve finalises it
(`Wallet::finalize_psbt`) and broadcasts through the existing
`Requester::submit_package`. No new broadcast path.

### Import anything — not built

A PSBT built somewhere else that this wallet can sign. Sieve shows what it
does, signs with the vault key on the existing password path, then either
broadcasts — if that signature completed it — or saves it back out for the next
signer. This is the general case, and the one that becomes multisig.

## Where it lives

- **Main menu → Open a payment file…**, beside About. Not inside the send form:
  an imported PSBT is not always something that started here.
- **Send form review → Save unsigned payment…**, which is where a payment built
  here naturally leaves.

## The review screen is the whole safety story

This is where the effort goes. **A PSBT is a stranger's claim about what a
transaction does. The only defence is computing the answer independently.**

```
This payment file                                  ‹ Back
┌──────────────────────────────────────────────────────┐
│  Pays out          0.00250000 BTC                    │
│  Fee               0.00000620 BTC · 2.0 sat/vB       │
│                                                      │
│  Paying                                              │
│    bc1qsomeone…                       0.00250000 BTC │
│  Coming back to you                                  │
│    bc1p… m/86'/0'/0'/1/12             0.00749380 BTC │
│                                                      │
│  Spends 2 of your coins · signed by 0 of 1           │
│                                                      │
│           [ Sign ]   [ Save a copy ]                 │
└──────────────────────────────────────────────────────┘
```

Every figure is recomputed from our own descriptors. None is read from the
file's claims about itself:

- **Change is verified, never assumed.** An output counts as "coming back to
  you" only when `Wallet::is_mine` says our descriptors derive its script. The
  classic attack on a PSBT signer is a change output that is not yours; a wallet
  that trusts the file's `bip32_derivation` field walks straight into it. Show
  the full derivation path beside it, as the transaction detail already does, so
  it can be checked against a device screen.
- **The fee is computed** as inputs − outputs and shown prominently. The other
  classic attack is a fee that quietly eats the change. An unreasonable rate —
  say more than an order of magnitude above the current tip's average — is
  called out rather than merely displayed.
- **Inputs we do not own are named as such.** When some inputs are not ours,
  `calculate_fee` cannot be trusted, and the honest answer is "the fee cannot be
  worked out from this file alone" rather than a confident wrong number. That
  case is also exactly the multisig and coinjoin shape, so failing honestly here
  is what makes the screen extensible later.
- **Refuse outright** when no input is ours: *"this payment does not spend
  anything from this wallet."* There is nothing useful Sieve can do with it and
  pretending otherwise invites signing something unrelated.
- **Signing reuses the existing path**, including `check_signer`, which already
  catches a wallet whose BIP-39 passphrase does not match the descriptors.

## Format

Read **both** binary `.psbt` and base64 text, sniffed on the `psbt\xff` magic
bytes. Coldcard writes binary, half the internet writes base64, and refusing
either is an arbitrary annoyance at the exact moment somebody is trying to move
money. Write binary by default, with a *Copy as text* for pasting elsewhere.

`gtk::FileDialog` for both directions, as the BIP-329 label import/export
already uses, so it works through the Flatpak portal.

## Deferred

- **QR transport** — BBQr or UR animation for Jade and Coldcard Q. A real chunk
  of work, and the return trip needs a camera Sieve does not have yet.
- **Broadcast-only import**, for a fully signed PSBT from elsewhere. Falls out
  of the above nearly free; include it if it is cheap.

## Where this stopped, and why

Export is in. Import is not, and is not next.

**What PSBT files are actually for, once the list is honest.** A Coldcard on an
SD card; an air-gapped second machine; a Jade over QR, which needs a camera
Sieve does not have; and multisig, which is a milestone away. What they are
*not* for is the device most likely to be plugged into the computer running
this, because for a device on USB `HWI::sign_tx` is a better path than *save a
file, find a card, carry it, bring it back*.

So the ordering was wrong. PSBT-first was argued on the grounds that it needs no
device on the desk to build or test — a developer-convenience argument wearing
the clothes of a priority. The thing that makes a hardware wallet spendable is
USB signing, and that is what M4a should reach for next.

Export still earns its place: it takes a device-imported wallet from *cannot
spend at all* to *can spend awkwardly*, it is finished and tested across all
four script types, and it leaves no half-built state behind, because import was
never started.

**What to build when import comes back.** The review screen above, unchanged —
it is still the whole safety story. And one thing learned since: once a
transaction is signed *and finalised*, PSBT stops being the useful artefact. The
thing to carry to a broadcasting machine is the raw transaction hex, which
`sendrawtransaction` and every explorer's broadcast box will take. Partially
signed stays a PSBT; fully signed should offer the hex as well. Building "sign
and save it back out" on the same screen as "sign and broadcast" gets that for
nothing.

## Multisig

Deliberately out of scope, and worth saying why it is nonetheless adjacent.

The loop above *is* a multisig signing round: import, sign, export, hand to the
next co-signer. If the review screen handles "inputs I do not own" and "signed
by 1 of 3" honestly from the start, none of this needs revisiting.

What multisig actually needs is elsewhere, and is a milestone rather than a
feature:

- `wallet::watch::parse` understanding `wsh(multi(...))` and `tr()` with script
  paths, where today it takes single-key descriptors only.
- A different account-creation path: `Account::create_watching` assumes one
  external and one internal descriptor derived from one key.
- Key-origin bookkeeping for co-signers, and somewhere to keep their xpubs.
- Wallet-policy registration on hardware — the same HMAC dance already noted in
  `ROADMAP.md` M4a for verifying addresses on a Ledger.
- A coordinator flow for assembling the descriptor in the first place, which is
  the part with no obvious right answer.

## What already exists to build on

| Piece | Where | Note |
|---|---|---|
| Watch-only PSBT construction | `Session::plan` | No keys involved |
| Signing and the passphrase check | `wallet::send::{signer, check_signer, sign}` | Reused unchanged |
| Broadcast | `Session::sign_and_send` → `submit_package` | Split the signing half out |
| Reading a built transaction's outputs | `wallet::send::split_outputs_of` | Written for RBF; the review screen wants exactly this |
| File dialogs through the portal | `App::{export_labels, import_labels}` | Same shape |
