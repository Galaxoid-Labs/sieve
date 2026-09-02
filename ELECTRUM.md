# Electrum seeds

Recognised and refused. This is what supporting them would take, written down
because the decision not to is a decision rather than an oversight, and because
the reasons will not be obvious to whoever reads the refusal next.

## What Electrum does

**It does not use BIP-39, and it uses the same 2,048 English words.** That
combination is the whole problem: an Electrum seed is twelve real BIP-39 words
that fail the BIP-39 checksum, which is indistinguishable from a mistyped
phrase unless something looks for it.

Instead of a checksum it uses a **version number** — a prefix of a hash of the
phrase:

```python
# electrum/mnemonic.py
hmac_sha_512("Seed version", normalize_text(phrase)).hex().startswith(prefix)
```

| Prefix | Wallet | Account | Script |
|---|---|---|---|
| `01` | standard | `m/0'` | p2pkh |
| `100` | segwit | `m/0'` | p2wpkh |
| `101`, `102` | two-factor | — | 2-of-3 with Electrum's co-signing service |

Receive addresses are `m/0'/0/i` and change is `m/0'/1/i`.

The root key comes from PBKDF2 with one word changed from BIP-39:

```python
# electrum/mnemonic.py — the salt is `electrum`, where BIP-39 uses `mnemonic`
hashlib.pbkdf2_hmac('sha512', mnemonic, b'electrum' + passphrase, iterations=2048)
```

## What Sieve does today

`phrase::electrum_seed` detects it and the screen says so, in `warning` rather
than `error`, and the Import button is not pressable. **The point of all three
is that the person is holding a correct backup.** Before this, the status line
read "one is out of order or in place of another", which sends somebody to
re-check paper that is already right — and the conclusion available to them is
that their backup is ruined.

Two-factor seeds are detected as well, and they are not a Sieve problem at all:
those wallets need Electrum *and* its co-signing service, so the words alone
recover nothing anywhere.

## What supporting it would take

In order. The last two are the work; the first two are an afternoon.

### 1. The root key — easy

PBKDF2-HMAC-SHA512, 2,048 rounds, salt `electrum` + passphrase, 64 bytes out,
into `Xpriv::new_master`. **`ring` is already in the dependency tree** and has
`PBKDF2_HMAC_SHA512`, so this adds no crate. Perhaps fifteen lines.

### 2. Full text normalisation — small, and must be exact

Detection normalises the English case only: lowercase and single spaces. That
is safe for *recognising* a seed, because anything unusual simply falls back to
the ordinary message.

**Derivation is different: it has to match Electrum byte for byte, or it
produces a valid-looking wrong wallet.** Electrum's `normalize_text` does NFKD,
lowercase, strips combining characters, normalises whitespace, and removes
whitespace between CJK characters. For English these are equivalent; for
anything else they are not, and the failure is silent.

### 3. A derivation path that is not a BIP purpose — the actual work

`ScriptType` is four variants — Legacy, NestedSegwit, NativeSegwit, Taproot —
each of which bundles *a path and a script* on the assumption that the path is
a BIP purpose number. It is referenced around 190 times across ten files.

Electrum breaks the assumption twice: its path is `m/0'`, which is not a
purpose, and **the same path carries two different scripts** depending only on
the seed's version prefix.

So it needs two new variants, and the compiler will find every match. Most are
mechanical. Four are decisions:

- **`ScriptType::ALL` must not include them.** It is what an import searches,
  and adding Electrum paths there would give every ordinary BIP-39 import six
  databases and two permanently empty rows in the derivation breakdown.
- **`can_receive` / `offers_path`: yes.** An Electrum import lives on that
  path, so refusing to hand out addresses there would leave the wallet unable
  to receive at all.
- **`hardware::account_path`: refuse.** No device derives `m/0'`, and a
  device-backed wallet can never be an Electrum one.
- **`db_file`, `example_prefix`, labels**: mechanical.

### 4. Test vectors — not optional

This is money code whose failure is silent: a wrong path finds no coins and
reports no error, which looks exactly like an empty wallet. It needs known
seed → known first address pairs taken from Electrum's own test suite, for both
the standard and segwit versions, on mainnet and a test chain.

### 5. Signing

Falls out of the above. Once the descriptors are right, signing is the ordinary
path — `check_signer` compares the derived descriptor id against the watched
one and would catch a wrong derivation before anything is signed.

## The thing to try before any of it

**Watch-only Electrum may already work, with no code at all.**

`watch::script_type_of` falls through to the descriptor's script function when
the origin's purpose is not 44/49/84/86, and `import_descriptor` passes the
descriptor to BDK **verbatim** — `ScriptType` only picks a database filename and
a label. So `wpkh([fingerprint/0h]xpub…/0/*)` should import and watch
`m/0'/0/*` correctly today.

If that holds, an Electrum user can already see their wallet in Sieve — balance,
history, receive, and a PSBT to sign in Electrum — and the only thing seed
import adds is *spending from Sieve*. That is a much narrower reason to take on
a competitor's proprietary format.

The friction that used to exist here is gone: Electrum shows a **zpub**, and
Sieve now accepts `zpub`/`ypub`/`upub`/`vpub` and rewrites them, so there is
nothing to convert by hand.

**This is untested.** It is reasoning from the code, and the same reasoning was
wrong once already — an earlier reading of this file's own subject concluded
that `script_type_of` would refuse an Electrum origin, which it does not. Test
it before believing it.

## Why it is refused rather than half-supported

A wallet that imports a seed and shows an empty balance is worse than one that
says it cannot. Every route in would have to be right — the normalisation, the
salt, the path, the script — and each of them fails silently and identically.
Detection costs ten lines and turns the worst outcome, somebody concluding
their backup is ruined, into an accurate sentence.

## Sources

- [Electrum seed phrase documentation](https://electrum.readthedocs.io/en/latest/seedphrase.html)
- [`electrum/mnemonic.py`](https://github.com/spesmilo/electrum/blob/master/electrum/mnemonic.py)
- Sparrow, by contrast, uses BIP-39 with standard paths and already imports by
  phrase — its multisig is `m/48'`, which Sieve does not support for a
  different reason.
