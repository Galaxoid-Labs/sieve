# Security and privacy

What Sieve does with key material and what it tells the world, audited against
the code rather than against the intention. Every claim below names the file
that makes it true, so it can be checked and so it fails visibly when it stops
being true.

**Nobody independent has reviewed any of this.** It is a young program that
handles money.

## The threat model, stated so it can be argued with

Sieve is built against three adversaries and not against a fourth:

1. **A server that would learn your wallet.** Defeated by design: there is no
   server. Compact block filters are downloaded from ordinary peers and
   matched locally, so nothing is ever asked about a specific address.
2. **Somebody who takes the disk.** Defeated by the vault: the seed is sealed,
   and the databases hold only public descriptors.
3. **Somebody watching the network.** Reduced, not defeated. A peer sees your
   IP and that you broadcast a transaction; Tor removes the first.

Not defended against: **an attacker who is already running code as you.** They
can read the process's memory while it is unlocked, log your keystrokes, or
replace the binary. `PR_SET_DUMPABLE(0)` raises the cost of the first, and
that is all it does. No user-space wallet defends against this; a wallet that
claims otherwise is lying.

## Key material

**At rest** — `src/vault/`

The seed is sealed with XChaCha20-Poly1305 under a key derived by Argon2id at
256 MiB, 3 passes, 4 lanes. The header — version, salt, KDF parameters — is
bound as additional authenticated data, so a header edited to weaken the KDF
fails to decrypt rather than decrypting with weaker settings. Written
atomically, mode 0600.

Watch-only wallets have no vault at all: there is nothing to protect, and a
password would be theatre.

**In memory** — `src/wallet/send.rs`, `src/wallet/node.rs`

The seed is decrypted at exactly two moments: signing a payment, and revealing
the phrase. In both, the password is asked for again — being unlocked is not
being able to spend. The signing wallet is built with
`create_wallet_no_persist`, so key material never reaches a database, and is
dropped when the signature is finished.

Secrets travel in `Zeroizing`, and the types that carry them —
`Password`, `Secret` — implement `Debug` by hand to print `<redacted>`.
That is not tidiness: Relm4 traces every message under `RUST_LOG=relm4=trace`,
and a derived `Debug` would have written seed phrases into the log.

**What that does not cover.** A Rust `String` can reallocate, leaving a copy of
the old buffer for the allocator to hand out later; `Zeroizing` clears the
final buffer, not every buffer it ever lived in. GTK holds the text of the
password field and the revealed phrase in widgets Sieve does not control.
Both are real, both are bounded by "an attacker already running as you", and
neither has a fix that does not amount to writing a different toolkit.

**Process hardening** — `src/main.rs`

`RLIMIT_CORE=0` so a crash writes no dump. `PR_SET_DUMPABLE(0)` so another
process running as the same user cannot attach a debugger — the one that
matters. `mlockall(MCL_CURRENT|MCL_FUTURE)` is attempted and **routinely
fails**: the default `RLIMIT_MEMLOCK` is 8 MiB and a GTK application is far
larger, so this is expected rather than a misconfiguration. It is logged and
not treated as fatal.

Which means **secrets can reach swap**, and the answer is encrypted swap
rather than anything Sieve can do. On the machine this was written on, swap is
a file on a LUKS volume and a zram device with no disk backing, so both are
covered — but that is this machine, not a guarantee.

## What leaves the machine

Every outbound connection, and what it discloses:

| Where | When | What it reveals |
|---|---|---|
| Bitcoin peers | always | your IP; that you run a wallet. **Never which addresses are yours** |
| Broadcasting | on send | to that peer, that this transaction is probably yours |
| DNS seeds | at startup | to a resolver, that you are looking for Bitcoin peers |
| Bitfinex | only if dollars are on | your IP, and when you opened a wallet. No wallet data |
| mempool.space fees | only if switched on | your IP, and that a payment is about to be sent |
| mempool.space explorer | only when you click | that you looked at one specific transaction |

The last three are off by default, and each says what it costs at the switch
that turns it on. The first two are inherent to being a wallet.

**Tor** — `src/tor/`

With Tor on, all of the above go through the SOCKS5 proxy, and the proxy is
**verified to be Tor** using its `RESOLVE` extension before anything is sent
through it: a plain SOCKS5 proxy pretending to be Tor is rejected rather than
trusted. The node's own DNS seeding, which would have leaked around the proxy,
is replaced by seeds resolved through Tor. If Tor is on and unreachable, Sieve
refuses to connect rather than falling back to the clear.

## On disk

Everything Sieve writes lives under `~/.local/share/sieve`, mode 0700, with
every file 0600 — verified against a real installation, not just the code
that creates them.

| | |
|---|---|
| `wallets/<id>/vault.sieve` | the sealed seed |
| `wallets/<id>/wallet-bip*.sqlite` | public descriptors and transaction history |
| `wallets/<id>/wallet.meta.json` | network, birthday, paths, scan progress |
| `wallets/<id>/labels.jsonl` | **plaintext**, and the UI says so |
| `peers/<network>.json` | peers that worked last time |
| `settings.json` | preferences, including the last wallet opened |

**Labels are not encrypted**, deliberately: a watch-only wallet has no password
to encrypt them with, and the transaction history sitting beside them is
readable anyway. The preferences row that exports them says this in words.

## Findings from this pass

Three, all of them logging, all fixed in the commit that adds this file. They
share a shape: identifiers that name the user's own money, written somewhere
more people can read than the wallet file.

- **Transaction ids at `info`.** A fee bump logged the replaced and
  replacement ids at the default level, which under a desktop session means
  the systemd journal — readable by more people than `~/.local/share`. Now
  logged without them.
- **Block hashes of matched blocks at `debug`.** A block that matches this
  wallet's filters is a block holding this wallet's transactions. Naming them
  in a log hands the user's transaction set to whoever the log is shared with,
  which for a log is routinely somebody helping to debug something unrelated.
  Now a count.
- **The balance at `debug`.** The most sensitive number the app holds that is
  not a key. Now a transaction count.

## The dependencies

310 crates, audited with `cargo-audit` and governed by `deny.toml`. Both were
run for the first time in the same pass that produced this document.

**`cargo audit`: no vulnerabilities.** 310 crates against 1,226 RustSec
advisories, no warnings, nothing yanked.

**`cargo deny check`: advisories ok, bans ok, licenses ok, sources ok.**

`deny.toml` is the standing decision rather than a rubber stamp, and three
parts of it are worth reading:

- **Every dependency comes from crates.io.** No git dependencies, no alternate
  registries — checked, and now enforced. A git dependency is a supply chain
  with no index, no yank mechanism and no advisory database behind it.
- **Permissive licences only**, listed by name. Two MPL-2.0 crates are allowed
  deliberately (`option-ext`, and `serialport` — how a Specter is spoken to);
  MPL's copyleft reaches modified MPL files and not the program linking them,
  and Sieve modifies neither. `unescaper` offers "MIT OR GPL-3.0-only" and
  Sieve takes the MIT half, which is what a disjunction is for. A licence not
  on the list stops the check and gets read.
- **Duplicate versions warn rather than fail**, because the warning is the
  point. Today it reports four versions of `bitcoin_hashes` (0.13, 0.14, 0.15,
  0.20) and two of `rand_core` (0.6, 0.10), pulled in by different parts of
  the Bitcoin stack. Four copies of a hashing implementation is not a
  vulnerability, and it is exactly the shape of thing worth seeing rather than
  hiding — which is why the setting is `warn` and not `allow`.

Neither tool runs in CI yet, because there is no CI yet; `PACKAGING.md` puts
both in the release workflow, where a tag that fails an audit should not
produce packages.

## Known gaps
- **No reproducible build**, so a binary cannot be checked against this source.
- **The audit is a snapshot.** It passed on the day this was written; an
  advisory published tomorrow makes it stale, which is the argument for
  running it on every release rather than reading this paragraph.
- **No review by anybody else.** The vault format and the signing path are the
  two places where that matters most.
- **`unsafe`** appears in two files: `main.rs` (three libc calls for
  hardening) and `tor/daemon.rs` (process control). Both are small, both are
  syscalls rather than pointer arithmetic, and neither has been reviewed by
  anyone but their author.
- **Swap** is out of Sieve's hands, as above.

## If you find something

There is no security contact yet because there is no public repository yet.
When there is, this section gets an address and a key.
