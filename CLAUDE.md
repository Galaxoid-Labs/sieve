# Sieve

A privacy-focused Bitcoin wallet for Linux. Rust + Relm4 + libadwaita on the front,
BDK with a BIP157/158 compact-block-filter light client on the back.

The name is the architecture: compact block filters sieve blocks locally, so the wallet
never tells a server which addresses it owns.

## Passwords do not linger in fields

The unlock dialog empties its password row when a wallet is opened and again once a password
has done its job. It is one line, and it is not cosmetic: a password left in the box belongs to
the wallet you just left, and a prefilled field invites submitting it without looking. The same
applies to any field that ever holds a secret.

## Two secrets, two words

Never use these interchangeably in code, comments, or UI copy. Blurring them is how people
lose money.

| Term | What it is | Cost of losing it |
|---|---|---|
| **Password** | Encrypts the wallet file on disk. Always required. Sieve's own concept. | This copy of the wallet. The seed still recovers everything. |
| **Passphrase** | The optional BIP-39 25th word. Part of the seed; selects which wallet derives. | The money. A different passphrase silently derives a different, empty wallet. |

A wrong password produces an authentication error. A wrong passphrase produces an empty
wallet with no error at all — which is why the UI has to make the distinction obvious rather
than merely correct.

## Hard rules

These are not preferences. Violating one is a bug, not a style difference.

1. **Stock libadwaita widgets, always.** `adw::PreferencesGroup` + `ActionRow` / `EntryRow` /
   `SwitchRow` for forms, `adw::StatusPage` for empty states, `adw::ToolbarView` for the window
   shell, `adw::AlertDialog` for confirmations, `adw::Toast` for transient feedback. Do not
   hand-build a layout that an Adwaita widget already provides — the app must feel native in
   GNOME, and the HIG lives in these widgets. Reference: <https://developer.gnome.org/hig/>
2. **Secrets never enter the widget tree or a message type with a derived `Debug`.** Relm4 traces
   messages under `RUST_LOG=relm4=trace`. Any type carrying key material needs a hand-written
   redacted `Debug` (see `ui::unlock::Passphrase`).
3. **GTK objects are not `Send`.** Never move a widget into a command, worker, or thread. Commands
   return plain data; the component applies it on the main thread.
4. **The wallet is watch-only by default, and the seed is decrypted only at the point of
   use.** `bdk_wallet::ChangeSet` persists only `Descriptor<DescriptorPublicKey>`, so the
   SQLite store contains no private keys. Browsing balances and building PSBTs must not
   require an unlock.

   Unlocking does *not* hold the seed open. `wallet::unlock` decrypts the vault only to prove
   the password is right and to rebuild the descriptors if the databases are gone; the
   plaintext is dropped inside that function, and `UnlockOutput::Unlocked` carries nothing but
   `Paths` and a watch-only `Summary`. There is no field anywhere holding a decrypted seed for
   the session, and adding one is a bug.

   So every operation that genuinely needs key material asks for the password again and
   decrypts for itself: revealing the phrase does, and signing will. The only exception is the
   reveal screen holding the phrase in a `Zeroizing<String>` while it is on screen — which is
   the operation — and it is cleared when preferences closes.
5. **Light and dark follow the desktop, automatically.** `ColorScheme::Default` is supposed
   to follow the system on its own, but libadwaita's settings backend does not find the source
   in every session — on Hyprland the portal and gsettings both said `prefer-dark` and the app
   still came up light. So `org.gnome.desktop.interface color-scheme` is read directly and
   mirrored onto the style manager, and watched for changes. That is still following the
   desktop, by a route that works.

   There is now an Appearance preference — Follow the system, Light, Dark — defaulting to
   following the system. "Follow the system" still reads the desktop directly. The QR code is
   one of only two places a colour is hardcoded: it needs dark modules on a light ground to scan, so it
   carries its own white ground in both themes via the `.qr-ground` class rather than sitting
   on a card, which is dark exactly when the code needs light. The other is the balance card's
   ₿ mark, tinted by network — orange for mainnet, red for signet, green for the testnets — which
   is identity rather than decoration: a glance at the card should say which chain the money is
   on. Kept at a fifth of an alpha so the hue reads on a light or a dark card. Otherwise: no
   hardcoded colors, ever. Use Adwaita style classes (`suggested-action`, `destructive-action`, `error`,
   `warning`, `dim-label`, `pill`, `card`) and Adwaita named colors in any custom CSS, since those
   recolor themselves. Anything drawn by hand into a `gtk::DrawingArea` must read
   `StyleManager::is_dark()` and repaint on `AppMsg::ColorSchemeChanged`, which `app.rs` already
   wires up — a QR code drawn black-on-white disappears in dark mode.
6. **Argon2 never runs on the main thread.** `sender.spawn_oneshot_command(...)`. A blocked frame
   clock is a visible stall.

## Watch-only wallets, and where hardware signers fit

A wallet whose keys live on a device has no vault: the descriptors are public, so there is
nothing to seal and no password to ask for. `Meta::watch_only` records it, `Paths::is_initialised`
accepts metadata without a vault, and `open_watch_only` is the counterpart to `unlock` for a
wallet with no secret. **"Unlocked" and "holds keys" are now separate questions** — code that
assumes the first implies the second is wrong.

`wallet::watch::parse` turns what a device exports into the pair of descriptors BDK wants. It
takes a multipath descriptor (`…/<0;1>/*`), a single-path one ending `/0/*`, or a bare extended
key with its origin, and infers the script type from the purpose in that origin — 84h is native
segwit and so on. A bare key with no origin is **refused**, not guessed: the path is what says
which addresses to look for, and a wrong guess produces a wallet that finds nothing and looks
broken rather than wrong.

`src/hardware.rs` owns the USB side, on `async-hwi` (pure Rust — no Python, no HWI install, no
daemon). Importing asks a device for its master fingerprint and the extended public key at
`m/purpose'/coin'/0'`, and assembles the same descriptor a person could have pasted: importing a
hardware wallet and importing a descriptor land in the same place on purpose. A test asserts
that what `hardware::descriptor` produces is what `watch::parse` reads, for all four script
types — the two halves of that seam would otherwise only meet with a device on the desk.

Each backend is enumerated separately and a failure in one is logged rather than returned: a
machine with no serial ports must not report "no devices" because the Specter probe failed while
a Ledger sits right there. On Linux a device is invisible without udev rules, which is the
commonest reason for an empty list, so the interface says so instead of leaving a blank.

Still to come, following ecash-splitter's `ecx-signer`: signing. PSBT is the seam — BDK never
talks to a device, its output is a PSBT and the device's input is a PSBT. Export and import over
files covers every air-gapped device with no drivers at all; `async-hwi`'s `sign_tx` covers USB.
Whatever a device hands back is untrusted and re-verified before broadcast.

## Import model

Two axes, kept separate:

- **`CredentialKind`** — what the user pastes: a recovery phrase, a WIF key, or a descriptor.
  `carries_keys()` distinguishes the imports that could lose money from the one that cannot.
- **`ScriptType`** — where it is searched: BIP44/49/84/86. An import searches all of them,
  because one seed derives a different wallet on each path and guessing wrong finds nothing.
  Syncing all four costs no extra bandwidth: a filter covers a whole block regardless.

Each path is its own BDK wallet with its own SQLite file (BDK's table names are fixed), and
one `bdk_kyoto` node drives them all via `build_with_wallets`.

## Tor

Off by default, and when it is on it covers everything: peer connections, the price lookup and
the fee lookup all go through the same SOCKS5 proxy. `App::tor_proxy` is the only reader of the
setting, so no call site can forget it.

**Sieve provides Tor, rather than asking for it.** In order: a proxy already listening on 9050
or 9150 is borrowed and never touched; otherwise a `tor` binary is found and *started by us*,
with its own data directory, a port Tor picks, and `__OwningControllerProcess` so it exits if
Sieve dies without stopping it. The binary is looked for at `$SIEVE_TOR`, then beside the
executable — which is where `packaging/com.galaxoidlabs.Sieve.yml` puts it — then on `PATH`.

That last part is why the Flatpak manifest exists: it builds Tor from source into
`/app/bin/tor`, so someone who has never installed Tor gets it. A development build has no such
neighbour until `scripts/fetch-tor.sh` puts the official Expert Bundle in `target/<profile>/tor/`
— run it once, or the switch in preferences correctly refuses because there is nothing to
start. A Tor that ships its own libevent and OpenSSL needs `LD_LIBRARY_PATH` pointed at them,
which `daemon::ensure` does when it sees them beside the binary. Sparrow ships Tor binaries,
Wasabi falls back to a bundled copy, Feather bundles it too; expecting the user to install a
daemon is what Bitcoin Core and Electrum do, and a node is not a wallet. Embedding
[arti](https://tpo.pages.torproject.net/core/doc/rust/arti_client/) instead is possible but
worse for now: its own SOCKS listener sits behind `experimental-api` and outside semver, so it
would mean writing a local SOCKS server over `arti_client` streams, plus a client that
terminates the process on an obsolete consensus.

**Proving the proxy is Tor.** Anything can listen on 9050. `RESOLVE` (0xF0) is Tor's extension
to SOCKS5, not RFC 1928, so a plain SOCKS proxy answers `0x07 command not supported` and only
Tor answers with an address. `tor::check` resolves `example.com` — deliberately not a Bitcoin
seed, since this runs whenever preferences opens and should say nothing about what the app is
for. Turning the switch on runs the check first, and a failure puts the switch back rather than
leaving the app looking as though it is on Tor when it is not.

**The DNS leak that had to be closed.** kyoto resolves DNS seeds itself when it has no peers to
try, and that lookup is *not* proxied — reading bip157 0.6.3, `is_proxy` gates only BIP324 v2
transport, nothing else. Since kyoto ignores `data_dir`, its peer database is empty on every
launch, so the leak would fire every start. So `tor::resolve_seeds` resolves the same hostnames
through the proxy and hands the results to the builder as configured peers. The `x49.` prefix
asks a seeder for nodes that serve compact filters. If nothing resolves and there are no
remembered peers, `Session::start` fails rather than letting kyoto fall back to the resolver.

**Fail closed, and only fail closed.** If Tor is on and cannot be brought up, nothing connects:
no session starts, and the wallet shows a banner saying so with a Try again button. Going out
over the clear because Tor was unavailable is the one thing this must never do quietly. The
exception is someone flipping the switch on right now — that request cannot be honoured, so the
switch goes back, which is not the same as silently abandoning a setting they already had.

A Tor left behind by a previous run holds the data directory and Tor will not share one, so the
next start used to fail with "another Tor process is running". `daemon::ensure` now adopts a
leftover that still answers (via `socks.port`, since it is on a random port `detect` would never
find) and stops one that does not (via `tor.pid`).

Onion addresses are only handed to the node when Tor is on: dialling one directly spends a
connection attempt on something that cannot work.

**Two things that bit, and must not come back.** The watchdog that rescues a Tor which never
finishes starting has to be told when to stand down — an earlier version simply slept and then
killed, so every Tor Sieve started was shot exactly two minutes later. And when the proxy goes
away, kyoto retries as fast as the failures return, which costs a whole CPU; `App::check_tor`
runs on the periodic tick, stops the light client, and brings Tor back rather than letting it
spin.

**Tor costs a weaker filter quorum, and that is stated where it is chosen.** `REQUIRED_PEERS`
is not only how many connections to hold: kyoto passes it into `FilterHeaderAgreements`, so it
is also how many peers must agree on the filter headers before a single filter is downloaded.
Eight is reachable directly — measured — and never arrived through Tor exits, where filter
nodes are scarce, so a scan sat at exactly twenty-five percent for ever: filter headers
complete, quorum unreachable, no filters. Two over Tor, eight without it.

**What Tor does not cover:** the mempool.space link opens the system browser, which is outside
this process. And proxied connections lose BIP324 v2 encrypted transport, because kyoto
disables it for them.

## The non-Bitcoin connections

Two, both opt-in and both disclosed in the row that enables them: a price from Bitfinex, and
fee rates from mempool.space. Both are routed through Tor when Tor is on — a wallet that
tunnels its peers and then asks a price service over the clear is worse than one that never
claimed to. Neither carries wallet data; both reveal this machine's IP, and
the fee request additionally signals that a payment is imminent.

Fetching a price from Bitfinex is the older of the two. It carries no wallet data, but it discloses this machine's IP and when the
wallet was opened, so it is off by default, stated plainly in its preference row, and never
made on a test network. If any other outbound call is ever added, it gets the same treatment:
opt-in, disclosed, and justified in the row that enables it.

## Known upstream gap: no header persistence

bip157 0.6.3 accepts a `data_dir` and ignores it — `Node::new` destructures the config as
`data_path: _` and the field is read nowhere else. So block headers live in memory only and
are re-fetched on every launch. `chain/<network>/` is created in anticipation of a version
that uses it; do not assume anything is in there.

The impact is smaller than it sounds. `ScanType::Sync` starts the node's chain at the
*wallet's* checkpoint, which BDK does persist, walked back 7 blocks for reorg safety — so a
restart fetches seven blocks, not the chain. Only a wallet still sitting at its birthday pays
for a long header walk, and only once.

What a restart actually costs is peer discovery: roughly a minute of DNS seeding, connecting
and handshaking before anything syncs. If start-up feels slow, that is where the time is —
not headers.

## Remembered peers

**Peers connected while filters are syncing, and only those.** That is proof rather than
hearsay: kyoto drops any peer whose version message lacks `NODE_COMPACT_FILTERS | NODE_NETWORK`
as soon as it is past the block-header phase (`node.rs`, the version handshake), so a peer still
connected during the filter phase serves filters by construction. The service flags `peer_info`
reports are absent for most connections and prove nothing either way — pinning on those alone
meant pinning nothing.

Not gated on a finished sync. A recovery scan can run for an hour, and the peers doing that work
are the ones worth having next time; waiting for the end meant learning nothing from a scan that
was interrupted.

The rule this replaced remembered whatever was connected whenever no flag could be confirmed.
It cost a day to find: those addresses are handed to the node *first* on the next start, they
take the connection slots, and a peer that cannot serve a filter is worse than no peer at all to
a wallet whose entire sync is filters. A scan would sit with seven connections and nothing to
download from.

The file carries a version and a `serves_filters` claim, so lists written by that old rule are
no longer believed — not deleted, since that is not this code's decision, simply not read.
Seeded peers (asked for with the `x49`/`x849` service bits) are offered to the builder before
remembered ones.

Onion addresses are remembered alongside plain ones and only offered when Tor is on; dialling
one directly spends an attempt on something that cannot work. `tor::onion` encodes and decodes
them, checksum included, so a corrupted entry is dropped rather than dialled.

## Receiving

`next_unused_address` never returns a *used* address, but it returns the same unused one
every time — which links two payers who are each handed it. The refresh button calls
`reveal_next_address`, advancing the keychain and persisting the reveal so the new script is
watched. Show a fresh address per payer; never present one address as "the" wallet address.

The derivation-path list is a balance breakdown and must not show addresses: it duplicated
the receive row and read as address reuse.

## The Bitcoin logo

`data/icons/hicolor/scalable/apps/bitcoin-logo.svg` is the logo itself, public domain — created
by Satoshi Nakamoto and distributed as such by Wikimedia Commons (File:Bitcoin.svg). Vendored
rather than fetched, so the build has no network in it, and drawn as it is meant to be drawn:
the tilt and the proportions are the mark, and a font's ₿ is only an approximation. It carries
its own colour, so no tint is applied to it.

The balance card's watermark is still the glyph, deliberately — that one is tinted by network,
and tinting a logo that is already orange would say nothing.

## Layout

```
src/
  main.rs            app ID, process hardening, RelmApp bootstrap
  app.rs             root component: adw::ApplicationWindow + ToolbarView + screen stack
  ui/
    unlock.rs        passphrase entry; Argon2 off-thread via spawn_oneshot_command
    wallet_page.rs   placeholder for the unlocked view
  vault/
    mod.rs           sealed seed file: Argon2id KEK wraps a random DEK, XChaCha20-Poly1305
    atomic.rs        crash-safe write: tmp -> fsync -> rename -> fsync dir
  wallet/
    mod.rs           BDK + bdk_kyoto. No GTK types here, ever.
```

## Vault format

`magic | header_len | header JSON | salt | nonce | wrapped DEK | nonce | ciphertext`

The header (KDF cost, network) is authenticated as AEAD associated data. Without that binding an
attacker who can write the file could downgrade `m_cost` to 8 KiB and brute-force the passphrase.
Do not change the format without bumping the magic byte and writing a migration.

Defaults are 256 MiB / 3 passes / 4 lanes — measured at ~0.7s on desktop hardware, where
512 MiB / 4 passes cost 2.1s. Run `cargo test --release -- --ignored --nocapture kdf_cost`
to re-measure. Not user-tunable. Changing the default is safe: the parameters used to seal a
file travel in its header, so existing wallets keep opening.

Argon2 is unusable at `opt-level = 0` (~36s), so `Cargo.toml` optimises `argon2` and `blake2`
even in dev builds. Do not remove those profile overrides.

## Versions

- relm4 0.11, gtk4 0.11, libadwaita 0.9 — all reached through `relm4::{gtk, adw}` re-exports.
  Do **not** add direct `gtk4` / `libadwaita` dependencies; two copies of gtk4 in the tree produce
  baffling trait errors. Check with `cargo tree -d`.
- `gnome_46` feature = libadwaita 1.5 baseline. Chosen for reach, not for the dev machine (which
  has 1.9.3). Raising it to `gnome_49`/`gnome_50` drops support for older distros — deliberate
  decision, not a routine bump.
- bdk_wallet 3.1.0 + bdk_kyoto 0.17.0 — the pairing bdk-ffi ships, known to work together.
- MSRV 1.93 (relm4's).

## Commands

```sh
cargo run                       # launch
cargo test                      # vault round-trip and tamper tests
RUST_LOG=sieve=debug cargo run   # app logging
GTK_DEBUG=interactive cargo run  # widget inspector
```

## Relm4 usage

There is a `relm4` skill with the framework reference (view! macro attributes, trait selection,
factories, Adwaita patterns). Consult it rather than guessing at macro syntax.

## Not yet built

The primary menu, and a BIP-39 passphrase option when creating a wallet (import has one;
creation passes `None`) or when signing with a wallet that was imported with one.


The full milestone plan (M0–M8, with the decisions that gate M1 and the mainnet gate at M8) is in
`ROADMAP.md`.
