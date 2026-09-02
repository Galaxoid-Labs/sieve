<h1 align="center">
  <img src="data/icons/hicolor/256x256/apps/com.galaxoidlabs.Sieve.png" width="128" alt=""><br>
  Sieve
</h1>

<p align="center">A Bitcoin wallet for Linux that keeps its business to itself.</p>

Sieve syncs by downloading BIP157/158 compact block filters and matching them
on your own machine. No server is ever told which addresses are yours, what
your balance is, or what you have spent — because no server is ever asked. The
wallet talks to ordinary Bitcoin nodes, the same way a full node's peers do,
and works out the answers itself.

Written in Rust with [Relm4](https://relm4.org), GTK 4 and libadwaita, so it
behaves like the rest of the desktop rather than like a browser in a window.

## What it does

**Sync without disclosure**
- Compact block filters (BIP157/158) downloaded from ordinary peers and
  matched locally. There is no server, no Electrum, no API key, no account.
- Scans resume where they were interrupted rather than starting over, and can
  be started again from the wallet's birthday on demand.
- Honest progress: headers, filters and the final phase — fetching the blocks
  a filter matched — are three different jobs and are reported as three.

**Tor, if you want it**
- Every outbound connection — peers, prices, fee rates — through a SOCKS5
  proxy, with the proxy *verified to actually be Tor* rather than taken on
  trust.
- Sieve uses a Tor already running on the machine, and starts one itself if
  there is none.
- If Tor is switched on and cannot be reached, Sieve refuses to connect rather
  than quietly going out over the clear.

**Spending, carefully**
- Payments built watch-only. Every number — amount, fee, change — is worked
  out before any key is involved.
- **Coin control**: choose which coins a payment spends, each named by you or by
  the payment that brought it in, with the linking you are about to create
  stated in plain words.
- **Freeze a coin** so it is never spent — on its own or alongside others. Kept
  as BIP-329's `spendable: false` in the same label file everything else uses,
  so a wallet you export to holds the same coins back.
- **Fee bump (RBF)** for a payment that is taking too long, with the race
  against the original explained rather than hidden.
- **Try to cancel** an unconfirmed payment, by replacing it with one that pays
  nobody. Called "try" because it is: the original is already out there, and if
  it is mined first the money is gone as it was meant to be.
- **Several recipients** in one transaction, which costs less in fees than the
  same payments made separately and says, to anybody reading the chain, that
  one person made all of them.
- **Attach data** (`OP_RETURN`) to a payment, or send a transaction that only
  carries data. Capped at 80 bytes so it relays everywhere, submitted exactly
  as typed, and warned about honestly — with nobody paid, such a transaction
  proves which outputs are yours rather than leaving anyone to guess.
- **Max**, which drains a path — or, with coins chosen, exactly those coins.
- BIP-21 payment URIs read on paste, including the amount and who is being
  paid.
- Fee estimates from the last block Sieve downloaded, which tells nobody
  anything; optionally from mempool.space, which is a disclosure and says so
  where you switch it on.
- Unconfirmed coins are never spent.

**Knowing what you have**
- An imported seed is watched on all four standard derivation paths at once —
  legacy, nested segwit, native segwit and taproot — because a restored seed
  can have coins on any of them, and watching four costs no more bandwidth than
  watching one. A wallet created here derives taproot and native segwit, since
  it has no history on the others and nothing sends to them by preference.
- **Watching a path and receiving on one are different questions.** A legacy
  address is watched so that money already there is found and can be spent, and
  is never handed out: a `1…` input carries its whole signature into the
  transaction's weight, so every coin received there costs more to move for as
  long as it exists. Native segwit is what the receive screen offers first,
  because `bc1q` is refused by nothing; taproot is one row down.
- **Search other derivation paths**, for when a wallet's recovery phrase has
  been used somewhere else and money is sitting where Sieve is not looking.
  Derives the missing accounts and scans again from the birthday — which is the
  honest answer to "my coins are missing", rather than a balance that quietly
  omits them.
- Activity filterable by derivation path, with the full path shown on every
  address and output: `m/86'/0'/0'/1/7`.
- **Labels** on payments, addresses and individual coins, stored in BIP-329's
  format so they can be exported and imported anywhere else. A coin without a
  name of its own inherits one from the payment that brought it in.
- Every address you have handed out, which are used, what each received, and
  which have been paid more than once.
- **Search** the activity list by amount, address, transaction id, the name you
  gave a payment, or anything it published.
- Amounts **typed in dollars** as well as read in them, with the bitcoin figure
  shown before you commit to it.
- **Export the public descriptors** — BIP-380 with checksums, which is what
  every other wallet reads. Enough to watch this wallet anywhere and not enough
  to spend from it — so it is the backup that costs privacy rather than money:
  whoever holds it can see every address and every payment, past and future.

**Hardware wallets**
- Import a watch-only wallet from a Ledger, Coldcard or Specter over USB — no
  Python, no HWI installation, no daemon.
- All four paths read from the device in one prompt.
- **Sign a payment on the device**, and **verify a receive address on it** — the
  one check this program cannot make for itself, since it computes the address
  and draws it on the same screen. Sieve never reports "verified": it cannot see
  the device's screen, and a tick it drew would be exactly the reassurance the
  attack needs.
- The right device is found by asking the connected ones which keys they hold,
  so a wallet is never tied to a note about the hardware it came from.
- **Save a payment to a file** instead, for a signer that is not plugged in.
- **None of the device code has been run against a device yet.** It is written,
  reviewed and covered by every test that does not need hardware. Treat it as
  untested until somebody has signed with it.

**Keys**
- The recovery phrase's randomness comes from the operating system —
  `getrandom(2)`, the same call that seals the vault — and the screen that shows
  you the words says so, with the number of bits they carry.
- **Roll your own randomness**, optionally: 50 to 100 throws of a die from a d6
  to a d20, *mixed into* the system's entropy rather than replacing it. It can
  only add — no roll count and no loaded die can make a phrase weaker than the
  one Sieve would have made alone. That matters because the way seed generation
  actually fails is a wiring mistake nobody can see: a build flag sent five
  years of Coldcards to a deterministic PRNG, and the seeds that survived the
  resulting theft were the ones with dice in them. See `DICE.md`.
- The seed is sealed with XChaCha20-Poly1305 under a key derived from your
  password with Argon2id (256 MiB, 3 passes, 4 lanes) and is decrypted only at
  the moment of signing — never held open while the wallet is on screen.
- Watch-only wallets can have a password too. They hold no keys, so there is
  nothing to decrypt — the password seals a known value instead, which gives a
  hardware-wallet or descriptor wallet something to fail against rather than
  leaving its whole history open to anyone who opens the app.
- Idle auto-lock, and lock when the computer goes to sleep or the session is
  locked — the three ordinary ways a machine is left unattended.

**Looking like the rest of the desktop**
- Light and dark follow the desktop, with no switch of Sieve's own: an
  application that can disagree with everything around it is an application
  that will.
- The system's accent colour on primary buttons — and on
  [Omarchy](https://omarchy.org), where a theme publishes a whole palette that
  GNOME's settings never carry, the backgrounds too. Nothing about any
  particular theme is written down: Sieve reads the mode, the accent and three
  surface colours out of whatever theme is current, so one you write yourself
  works exactly like a shipped one.
- Themes are followed live. Switching one changes the wallet without
  restarting it, light to dark included.

## What it does not do yet

Stated plainly, because a wallet that overstates itself is dangerous:

- **Hardware signing has never met hardware.** The code is written — signing,
  address verification, refusing the wrong device — and none of it has run
  against a real one. PSBT *export* works; *import*, which would let a payment
  signed elsewhere come back to be broadcast, is designed (`PSBT.md`) and not
  written.
- **Silent payments (BIP-352).** Paying one is designed and not written —
  `SILENT_PAYMENTS.md` has the plan. *Receiving* one is blocked on something a
  compact-filter wallet structurally cannot compute, and the same file explains
  why that is not a matter of effort.
- **Electrum seed phrases.** Electrum does not use BIP-39 — same words,
  different format, and a derivation path outside the BIP standards. Sieve
  recognises one, says so rather than calling it a typo, and will not let the
  import proceed; `ELECTRUM.md` records what supporting one would take.
  Wallets that use BIP-39 and standard paths, Sparrow among them, import by
  phrase today.
- **Multisig.** Single-signature only.
- **Packages.** There is no `.deb`, `.rpm` or AUR package yet — see
  `PACKAGING.md` for the plan.

Mainnet works and has been used with real funds. It has not been audited by
anybody, and it is a young program that handles money: read the source, and do
not trust it with more than you can afford to lose.

## Building

Requires Rust 1.93 or newer, and the GTK 4 and libadwaita development
packages. libadwaita must be **1.5 or newer** (GNOME 46).

```sh
# Arch, Omarchy
sudo pacman -S --needed rust gtk4 libadwaita sqlite openssl systemd-libs
# Debian, Ubuntu
sudo apt install cargo libgtk-4-dev libadwaita-1-dev libsqlite3-dev libssl-dev libudev-dev
# Fedora
sudo dnf install cargo gtk4-devel libadwaita-devel sqlite-devel openssl-devel systemd-devel

cargo run --release
```

Tor is optional and comes from your distribution: `pacman -S tor`,
`apt install tor`, `dnf install tor`. Sieve finds it on `PATH`.

### Hardware wallets from a source build

A USB signer is visible only to root until udev says otherwise, which is why
"the device is plugged in and nothing happens" is the usual first experience
on Linux. A packaged build installs the rules; a source build does not:

```sh
sudo install -Dm644 packaging/udev/51-sieve-hardware.rules \
  /usr/lib/udev/rules.d/51-sieve-hardware.rules
sudo udevadm control --reload && sudo udevadm trigger
```

Then unplug the device and plug it in again.

## Where things are

| | |
|---|---|
| `src/wallet/` | descriptors, the node, sending, labels, BIP-21 |
| `src/ui/` | one Relm4 component per screen |
| `src/vault/` | the sealed seed, and nothing else |
| `src/tor/` | the SOCKS client, the daemon, onion addresses |
| `src/hardware.rs` | USB signers |
| `CLAUDE.md` | how the pieces fit, and the mistakes not to repeat |
| `ROADMAP.md` | what is missing, in the order it hurts |
| `PACKAGING.md` | how this reaches other people's machines |
| `PSBT.md` | air-gapped signing, designed and not built |
| `SILENT_PAYMENTS.md` | why sending is buildable and receiving is not |
| `DICE.md` | entropy of your own: why it is mixed in and never substituted |
| `NOTIFICATIONS.md` | why there are none, and what it would take |
| `ELECTRUM.md` | why an Electrum seed is recognised and refused |
| `SECURITY.md` | what is defended against, what is not, and what leaves the machine |

```sh
cargo test          # 225 tests, needing no network and no display
cargo fmt --check
cargo clippy -- -D warnings
```

## Where your data lives

Two directories, both listed in Preferences under **Files**, with a button that
opens each one:

| | |
|---|---|
| `~/.local/share/sieve` | wallets: sealed seeds, watch-only databases, labels |
| `~/.config/sieve` | preferences, and nothing else |

They are separate so that one of them is safe to delete. Every preference can
be set again in a minute; a sealed seed can be recovered only from the recovery
phrase.

**Uninstalling Sieve leaves both of them where they are.** That is deliberate.
No Linux package manager removes files from a home directory — and for a wallet
it would be dangerous if one did, since a reinstall or a distribution upgrade
would take the seed with it. To remove a single wallet, use **Remove this
wallet** in Preferences. To remove everything, delete the directories above,
and only if you have the recovery phrase written down: it is the only other way
back to the coins.

## Networks

Mainnet, signet and testnet4.

Signet is the one to learn on: real block times, real peers, coins worth
nothing. Testnet4 is there for development against the chain your own software
targets, and it works because it has the peers for it — Sieve needs peers
serving compact filters, not merely peers, and testnet4 answers with enough of
them:

```
cargo test -- --ignored --nocapture filter_peers
bitcoin:  64 filter-serving addresses, need 8
signet:   35 filter-serving addresses, need 8
testnet4: 23 filter-serving addresses, need 8
```

That test asks the DNS seeders directly, so it reports what is reachable today
rather than what was true when this was written.

## Verifying a release

Releases carry one signature, over `SHA256SUMS`. Check the checksums against
it, then check your download against the checksums:

```sh
gpg --import sieve-signing-key.asc     # in this repository
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
```

The signing key is:

```
15C1 CCED 1259 9960 3558  12BC 1A9E 0F86 4D41 2FB7
Galaxoid Labs
```

Read that fingerprint from here rather than from the release page. A
fingerprint that only ever appears beside the files it signs is one an attacker
can replace along with them — the point of it being in the repository, in the
history, is that it is somewhere else.

Releases are signed by a subkey; the key above is the primary that certifies
it, and is the one to compare against. If a signature ever fails to verify,
that is a reason to stop rather than a reason to download it again.

## Licence

MIT. No warranty of any kind — see `LICENSE`.
