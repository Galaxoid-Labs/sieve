# Passwords, and how many of them there should be

A design, not an implementation. Nothing here exists yet.

Sieve has one password per wallet. Two things are wrong with that, and they
are separate problems that happen to share a solution.

## The two problems

**A wallet with no keys has no lock.** A wallet imported from a Ledger has no
vault, because there is no seed to seal — so there is no password, and nothing
to ask for. Open Sieve, pick that wallet, and its balance, its history and
every address it has handed out are on screen. The idle lock shuts the view;
it cannot shut a wallet that was never locked.

Sparrow states the principle plainly: *"even if it only contains public keys,
that data is still worth protecting properly."* It encrypts watch-only wallets.
Sieve does not.

**Several wallets means several passwords.** Mainnet, signet, a hardware
import: three wallets, three passwords, and in practice that means one
password used three times, or three weak ones. The security model assumes
independent secrets and the human supplies a single reused one.

## What the others do

Checked rather than remembered.

| | Electrum | Sparrow | Sieve today |
|---|---|---|---|
| Scope | per wallet file | per wallet file | per wallet |
| KDF | PBKDF2 / ECIES for the file | Argon2, tuned to ≥500ms | Argon2id, 256 MiB / 3 / 4 |
| Watch-only | no password | **encrypted** | **no password** |

Neither has a global application password. Both are per-wallet, which is the
model Sieve already has — so this is not a case of being unusual. What Sieve
is missing is the thing Sparrow does: locking a wallet that holds no keys.

Sources: [Electrum FAQ](https://electrum.readthedocs.io/en/latest/faq.html),
[Sparrow features](https://sparrowwallet.com/features/),
[Sparrow best practices](https://sparrowwallet.com/docs/best-practices.html).

## The proposal: both, chosen once

At first run, one question with two answers, and it can be changed later:

**One password for everything.** Every wallet — including watch-only ones — is
sealed with the same secret. Type it once when Sieve opens; wallets open
without asking again. This is the right answer for almost everybody, because
the realistic alternative is not three strong passwords but one reused one.

**A password for each wallet.** What Sieve does now, made deliberate rather
than incidental. For somebody who wants a wallet that a person who watched
them unlock the app still cannot open.

The choice is per *installation*, not per wallet, but a wallet can opt out of
the shared password afterwards — that is strictly a matter of which secret
seals it, and the format does not care.

## Why this needs no change to the vault

The format already separates the two keys:

```
magic 6 | salt 16 | header | nonce 24 | wrapped DEK 48 | nonce 24 | ciphertext
```

A password derives a **KEK**; the KEK wraps a random **DEK**; the DEK encrypts
the seed. Every wallet has its own DEK, generated from the OS. "One password"
therefore does not mean one key: it means one secret that unwraps several
independent keys, which is the property worth having.

Which is also why changing the password is cheap: re-derive the KEK, re-wrap
each DEK, leave every ciphertext alone. A seed is never re-encrypted.

## The one new artefact

A shared password needs something to check it against, because a watch-only
wallet has no ciphertext of its own to fail on. So: `app.lock`, a vault whose
plaintext is a known constant, sealed with the app password. Opening it proves
the password; failing to open it is a wrong password, told apart from a
corrupt file by the AEAD tag exactly as the wallet vault already is.

It is the same format, the same code path, and nothing new to review.

For a watch-only wallet under the shared password, `app.lock` is the whole
mechanism: there is still no vault for that wallet, because there is still no
seed. What the password gates is the view, and that is honest — the wallet's
data is public, and what is being protected is somebody's privacy rather than
their coins. **The UI must say that**, or it promises more than it delivers.

## What changes on screen

- **First run** asks the question once, in plain terms: *one password for this
  app, or a password for each wallet.* With a recommendation, because most
  people should take the first.
- **Unlock** becomes an app-level screen when the shared password is on, and
  stays per-wallet otherwise.
- **A watch-only wallet gets a lock** either way — the per-wallet mode needs an
  optional password for it, which is a wallet with an `app.lock` of its own and
  no seed.
- **Signing still asks again.** Same secret now, but being unlocked must not
  mean being able to spend. The re-ask is about deliberateness, not about a
  second factor.
- **Changing the password** re-seals `app.lock` and re-wraps every DEK.

## Migration

Existing wallets are sealed with their own passwords, and there is no way
around asking for them one final time:

1. Set the app password.
2. For each wallet, ask for its current password once, unwrap the DEK, re-wrap
   it under the new KEK, write atomically.
3. A wallet whose password is not known is left alone and stays per-wallet.
   Nothing is lost and it can be converted later.

Every step is a re-wrap, never a re-encrypt, so a failure part-way leaves each
wallet openable by exactly one password — the old one or the new one — and
never by neither.

## The argument against, stated properly

**Blast radius.** Today, whoever learns the signet wallet's password learns
nothing about the mainnet one. Under a shared password, one secret opens
everything. That is a genuine loss and no amount of convenience makes it not
one.

The counter-argument is empirical rather than theoretical: several passwords
in practice means one password entered several times. If that is what happens,
the separation was never real, and a single password somebody actually chose
well is better than three they reused. Offering both is how that argument is
settled by the person it affects rather than by this document.

## Open questions

- Should the shared password be **required**, or is "no password at all" a
  legitimate choice for a machine with full-disk encryption? Sieve currently
  forces one for a seed wallet and permits none for a watch-only one, which is
  the least defensible pair of positions available.
- Should `app.lock` hold anything useful — the wallet list, say — so that the
  set of wallets is not readable without the password? It is a directory
  listing today.
- Does the idle lock re-ask the app password, or shut the view only? Shutting
  the view is what it does now, and under a shared password "locked" starts to
  mean something stronger.
