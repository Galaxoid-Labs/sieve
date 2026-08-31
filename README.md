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
- **Coin control**: choose which coins a payment spends, with each named by the
  label on the payment that brought it in, and the linking you are about to
  create stated in plain words.
- **Fee bump (RBF)** for a payment that is taking too long, with the race
  against the original explained rather than hidden.
- **Max**, which drains a path — or, with coins chosen, exactly those coins.
- BIP-21 payment URIs read on paste, including the amount and who is being
  paid.
- Fee estimates from the last block Sieve downloaded, which tells nobody
  anything; optionally from mempool.space, which is a disclosure and says so
  where you switch it on.
- Unconfirmed coins are never spent.

**Knowing what you have**
- All four standard derivation paths watched at once — legacy, nested segwit,
  native segwit and taproot — because a restored seed can have coins on any of
  them. Watching four costs no more bandwidth than watching one.
- Activity filterable by derivation path, with the full path shown on every
  address and output: `m/86'/0'/0'/1/7`.
- **Labels** on payments and addresses, stored in BIP-329's format so they can
  be exported and imported anywhere else.
- Every address you have handed out, which are used, what each received, and
  which have been paid more than once.

**Hardware wallets**
- Import a watch-only wallet from a Ledger, Coldcard or Specter over USB — no
  Python, no HWI installation, no daemon.
- All four paths read from the device in one prompt.
- Signing on the device is **not built yet**; see below.

**Keys**
- The seed is sealed with XChaCha20-Poly1305 under a key derived from your
  password with Argon2id (256 MiB, 3 passes, 4 lanes) and is decrypted only at
  the moment of signing — never held open while the wallet is on screen.
- Watch-only wallets can have a password too. They hold no keys, so there is
  nothing to decrypt — the password seals a known value instead, which gives a
  hardware-wallet or descriptor wallet something to fail against rather than
  leaving its whole history open to anyone who opens the app.
- Idle auto-lock, and lock when the computer goes to sleep.

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

- **Signing on a hardware device.** A device-imported wallet can receive and
  build payments but cannot spend. PSBT export and import is designed
  (`PSBT.md`) and not written.
- **Multisig.** Single-signature only.
- **More than one recipient** per payment.
- **Amounts typed in dollars.** They can be read in dollars.
- **Storing anything the node downloads.** The version of kyoto Sieve uses
  discards the data directory it is given — `data_path: _` in its own
  `node.rs` — so block headers and filters are fetched again on every start,
  and a second wallet on the same network gains nothing from the first. This
  is the largest single thing between Sieve and feeling quick.
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
sudo pacman -S --needed rust gtk4 libadwaita sqlite openssl
# Debian, Ubuntu
sudo apt install cargo libgtk-4-dev libadwaita-1-dev libsqlite3-dev libssl-dev
# Fedora
sudo dnf install cargo gtk4-devel libadwaita-devel sqlite-devel openssl-devel

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
| `SECURITY.md` | what is defended against, what is not, and what leaves the machine |

```sh
cargo test          # 131 tests, needing no network and no display
cargo fmt --check
cargo clippy -- -D warnings
```

## Networks

Mainnet and signet. Signet is the one to learn on: real block times, real
peers, coins worth nothing.

## Licence

MIT OR Apache-2.0, at your option. No warranty of any kind.
