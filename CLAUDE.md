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
   the only place a colour is hardcoded, and everything hardcoded is there: it needs dark modules
   on a light ground to scan, so it carries its own white ground in both themes via the
   `.qr-ground` class rather than sitting on a card, which is dark exactly when the code needs
   light — and the Bitcoin mark drawn in its middle is coloured by chain, bitcoin's own orange
   with mempool.space's purple for signet and green for the testnets, so the association is
   borrowed rather than invented.

   **That surface is why those colours are allowed.** A QR code's ground is white whatever the
   desktop is doing, so a colour drawn on it has a *known* background and can be chosen for
   contrast once. The balance card's ₿ mark used to be tinted the same way and is not any more:
   it sits on a card that is light or dark depending on the hour, a fixed colour had to work
   against both at a fifth of an alpha, and a dark signet purple simply disappeared. It takes
   the desktop accent on every chain now. The chain is named in words in the header directly
   above the card, so the tint was answering a question the screen had already answered.
   Otherwise: no
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

What is **not** built yet: signing. Every command Sieve sends a device is a read —
`enumerate`, `get_version`, `get_master_fingerprint`, `get_extended_pubkey`, and
`display_xpub(false)` to suppress a prompt — so a hardware wallet can fund and watch but not
yet spend, and the send flow says so rather than failing at the last step. `ROADMAP.md` M4a
lists what closing that costs, including why "verify address on device" works for taproot
today and needs a registered wallet policy for the other three paths.

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
  Syncing all four costs no extra bandwidth: a filter covers a whole block regardless. A
  wallet *created* here watches only taproot and native segwit, because those are the only
  paths it will ever hand an address out on and a new wallet has no history anywhere else.

**Watching and handing out are separate questions, and `can_receive` is the difference.**
Legacy is watched on an import — money already on BIP44 has to be found and spent — and no
wallet ever hands out a fresh `1…`. The receive picker asks `offers_path`, not `has_path`,
and `AppMsg::RevealAddress` refuses it again: a rule kept only by the visibility of a row is
a rule the view is keeping, and this one decides which script somebody is about to give a
payer. Native segwit is the *default* selection everywhere, taproot one row down — `bc1q` is
refused by nothing, and the cost of `bc1p` lands on the person being paid.

`wallet::add_script_types` starts watching a path that was not being watched, for the case
that reads as lost money: a phrase restored into another wallet, used on some other path,
brought back. It needs the password, because those descriptors exist nowhere and only the
seed makes them, and it refuses a watch-only wallet outright. Adding an account at the
birthday is all it takes to force a full rescan — `build_with_wallets` starts the node at the
lowest checkpoint of any wallet it is given.

Each path is its own BDK wallet with its own SQLite file (BDK's table names are fixed), and
one `bdk_kyoto` node drives them all via `build_with_wallets`.

## Typing a phrase back in

`src/ui/phrase.rs`. A box per word rather than one line of space-separated text, because
the two mistakes people actually make — a word in the wrong place, and a word that is not a
BIP-39 word — are both invisible in running text. **Recovery phrase is `KINDS[0]`**, which is
what the import form opens on; the order of that array is the default, so reordering it is how
the default changes.

Four rules, each of which is the reason a line of that file exists:

- **A completion must never invent a word.** BIP-39's list is built so four characters
  identify a word, so expanding a unique prefix on space is safe — but only when exactly one
  word matches. A test walks all 2,048 and asserts every completion is itself a real word,
  because a wrong one is submitted as part of a seed and derives a different, empty wallet.
- **`Word` carries a redacted `Debug`**, for the reason `Face` does. These messages carry a
  seed one word at a time, and relm4 traces every message.
- **A valid phrase is said out loud.** The status line reads `N of 12 words`, then names a
  word that is not on the list, then — when every word is real and the checksum still fails —
  says one is out of order. A correct phrase gets told it is correct. That is the whole
  point: the checksum exists so a mistyped word is caught here rather than becoming an empty
  wallet that looks exactly like a correct one.
- **The entry text is pushed only when it disagrees with the model.** A `#[watch]` would
  `set_text` on every keystroke and move the caret to the end, making the middle of a word
  uneditable. Hence `apply`, and hence `update_with_view` rather than `update`.

Pasting works into *any* box: whitespace means a boundary, the rest spills into the boxes
after it, and a 24-word phrase pasted into a 12-box grid grows the grid.

## Where a phrase's randomness comes from

`getrandom::fill` — the `getrandom(2)` syscall — the same call the vault uses for its salt,
its nonces and its data key. **One entropy source in the program, and this is it.**
`Mnemonic::generate` would have supplied its own from `rand`'s `ThreadRng`, which is a real
CSPRNG seeded from the OS and was never unsafe; but it meant the one irreplaceable secret
came from userspace while everything else came from the kernel, and one source is easier to
argue about than two. `generate_mnemonic` asks for the 32 bytes itself and hands them to
`generate_with_entropy`.

A failure to read entropy stops wallet creation. It cannot be worked around: a phrase from a
fallback nobody chose is exactly the silent weakness that looks identical to a good wallet.

**Dice are mixed in, never substituted.** `generate_mnemonic_with_rolls` computes
`os_bytes XOR SHA256(rolls)`, so no roll count and no loaded die can produce a phrase weaker
than the OS alone would have given. Replacing the OS bytes with `SHA256(rolls)` is the one
thing that must never be built here — it is verifiable, and it hands somebody a way to seal a
wallet nobody can afford. `DICE.md` records why, including that Electrum shipped exactly that
and withdrew it. The test that guards the whole design asserts the *same rolls twice give
different phrases*.

The rolls are key material until the phrase exists. `Face` carries a redacted `Debug` for the
same reason `Passphrase` does: relm4 traces every message, and a derived `Debug` would write
the sequence to the log one roll at a time.

## Tor

Off by default, and when it is on it covers everything: peer connections, the price lookup and
the fee lookup all go through the same SOCKS5 proxy. `App::tor_proxy` is the only reader of the
setting, so no call site can forget it.

**Sieve provides Tor, rather than asking for it.** In order: a proxy already listening on 9050
or 9150 is borrowed and never touched; otherwise a `tor` binary is found and *started by us*,
with its own data directory, a port Tor picks, and `__OwningControllerProcess` so it exits if
Sieve dies without stopping it. The binary is looked for at `$SIEVE_TOR`, then beside the
executable — which is where `packaging/com.galaxoidlabs.Sieve.yml` puts it — then on `PATH`.

**Nothing puts a binary beside the executable any more.** That step in the lookup order still
works and stays; what has gone is anything that used it. It was there for a Flatpak that builds
Tor into `/app/bin/tor`, and the plan is native packages and no Flatpak — so
`packaging/com.galaxoidlabs.Sieve.yml` and `scripts/fetch-tor.sh` are dead, kept only until the
AUR package is proven because they are the sole record of how to bundle Tor.
`scripts/fetch-tor.sh` is still how a development machine without a distribution `tor` gets
one, which is why `daemon.rs` names it. What is left is the ordinary case and it is enough:
`find` looks on `PATH`, so a distribution's own `tor` package is all anybody needs, and a
development machine with `tor` installed behaves exactly like a packaged one. A Tor that ships
its own libevent and OpenSSL still needs `LD_LIBRARY_PATH` pointed at them, which
`daemon::ensure` does when it sees them beside the binary. Sparrow ships Tor binaries,
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

## Where a restart begins

| Wallet state | Node starts at |
|---|---|
| Scan completed | A few blocks below the tip — BDK's checkpoint is at the tip, so `ScanType::Sync` and `walk_back_max_reorg` |
| Scan interrupted | `Meta::scanned_to`, the recorded resume point |
| Never scanned | The birthday |
| Made here, never connected | The tip, walked back — see below |

The middle row is the one that needed building. `bdk_kyoto` matches each filter as it arrives but
only produces a wallet update on `Event::FiltersSynced` — the end of the *whole* sync — so BDK's
checkpoint never moves mid-scan and an hour of scanning used to leave no trace at all.

`Meta::scanned_to` and `scanned_hash` are that trace: a checkpoint is a height *and* a hash, and
the node will not take one half. The height is derived from kyoto's progress fraction, which is a
float rather than a height, and then pulled back by `SCAN_MARGIN` (2,016 blocks) — a resume point
past where the scan truly reached would skip blocks, and skipped blocks are missing money.
Rescanning a difficulty period costs seconds.

**A wallet made here starts near the tip, and could not before.** A checkpoint is a height
*and* a hash, and a hash only comes from the chain — so creation, which happens offline, could
only fall back to the newest checkpoint compiled into the binary. On a chain whose only
checkpoint is genesis that meant scanning everything, for a wallet that by construction has no
history at all.

`create` records `Meta::created_at` and sets `birthday_pending`, keeping the compiled
checkpoint as a floor. `Session::adopt_tip_birthday` then asks the node for its tip, computes
`Meta::tip_birthday`, and asks for the header at that *exact* height for its hash — not the
tip's own hash, because a checkpoint whose height and hash disagree is refused and one taken
from the wrong block is worse than refused. It persists, clears the flag, and calls
`rescan_from` so the first run benefits rather than only the second. Every failure leaves the
wallet on the floor: slower, always correct.

**Only `create` sets it, and the arithmetic errs early.** An imported seed may have been
spending for years, so moving *its* birthday forward would skip the blocks holding its coins.
And a wallet made here can still be paid before it is ever online — hand out an address, get
paid, open Sieve a fortnight later — so the birthday is walked back by the time since creation
*plus* `BIRTHDAY_MARGIN` (2,016 blocks). This is the one piece of arithmetic here that can lose
money, which is why its test is about not moving far enough rather than about accuracy.

**Do not try to hand the node a stored header chain.** `bdk_kyoto::build_with_wallets` sets
`self.chain_state(ChainState::Checkpoint(cp_min))` one line before it builds, so any snapshot is
discarded — silently, which is how a store that wrote, merged, validated and loaded 900,000
headers came to do nothing at all for a whole evening. That store has been removed: it was
consulted for exactly one block hash, which now lives in the wallet's metadata beside the height
it pins. Moving the recovery checkpoint is the only lever this API offers.

Reusing headers across wallets on a network would mean building through `Builder::build()` and
reimplementing the glue that turns filters into wallet updates — the part that decides which
blocks get fetched. It would save the header walk, which is ~77 MB and thirteen minutes, against
filters at 3–4 GB and hours. The expensive phase is the one resume already protects.

## Known upstream gap: kyoto ignores `data_dir`

bip157 0.6.3 accepts a `data_dir` and ignores it — `Node::new` destructures the config as
`data_path: _` and the field is read nowhere else. So block headers live in memory only, and
without help they are re-fetched on every launch and by every wallet.

**There is no help, and the attempt to write some was removed.** A store that wrote, merged,
validated and loaded 900,000 headers did nothing at all, because `build_with_wallets` sets its
own chain state one line before it builds and discards any snapshot handed to the builder. See
"Where a restart begins" above; the note lives in `node.rs` where somebody would otherwise try
it again.

The impact is smaller than it sounds. `ScanType::Sync` starts the node's chain at the
*wallet's* checkpoint, which BDK does persist, walked back 7 blocks for reorg safety — so a
restart fetches seven blocks, not the chain. Only a wallet still sitting at its birthday pays
for a long header walk, and only once.

What a restart actually costs is peer discovery: roughly a minute of DNS seeding, connecting
and handshaking before anything syncs. If start-up feels slow, that is where the time is —
not headers.

## Remembered peers

**The two stages have different peer rules, and the interface says so.** Block headers are a
strict chain — each `getheaders` locator depends on the previous answer — so there is nothing to
parallelise, and kyoto holds exactly one connection while it walks them (`NodeState::Behind => 1`),
as Bitcoin Core does. Filter support is irrelevant to headers, so it is not asked for, which is
why a header-stage peer usually reports no services at all. Filters are the opposite: hundreds of
thousands of independent items, each verifiable against its committed header, so the node opens up
to `REQUIRED_PEERS` and pulls them in parallel — and disconnects everyone lacking
`NODE_COMPACT_FILTERS | NODE_NETWORK`, which is the eviction people watch and wonder about.

None of that is a fault, and all of it looks like one unexplained, so the peer count and the peers
list both say which stage they are describing.

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

The picker defaults to **native segwit** on every wallet that watches it, and never offers
legacy at all. See the import model above for why those are two different rules.

## Coins, and holding one back

`wallet::labels` is the store for both: BIP-329 keys a label on `txid:vout`, and Sieve writes a
name and a `spendable` flag against that key.

**A coin's name is its own first, inherited second** — the coin, then the payment that brought
it in, then the address it landed on. Inheritance alone gives every coin out of one transaction
the same name, which is exactly when a name has to tell them apart.

**Freezing is `spendable: false`, and it is a rule the builder keeps, not the view.**
`node::plan` adds frozen coins to `unspendable` beside the unconfirmed ones, so automatic
selection cannot reach for one to make the numbers work — which would break the link the coin
was frozen to avoid. The picker also refuses to tick one, but that is a courtesy.

**Three traps, all found by using it, all of which must stay shut:**

1. **Freezing must never be a one-way door.** `has_funds` decides whether the send form is
   drawn at all, so pointing it at the frozen-adjusted figure meant freezing everything
   replaced the form with "Nothing to send" — taking the only route to the padlock with it.
   It reads the *path* balance, and the row that says everything is frozen carries its own
   way through to the picker.
2. **Clearing a name must not clear the freeze.** `Labels::set` deleted the whole entry on an
   empty label, `spendable` included.
3. **The picker is built once and never rebuilt**, so anything a control changes has to be
   painted onto the row by that control. A padlock that wrote to the file and left the screen
   alone looked exactly like a button that does nothing.

**Copy about coins has to survive a second funded path.** Each path is its own wallet with its
own coins and a payment is built from one, so "all your coins" is false the moment another path
holds anything — false in the direction that sends somebody hunting for money that is not
missing. `coins_scope` says "in this wallet" or names the path, depending.

## Two directories, and which one is safe to delete

Wallets are in `wallet::data_root()` — `~/.local/share/sieve`. Preferences are
in `wallet::config_root()` — `~/.config/sieve`. Both come from
`directories::ProjectDirs`, which on Linux ignores the qualifier and the
organisation and uses the lowercased application name alone.

**The split is the point, and it is load-bearing.** One directory is
disposable and the other is not: every preference in `settings.json` can be set
again by opening the dialog, and a `vault.sieve` can be recovered only from the
recovery phrase. Keeping them apart is what lets the UI and the docs tell
somebody that one of them is safe to remove. `preferences_do_not_live_among_the_wallets`
asserts the two roots differ and that neither contains the other, so a change
that collapses them fails loudly rather than putting a vault inside the
directory Sieve describes as disposable.

Preferences moved there; they used to sit beside the wallets. `Settings::load`
calls `migrate` once — copy, then remove, never `rename`, since the two can be
on different filesystems, and the old file is left alone if the write fails.
`settings::path` is the only reader of either location, which is why the move
touched one function.

**Nothing cleans this up on uninstall, and nothing should.** No Linux package
manager removes files under `$HOME` — dpkg, rpm and pacman all run their
scripts as root against system paths, and Debian policy forbids it — so there
is no hook to implement even if it were wanted, and it is not: a repository
change or a distribution upgrade that removed a vault would cost somebody their
coins. So the app says where the files are instead, in a Files group in
preferences, and `PACKAGING.md` tells a packager not to add a cleanup hook to
be helpful. Deleting one wallet from inside Sieve already exists and is the
right route — **Remove this wallet**, confirmed and `destructive-action`.

## Three chains, and the two traps in adding one

Mainnet, signet and testnet4. Testnet4 is for somebody developing against the
chain their own software targets, and it was worth adding because it has the
peers for it — `cargo test -- --ignored --nocapture filter_peers` asks the
seeders and reports 23 filter-serving addresses against a `REQUIRED_PEERS` of
8, next to signet's 35. That number is the question: a chain with peers but no
*filter-serving* peers cannot be scanned at all, because `REQUIRED_PEERS` is
also kyoto's `FilterHeaderAgreements` threshold — see the Tor note above for
what that failure looks like, which is a scan frozen at exactly 25%.

`wallet::NETWORKS` is the one table. Both pickers build their model from it and
read their index back through `wallet::network_at`, so the list and the meaning
of an index cannot drift apart.

**Two things were silently wrong for a third chain, and both are now tests.**

1. **`.min(1)`.** Both screens clamped the picker index against a list of two,
   so a third chain became the second: choosing testnet4 would have made a
   *signet* wallet, with every screen then agreeing it was signet.
   `a_picker_index_is_the_chain_at_that_index`.
2. **A missing port.** `resolve_seeds_directly` matched the p2p port with a
   catch-all returning *no addresses*, so an unnamed chain seeded nothing at
   all — no error, no peers, a progress bar at zero, indistinguishable from a
   slow network. It is `p2p_port` now, and `every_offered_chain_has_a_port`
   guards it, with `every_network_asks_its_own_seeds_and_prefers_filter_nodes`
   doing the same for the seed list.

Checkpoints are genesis-only for testnet4, deliberately: a checkpoint needs a
real block hash, which cannot be derived here, and a wrong one is a node that
will not start. A genesis hash *can* be computed locally, so it is the one
written down, and `the_floor_checkpoints_are_the_real_genesis_blocks` checks
all three against `genesis_block` rather than trusting sixty-four hand-copied
characters. Scanning testnet4 from the beginning costs minutes, which is why it
needs no others and mainnet needs seven.

## Every field that edits something saved has a way out

`ui::cancellable_edit`. Adwaita's `EntryRow` gives an apply button and no
counterpart, so every field in Sieve that edited something already saved was a
one-way door — the exits were saving something or leaving the screen. On the
rows that *replace* a display line while they are open, leaving the screen was
the only exit at all.

Three rules, and the second is the one that is easy to get wrong:

- **Restoring is not optional.** Cancelling puts the saved value back. A cancel
  that shut the row and kept the typing would show the abandoned text the next
  time it opened, which looks like the app saved it.
- **`cancel` returns whether it had anything to cancel, and that decides whether
  Escape is swallowed.** A row that opens and shuts always has something to do,
  and must swallow it — an Escape that also reached the window would close the
  dialog the row is sitting in, which is a second, larger cancel nobody asked
  for. A settings row with nothing typed has nothing to undo, and swallowing
  there would break the one thing Escape is for in a preferences window.
- **A visible ✕ only on the rows that open and shut.** A permanent one beside a
  preference reads as "remove this setting".

The coin-naming field is the exception that needs nothing: it lives in an
`adw::AlertDialog`, which already has a cancel response.

## No `unsafe`, and the lint that keeps it that way

`#![forbid(unsafe_code)]` at the crate root. **`forbid` rather than `deny`**,
because `deny` can be switched off again by an `allow` on the offending item —
which is precisely what somebody in a hurry reaches for.

The four syscalls Sieve makes go through `nix`: `setrlimit(RLIMIT_CORE)`,
`prctl(PR_SET_DUMPABLE)`, `mlockall`, and `kill`. **This does not remove the
`unsafe` so much as relocate it** — a kernel boundary is unsafe by definition,
and `nix::set_dumpable` is a thin wrapper around the same call. What it buys is
who has read it: the gap `SECURITY.md` recorded was never the keyword, it was
that those blocks had been reviewed by nobody but their author.

`nix` was already in the tree transitively, so this added an edge and not a
crate — 310 before, 310 after — and the direct `libc` dependency went with it.
Returning `Result` is a real gain rather than ceremony: `setrlimit`'s return
value had been discarded, so a failure to disable core dumps was silent.

The tests used to write `$SIEVE_TOR` to inject a stand-in Tor, which Rust 2024
made `unsafe` for a good reason — the environment is process-wide and writing
it races every other thread reading it. `ensure_in_using` and `find_binary_with`
take the path as an argument instead. **The variable was never the seam a test
wanted; it was the seam that happened to exist.**

## The name

**Sieve**, one word, and it stays that way. The GNOME naming guidance asks for
under fifteen characters, one or two simple nouns, no generic descriptor, and
ideally a physical object that an icon can depict — "Sieve Bitcoin Wallet"
fails three of those and "Sieve" satisfies all four, since a sieve is both an
object and what the program does to block filters.

What kind of program it is lives where a launcher looks for it: `GenericName`
in the desktop entry, `Comment` under that, and `Keywords` for search. Adding
it to `Name` would put it in the window title and the About window too, where
it is noise.

The one risk is that Sieve is also an email filtering language (RFC 5228). It
is a protocol rather than an application, so the collision is in search results
rather than in anybody's app menu — worth knowing before the name is on a
release.

## Following the desktop's accent

`src/palette.rs`. GNOME publishes an accent as one of nine names and
libadwaita turns it into `@accent_bg_color`, which every other accent colour
derives from — a GTK app gets that for free, including this one.

Omarchy is the exception, and it is what this file exists for. Its themes
carry a full palette in `colors.toml`, but `omarchy-theme-set-gnome` applies
only three settings: `color-scheme`, `gtk-theme` and `icon-theme`. It never
sets `accent-color`. So a machine themed catppuccin has purple folders, a
lavender terminal and stock GNOME blue buttons, and `dconf read
/org/gnome/desktop/interface/accent-color` is empty because nothing ever wrote
it.

Three decisions worth keeping:

- **One colour.** Only `accent_bg_color` is set; libadwaita derives
  `accent_color`, `theme_selected_bg_color` and the focus ring from it. The
  background was deliberately left alone: `window_bg_color` has a dozen
  relatives — cards, headerbars, dialogs, sidebars, shades — and replacing one
  of them leaves the rest mismatched.
- **The label colour is computed, because libadwaita hardcodes white.** That is
  right for all nine GNOME accents and wrong for a light one. White clears the
  3:1 contrast minimum only up to luminance 0.30, and GNOME's lightest accent
  sits at 0.299 — so the threshold is derived rather than chosen, and a test
  pins all nine to white and six real theme accents to dark.
- **Detected by the file, not the distribution.** The question is whether there
  is a palette to read. A missing or unreadable file falls back to whatever
  libadwaita would have done alone, and the hex is validated before it reaches
  a stylesheet.

Watched with a `GFileMonitor` rather than the gsettings signal: switching
between two dark themes changes the palette without changing `color-scheme`,
so nothing else would fire.

## The application icon

`data/icons/hicolor/<size>/apps/com.galaxoidlabs.Sieve.png`, at 16 through 512. Fixed-size PNGs
rather than one SVG because that is what the artwork is; the theme handles either.

It is compiled into the binary **and** installed by the package, which is not redundant: a build
run straight from the source tree has no hicolor directory to find it in, and an app whose own
About window shows a broken image looks broken. The gresource prefixes have to be exactly
`/com/galaxoidlabs/Sieve/icons/<size>x<size>/apps` — GTK searches `resource://{app-path}/icons`,
and anything else buries the file where it will not be found.

One name everywhere: the desktop entry, the About window, the welcome screen and the launcher
all ask for `com.galaxoidlabs.Sieve`. `ICONS` in `app.rs` lists every icon name the app uses and
warns at startup for any that does not resolve, which is how a missing one is caught before
somebody sees a placeholder rather than after.

The Bitcoin logo — public domain, from Wikimedia Commons — is back, and not as the app icon:
it is the mark in the middle of a receive QR code. It stopped being a stand-in for Sieve's own
icon and became a picture of what the code contains, which is the one thing it is actually
qualified to say.

**It is a PNG of the glyph, not the SVG.** `gdk::Texture::from_bytes` decodes PNG, JPEG and
TIFF itself and hands SVG to a gdk-pixbuf loader — librsvg's, and absent on plenty of machines
including the one this was written on. The load returned `None`, the code drew a blank where
the mark should be, and nothing said why. `bitcoin-logo.svg` stays beside the PNG as the source
it was rendered from, with the command in the doc comment. Only the glyph is an asset, because
only the glyph has a fixed colour; the circle under it is drawn in `qr::stamp` so the chain's
colour stays an argument rather than three more files.

The balance card's watermark is still the glyph in the app's own font, and it now takes the
desktop accent on every chain — see the colour rule above for why it stopped being tinted.

## Layout

```
src/
  main.rs            app ID, process hardening, the stylesheet, RelmApp bootstrap
  app.rs             root component: the window, the session, preferences, locking, Tor
  about.rs           the About window, with per-component licences
  fees.rs            fee rates from mempool.space, when that is switched on
  hardware.rs        USB signers over async-hwi. Reads only; no signing yet
  palette.rs         the desktop's accent, and Omarchy's whole palette
  peers.rs           peers that served filters last time
  price.rs           a price from Bitfinex, when that is switched on
  settings.rs        preferences on disk
  ui/
    onboarding.rs    make a wallet: password, phrase, dice, verification
    phrase.rs        typing a recovery phrase, one numbered box per word
    restore.rs       import one: phrase, key, descriptor, device
    unlock.rs        the wallet *password*; Argon2 off-thread via spawn_oneshot_command
    wallet_page.rs   the unlocked wallet: activity, receive, send, preferences
    send.rs          the send form, coin control, the confirmation
    reveal.rs        showing the phrase again, behind the password
    chooser.rs       the wallet list
    browser.rs       opening a link in the system browser
    qr.rs            drawing a receive code
  tor/
    mod.rs           the SOCKS5 client, and proving the proxy is Tor
    daemon.rs        finding, starting and adopting a tor process
    onion.rs         encoding and decoding onion addresses
  vault/
    mod.rs           sealed seed file: Argon2id KEK wraps a random DEK, XChaCha20-Poly1305
    atomic.rs        crash-safe write: tmp -> fsync -> rename -> fsync dir
  wallet/
    mod.rs           create, unlock, rescan, derivation paths. No GTK types here, ever
    accounts.rs      ScriptType, one BDK wallet per path, the portfolio
    node.rs          bdk_kyoto: filters, progress, peers, broadcast
    send.rs          building, signing and bumping transactions
    watch.rs         reading a pasted descriptor or xpub
    labels.rs        BIP-329 labels beside the wallet
    uri.rs           BIP-21 payment requests
```

`wallet_page.rs`, `app.rs` and `wallet/mod.rs` are the three largest files by a
distance. Nothing here is a placeholder any more.

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
cargo run                        # launch
cargo test                       # 213 tests; no network, no display
cargo clippy --all-targets -- -D warnings
cargo fmt --check
RUST_LOG=sieve=debug cargo run   # app logging
GTK_DEBUG=interactive cargo run  # widget inspector

# Slow ones, opted into by name:
cargo test -- --ignored --nocapture filter_peers
cargo test -- --ignored --nocapture qr_samples   # writes codes to look at
cargo test --release -- --ignored --nocapture kdf_cost
cargo test --release -- --ignored --nocapture repeated_words
```

The last measures how often a generated phrase repeats a word, against what the birthday
arithmetic predicts. A repeat is normal — about one 12-word phrase in 31 — and it is also what
a broken entropy path would look like, which is why the rate is measured rather than assumed.
Run it after anything touches how entropy reaches BDK.

## Relm4 usage

There is a `relm4` skill with the framework reference (view! macro attributes, trait selection,
factories, Adwaita patterns). Consult it rather than guessing at macro syntax.

## Not yet built

**Signing on a hardware device**, which is the whole of M4a and the largest hole: a
device-imported wallet can receive and build a payment and cannot spend one. PSBT export and
import as files does not exist either, so there is no air-gapped path at all — `PSBT.md` has
the design.

**Packages.** No `.deb`, `.rpm` or AUR package has been published, and the signing key is not
made. `PACKAGING.md` has the plan and `ROADMAP.md` M8 the order.

Smaller, and each with a note of its own: silent payments (`SILENT_PAYMENTS.md`, sending is
buildable and receiving is not), desktop notifications (`NOTIFICATIONS.md`, deliberately
deferred), coin freezing, and reading a BIP-21 request from a camera.

The full milestone plan (M0–M8) is in `ROADMAP.md`.
