# Sieve

A privacy-focused Bitcoin wallet for Linux. Rust + Relm4 + libadwaita on the front,
BDK with a BIP157/158 compact-block-filter light client on the back.

The name is the architecture: compact block filters sieve blocks locally, so the wallet
never tells a server which addresses it owns.

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
   the one place a colour is hardcoded: it needs dark modules on a light ground to scan, so it
   carries its own white ground in both themes via the `.qr-ground` class rather than sitting
   on a card, which is dark exactly when the code needs light. Consequently: no hardcoded
   colors, ever. Use Adwaita style classes (`suggested-action`, `destructive-action`, `error`,
   `warning`, `dim-label`, `pill`, `card`) and Adwaita named colors in any custom CSS, since those
   recolor themselves. Anything drawn by hand into a `gtk::DrawingArea` must read
   `StyleManager::is_dark()` and repaint on `AppMsg::ColorSchemeChanged`, which `app.rs` already
   wires up — a QR code drawn black-on-white disappears in dark mode.
6. **Argon2 never runs on the main thread.** `sender.spawn_oneshot_command(...)`. A blocked frame
   clock is a visible stall.

## Import model

Two axes, kept separate:

- **`CredentialKind`** — what the user pastes: a recovery phrase, a WIF key, or a descriptor.
  `carries_keys()` distinguishes the imports that could lose money from the one that cannot.
- **`ScriptType`** — where it is searched: BIP44/49/84/86. An import searches all of them,
  because one seed derives a different wallet on each path and guessing wrong finds nothing.
  Syncing all four costs no extra bandwidth: a filter covers a whole block regardless.

Each path is its own BDK wallet with its own SQLite file (BDK's table names are fixed), and
one `bdk_kyoto` node drives them all via `build_with_wallets`.

## The one non-Bitcoin connection

Fetching a price from Bitfinex is the only outbound request Sieve makes that is not Bitcoin
peer-to-peer. It carries no wallet data, but it discloses this machine's IP and when the
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

Recorded only once a sync has landed, because before that the connected set includes peers
the node is still evaluating and will drop for not serving filters. Peers that positively
advertise `COMPACT_FILTERS` are preferred, but not required: kyoto reports no service flags
for most connections, so demanding the flag would remember almost nobody.

So the list is "peers present through a working sync", not "peers known to serve filters".
Do not describe it as the latter — the flags to prove that are usually absent.

## Receiving

`next_unused_address` never returns a *used* address, but it returns the same unused one
every time — which links two payers who are each handed it. The refresh button calls
`reveal_next_address`, advancing the keychain and persisting the reveal so the new script is
watched. Show a fresh address per payer; never present one address as "the" wallet address.

The derivation-path list is a balance breakdown and must not show addresses: it duplicated
the receive row and read as address reuse.

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

The signer worker, the primary menu, and a BIP-39 passphrase option when creating a wallet
(import has one; creation passes `None`).
Nothing signs yet, and the balance never changes because there is no chain sync.

The full milestone plan (M0–M8, with the decisions that gate M1 and the mainnet gate at M8) is in
`ROADMAP.md`.
