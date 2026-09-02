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

**A hardware signer moves that last line**, which is the only thing in this
model that does. The keys are never in this process, so reading its memory
yields nothing to spend with, and the device's own screen and button are what
an attacker on this machine has to get past. See "Keys that are not on this
machine" below for what that does and does not cover.

## Key material

**Where it comes from** — `src/wallet/mod.rs`

`getrandom::fill`, which is the `getrandom(2)` syscall on Linux — the same call
the vault uses for its salt, its nonces and its data key. One entropy source in
the program. A failure to read it stops wallet creation rather than falling back
to anything, because a phrase from a fallback nobody chose is indistinguishable
from a good one for as long as the wallet exists.

`Mnemonic::generate` would have supplied its own from `rand`'s `ThreadRng` — a
real CSPRNG, seeded from the OS, never unsafe — but it meant the one
irreplaceable secret came from userspace while everything else came from the
kernel, and one source is easier to argue about than two.

Dice rolls, when somebody supplies them, are **mixed in and never substituted**:
`os_bytes XOR SHA256(rolls)`. So no roll count and no loaded die can produce a
phrase weaker than the operating system alone would have given. Replacing the OS
bytes with `SHA256(rolls)` is verifiable and is the one thing that must never be
built here; `DICE.md` records why, and that Electrum shipped exactly that and
withdrew it. A test asserts the same rolls twice give *different* phrases, which
is the only external sign that the mixing is still mixing.

**At rest** — `src/vault/`

The seed is sealed with XChaCha20-Poly1305 under a key derived by Argon2id at
256 MiB, 3 passes, 4 lanes. The header — version, salt, KDF parameters — is
bound as additional authenticated data, so a header edited to weaken the KDF
fails to decrypt rather than decrypting with weaker settings. Written
atomically, mode 0600.

Watch-only wallets have no vault, because there is no seed to seal. They can
still be locked: `lock.sieve` holds a known constant sealed with a password,
and opening it is what proves the password. That gates the wallet inside Sieve
— the balance, the history, the addresses — and **does not encrypt anything on
disk**, because the descriptors and history live in SQLite that the node
writes to on every block. Every screen that offers it says so.

The same fact from the other side: **deleting `lock.sieve` removes the lock**.
That is a real limit on what it defends — anybody with the files can take it
off — and it is also the only way back from a forgotten password, since a
watch-only wallet has no recovery phrase to restore from. Which is why the
dialog that sets one asks for it twice and will not accept a pair that
disagree.

**The password is never handed to the desktop keyring**, and that is a decision rather than
an omission. Secret Service is not the macOS Keychain: on an ordinary Linux session the login
keyring is unlocked by PAM at login, so a password kept there is guarded by *being logged in*
rather than by anything anybody knows — while the adversary this vault is built against is
the one holding the disk. Storing the password beside the file it opens erodes the only
boundary that file draws. Sieve doing its own locking and encryption is the answer to that,
not a stage on the way to something better.

**One password per wallet, not one for the app.** Electrum and Sparrow are
both per-wallet-file too, so this is the ordinary arrangement rather than an
unusual one. A single application password was considered and rejected: it
would mean one secret opening every wallet, and typing one fewer password is
not worth that. Sparrow's one improvement on the usual arrangement — locking
watch-only wallets, on the grounds that public keys are still worth protecting
— is the part Sieve adopted.

**In memory** — `src/wallet/send.rs`, `src/wallet/node.rs`, `src/wallet/mod.rs`

The seed is decrypted at exactly three moments, and each asks for the password
again — being unlocked is not being able to spend:

| Where | Why |
|---|---|
| `app.rs`, signing a payment | the only place a signature is made |
| `ui/reveal.rs`, showing the phrase | the operation *is* the disclosure |
| `wallet::add_script_types` | a derivation path nobody watched has no descriptors anywhere, and only the seed makes them |

The signing wallet is built with `create_wallet_no_persist`, so key material
never reaches a database, and is dropped when the signature is finished. The
third case derives public descriptors and writes only those.

`wallet::unlock` also opens the vault, and is deliberately not on that list: it
decrypts to prove the password is right and drops the plaintext inside the
function. `UnlockOutput::Unlocked` carries nothing but paths and a watch-only
summary. **There is no field anywhere holding a decrypted seed for the session,
and adding one is a bug.**

Secrets travel in `Zeroizing`, and the types that carry them — `Password`,
`Secret`, `Face` — implement `Debug` by hand to print `<redacted>`. That is not
tidiness: Relm4 traces every message under `RUST_LOG=relm4=trace`, and a derived
`Debug` would have written seed phrases into the log.

`Face` is one die roll, and it is on that list for a reason worth stating: a
single face looks harmless, and a derived `Debug` would have written the whole
sequence to the log one line at a time. The rolls are a share of the seed until
the phrase exists. They are never written to disk, and are dropped when the roll
screen is left.

**What that does not cover.** A Rust `String` can reallocate, leaving a copy of
the old buffer for the allocator to hand out later; `Zeroizing` clears the
final buffer, not every buffer it ever lived in. GTK holds the text of the
password field and the revealed phrase in widgets Sieve does not control.
Both are real, both are bounded by "an attacker already running as you", and
neither has a fix that does not amount to writing a different toolkit.

**Process hardening** — `src/main.rs`

`RLIMIT_CORE=0` so a crash writes no dump. `PR_SET_DUMPABLE(0)` so another
process running as the same user cannot attach a debugger — the one that
matters.

**`PR_SET_DUMPABLE` is lifted for the length of a file dialog**, and put back
when it closes. Setting it to zero does more than stop core dumps: the kernel
re-owns `/proc/<pid>` to root, and `xdg-desktop-portal` reads
`/proc/<pid>/root` to identify a caller. Unable to, it refuses that caller
everything — including the file chooser, which then never appears and reports
nothing. Every file dialog in Sieve silently did nothing until this was found.
The window is one somebody opened on purpose, and Sieve handles only public
data inside it: labels, descriptors, a PSBT. Signing is never inside it, and
`RLIMIT_CORE=0` is not lifted, so a crash still writes no dump. `mlockall(MCL_CURRENT|MCL_FUTURE)` is attempted and **routinely
fails**: the default `RLIMIT_MEMLOCK` is 8 MiB and a GTK application is far
larger, so this is expected rather than a misconfiguration. It is logged and
not treated as fatal.

Which means **secrets can reach swap**, and the answer is encrypted swap
rather than anything Sieve can do. On the machine this was written on, swap is
a file on a LUKS volume and a zram device with no disk backing, so both are
covered — but that is this machine, not a guarantee.

## Keys that are not on this machine — `src/hardware.rs`

A wallet imported from a hardware signer has **no vault**, because there is no
seed to seal. Everything above about Argon2, XChaCha20-Poly1305 and decrypting
at the point of use describes a wallet Sieve holds keys for, and none of it
applies here: `Meta::watch_only` records that there is nothing sealed, and the
descriptors on disk are public. The private keys never exist in this process,
which removes the whole class of memory disclosure the previous section is
careful about — swap, reallocated `String`s, GTK's widget buffers. It is the
strongest position Sieve can be in and it is worth saying plainly.

**Untested against real hardware.** Everything checkable without a device on
the desk is checked; nothing that needs one is. Treat this section as
describing what the code does rather than what has been observed to work.

Every exchange with a device, and what each one is:

| | |
|---|---|
| `enumerate`, `get_version` | which devices are attached, and what they are |
| `get_master_fingerprint` | four bytes identifying the seed on the device |
| `get_extended_pubkey` | a public account key at `m/purpose'/coin'/0'` |
| `display_address` | asks the device to show an address on its own screen |
| `sign_tx` | a PSBT out, partial signatures back |

**What comes back from a device is checked rather than believed.** Signatures
arrive as a PSBT, and turning them into witnesses is done from the wallet's own
descriptors — `finalize_psbt`, which fails rather than broadcasting when an
input is not properly signed. Nothing a device returns is broadcast on its
word.

**The device is identified before it is asked to sign.** Its master fingerprint
is compared against the one already written into this wallet's descriptors, and
a mismatch refuses with a reason. Signing with the wrong device otherwise
produces signatures that fail to finalise much later, with nothing to explain
why.

**Verifying an address on the device is the one check this program cannot make
for itself.** Sieve derives the address on this machine and draws it on this
machine's screen; code that has got onto the machine can do both differently
and the money goes elsewhere with nothing looking wrong. A device deriving its
own copy and showing it on its own screen is outside that. **Sieve never
reports "verified"** — it cannot see the device's screen, so a tick drawn by
this program would be precisely the reassurance an attacker needs.

**udev rules are the reason a device is invisible on Linux**, and they are
shipped by the package rather than by the application. A build from source
installs none, which is why an empty device list says so instead of leaving a
blank. What they grant is ordinary: the logged-in user's access to the device
node. Any process running as that user can then also talk to the signer, which
is the same boundary as everything else in this file — an attacker already
running as you can ask the device to sign, and the device's own screen and
button are what stand in the way. That is the reason a device has them.

## What leaves the machine

Every outbound connection, and what it discloses:

| Where | When | What it reveals |
|---|---|---|
| Bitcoin peers | always | your IP; that you run a wallet. **Never which addresses are yours** |
| Broadcasting | on send | to that peer, that this transaction is probably yours |
| DNS seeds | at startup | to a resolver, that you are looking for Bitcoin peers |
| Bitfinex | only if dollars are on | your IP, and when you opened a wallet. No wallet data |
| mempool.space fees | only if switched on | your IP, and that a payment is about to be sent |
| mempool.space explorer | only when you confirm | that you looked at one specific transaction |

The last three are off by default, and each says what it costs at the switch
that turns it on. The first two are inherent to being a wallet.

Not in that table because it is not a network connection: a **hardware signer
on the USB cable** receives an account derivation path, an address index and a
PSBT — which together describe this wallet's addresses and one payment. It goes
to a device somebody plugged in on purpose and nowhere else, and Tor is not
involved and could not be.

**Tor** — `src/tor/`

With Tor on, all of the above go through the SOCKS5 proxy, and the proxy is
**verified to be Tor** using its `RESOLVE` extension before anything is sent
through it: a plain SOCKS5 proxy pretending to be Tor is rejected rather than
trusted. The node's own DNS seeding, which would have leaked around the proxy,
is replaced by seeds resolved through Tor. If Tor is on and unreachable, Sieve
refuses to connect rather than falling back to the clear.

## Findings from the privacy pass

Three, all fixed in the commit that adds this section. The first is the one
that mattered.

**The opt-in services went out over the clear while Tor was coming up.** The
proxy was read as `settings.tor.then_some(tor_active).flatten()`, which
answered `None` for two different situations — Tor switched off, and Tor
switched on but not yet running — and every caller read `None` as "connect
directly". It was not a narrow window: the price lookup runs immediately
*before* the code that starts Tor, so with both switches on, opening the first
wallet after launch announced to a price service that a Bitcoin wallet at this
address had just been opened. `start_session` had its own guard and was
correct; these two did not. The proxy is a three-state `Route` now — `Direct`,
`Through`, `Refuse` — so there is no value meaning "no proxy" that a
wanted-but-absent Tor can be mistaken for. A fee lookup in that state falls
through to the node's own estimate, which needs no third party at all.

**The explorer link asks first.** Opening a transaction on mempool.space is the
only outbound request Tor cannot cover, because it leaves the process for the
system browser — carrying this machine's real address and whatever the browser
discloses, alongside a transaction that belongs to the person clicking. That
pairing is the most identifying thing available here, and it was one click away
in a screen people open to read a fee. It is a confirmation now, worded
differently when Tor is on, because "Tor is on and this still goes in the
clear" is the part that would otherwise surprise somebody.

**Copying a descriptor says where the copy goes.** A descriptor names every
address the wallet will ever have, and the dialog already said so; what it did
not say is that most desktops keep a clipboard history on disk. The file export
sitting beside it is the tidier route and now reads that way.

## On disk

Sieve writes to two directories, both mode 0700 with every file 0600 —
verified against a real installation, not just the code that creates them.

Wallet data lives under `~/.local/share/sieve` (`XDG_DATA_HOME`):

| | |
|---|---|
| `wallets/<id>/vault.sieve` | the sealed seed |
| `wallets/<id>/wallet-bip*.sqlite` | public descriptors and transaction history |
| `wallets/<id>/wallet.meta.json` | network, birthday, paths, scan progress |
| `wallets/<id>/labels.jsonl` | **plaintext**, and the UI says so |
| `peers/<network>.json` | peers that worked last time |
| `chain/<network>/` | handed to the node as a data directory, which it ignores |
| `tor/` | the data directory of a Tor that Sieve started |

Preferences live under `~/.config/sieve` (`XDG_CONFIG_HOME`):

| | |
|---|---|
| `settings.json` | preferences, including the last wallet opened |

**The split is a safety property, not tidiness.** One of those directories is
disposable and the other is not: every preference in it can be rebuilt by
opening the preferences dialog again, and a sealed seed cannot be rebuilt by
anything except the recovery phrase. Keeping them apart means somebody clearing
up after Sieve can delete the whole of one without going near the other. A test
(`preferences_do_not_live_among_the_wallets`) asserts the two roots differ and
that neither contains the other, so a future change cannot quietly put a vault
inside the directory this file calls safe to remove.

Preferences moved out of the data directory rather than having always been
there; `Settings::load` migrates a file from the old location once, by copy and
then remove, and leaves the old one alone if the write fails.

**Nothing removes any of this when the package is uninstalled**, deliberately —
see `PACKAGING.md`.

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

## Findings from the second pass

Seven, found by auditing the whole tree rather than a diff. All fixed in the
commit that adds this section, and the interesting part is not the list — it is
that **one of them was a fix this file already claimed to have made.**

- **The recovery phrase could reach the log.** `RevealCmd::Opened` was a
  `Result<String, String>` with a derived `Debug`, and relm4 formats every
  command result into a span field — `cmd_output=?message`, in its own
  `component/sync/builder.rs`. So showing the phrase wrote the phrase. It was
  the rule this project states first about secrets, broken in the one component
  whose entire purpose is that phrase. Now a redacted `Revealed` newtype, with
  a test that formats the message and looks for the words.
- **The balance was back in the logs**, at `debug`, after the paragraph above
  says it was taken out. Nothing failed when it returned; a document is not a
  mechanism. `no_balance_in_the_logs` reads `app.rs` and fails on any `tracing`
  call mentioning a balance — a test about text, because the failure was.
- **An unprotected copy of the seed during signing.** The vault's own buffer is
  `Zeroizing`; `str::to_string` made a second copy that lived across the whole
  broadcast and was freed with the phrase still in it. Two other sites doing the
  same conversion borrow instead of copying, and were already right.
- **The data key survived the error paths in `vault::open`.** `dek.zeroize()`
  ran only after everything that could fail had succeeded, so a wrong key length
  or a body that failed authentication returned the key to the allocator intact.
  A guard that only fires on success is not a guard; it is `Zeroizing` now.
- **A vault header could ask for unlimited memory.** KDF parameters are read
  before anything can be authenticated — the header *is* the associated data —
  so a file somebody else wrote decided an allocation. The AAD binding already
  stops the attack that matters, weakening `m_cost` to brute-force a password;
  this closes the one it does not, which is asking for terabytes. Capped at
  1 GiB, four times the default.
- **`SeedWord` derived `Debug` holding a word of the phrase.** Not reachable by
  relm4's logging today, because its `Input` is `()`. Fenced anyway: it is one
  message signature away from being the finding above.
- **A descriptor id at `warn`.** Derived from the descriptor, so a stable
  fingerprint for the wallet, in a log people paste when asking for help.

**What this pass did not cover**, so the next one knows where to start: network
disclosure and tracking, which is a separate audit; fuzzing the descriptor,
PSBT and BIP-21 parsers; and any dynamic analysis at all — no sanitisers, no
timing work. The `unsafe` blocks were read and not analysed.

## The dependencies

310 crates, audited with `cargo-audit` and governed by `deny.toml`.

**`cargo audit`: no vulnerabilities.** 310 crates against 1,239 RustSec
advisories, no warnings, nothing yanked. Re-run when this file was last
revised, not quoted from the pass that first produced it.

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
  0.20) and two each of `rand_core` (0.6, 0.10) and `getrandom` (0.2, 0.4),
  pulled in by different parts of the Bitcoin stack. Sieve's own entropy call is
  `getrandom` 0.4, the direct dependency; 0.2 arrives under `rand`. Four copies of a hashing implementation is not a
  vulnerability, and it is exactly the shape of thing worth seeing rather than
  hiding — which is why the setting is `warn` and not `allow`.

**Both now run in CI**, on every push, alongside `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and `cargo test --locked`. An
advisory published tomorrow therefore fails the next build rather than waiting
for somebody to remember this file.

## Known gaps
- **No reproducible build**, so a binary cannot be checked against this source.
- **The audit is a snapshot.** It passed on the day this was written; an
  advisory published tomorrow makes it stale, which is the argument for
  running it on every release rather than reading this paragraph.
- **No review by anybody else.** The vault format and the signing path are the
  two places where that matters most.
- **The hardware signing path has never been run against a device.** It is
  written, it is tested everywhere a test can reach without hardware on the
  desk, and that is not the same thing. `ROADMAP.md` M4a says which order to
  try it in.
- **`unsafe` is gone, and `#![forbid(unsafe_code)]` keeps it gone.** This was
  a gap on the grounds that nobody but the author had read those blocks. Four
  syscalls — `setrlimit`, `PR_SET_DUMPABLE`, `mlockall`, `kill` — moved to
  `nix`'s safe wrappers, which does not delete the `unsafe` so much as move it
  into code many more people have read; the kernel boundary is unsafe by
  definition and no crate changes that. Four writes to the process environment
  in `tor/daemon.rs`'s own tests were removed outright, by passing a path
  rather than exporting one. `nix` was already compiled in transitively, so the
  crate count did not move: 310 before and after. One thing improved on the
  way — `setrlimit`'s result had been discarded, so a failure to switch core
  dumps off was silent.
- **Swap** is out of Sieve's hands, as above.

## If you find something

**Email <jacob@galaxoidlabs.com>.** That is the security contact. It reaches
one person, because Sieve is written by one — there is no team behind the
address and no rota, so treat the timeline as best effort rather than as a
commitment this file is in a position to make.

**Please do not open a public issue for a vulnerability.** The repository is
public — <https://github.com/Galaxoid-Labs/sieve> — so an issue is readable by
everybody from the moment it is filed, which for a wallet means the people who
would use the finding learn it at the same time as the person who can fix it.
Anything that is not a vulnerability is welcome as an issue and easier to
track there.

**There is no PGP key yet**, so mail to that address is unencrypted in transit
and sits in plaintext on a mail provider's disk. Send enough for the report to
be understood and acted on, and hold back anything you would rather that
provider did not keep — a working proof of concept can wait for a channel
agreed in the reply. The key belongs with the release signing key that
`PACKAGING.md` describes and that does not exist either; when it is made, this
section names its fingerprint.

GitHub's private vulnerability reporting is worth enabling on the repository as
a second route, and this section should say so once it is on.

**What is most worth looking at**, given what the rest of this file admits: the
vault format and the code that opens it, the signing path — including what a
hardware device hands back — and any way to make Sieve reveal which addresses
belong to a wallet, since that last one is the claim the whole program is built
on.
