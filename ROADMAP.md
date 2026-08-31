# Sieve development plan

Nine numbered milestones in dependency order, each ending with something usable, plus M4a
for hardware signers — which grew out of M4 once it was clear a device changes the whole
signing path rather than adding a step to it.

**Mainnet is not gated behind anything**, and this line used to say it was. Creating a wallet
and importing one both offer bitcoin first, behind a switch acknowledging that the software is
unreviewed. See the note at M8 for why a signet default was a gate that gated nothing.

## Decide before M1

These get baked into the vault format, the descriptors, or both. Changing them later means
a migration and a re-scan.

| Decision | Recommendation | Why |
|---|---|---|
| Script type | **BIP86 taproot** | Single-sig key-path spends are indistinguishable on-chain. Costs acceptance at a few older services that reject `bc1p`. |
| Dev network | **Signet** (regtest for tests) | Real block times and enough `NODE_COMPACT_FILTERS` peers to exercise sync honestly. |
| Phrase length | **12 words** by default, 24 offered | 128 bits is beyond brute force either way, so this stopped being a security decision and became a preference some people arrive with. The cost of 24 is real and falls on the person copying them down, which is why 12 is still the default. |
| Password vs passphrase | **Both, named distinctly** | The *password* always encrypts the file. The *passphrase* is the optional BIP-39 25th word. A wrong password errors; a wrong passphrase silently derives an empty wallet, so the UI must never blur them. |
| Wallet count | **One per vault** | Keeps unlock, sync, and the signer singular. The header carries a version, so multi-account stays open. |

## What is missing, in the order it hurts

Written after a full evening of using the wallet on mainnet, and kept current as the list is
worked through. The milestone in brackets is where the work belongs.

1. **Hardware signing and PSBT files.** [M4a] A device-imported wallet can receive forever and
   never spend, and there is no air-gapped path at all. The PSBT half is designed but not
   built — see `PSBT.md`, which also records what multisig would need and why it is a
   milestone rather than a feature.

Then, in rough order:

- **Nothing the node downloads is kept.** [M2] `bip157::Node::new` destructures its config
  with `data_path: _`, so the data directory Sieve hands it is discarded, and an attempt to
  keep headers ourselves failed for a second reason — `build_with_wallets` sets its own chain
  state one line before it builds, discarding any snapshot given to the builder. The note
  about that is in `node.rs` where somebody would otherwise try it again.

  What this actually costs is narrower than it was once written down as. `resume_point` moves
  the recovery checkpoint forward, which is the one lever the API offers, and a synced wallet
  starts from its own checkpoint — an ordinary warm start reaches a balance in about seven
  seconds and re-downloads no headers. The cost lands on a **rescan** and on **importing a
  wallet with history**, both of which re-fetch filters from the birthday, and on a second
  wallet on the same network gaining nothing from the first.

## Closed since that list was written

- **The desktop's own colours.** [M6] libadwaita follows GNOME's accent for free, but Omarchy
  publishes a whole palette in a file its GNOME integration never applies — so a machine
  themed catppuccin had stock GNOME blue buttons. `palette.rs` reads that file and hands
  libadwaita the accent, plus the background family when the theme's mode matches the scheme
  in force. Nothing about any particular theme is written down: it reads `mode`, `accent` and
  three surface colours from whatever the current-theme symlink points at, so a theme someone
  writes themselves works exactly like a shipped one. All 23 themes on the development
  machine map fully, a user's own among them.
  - **The surfaces are clamped into Adwaita's order, not checked against it.** Adwaita draws
    for view < window < raised. Refusing a theme that arrives otherwise threw away three that
    were not wrong: two set `lighter_background` equal to `background` — a flat look, and
    equal is not out of order — and vantablack's background is `#000000`, so its
    `dark_background` is necessarily *lighter*. Clamping is never wrong and is sometimes the
    only sensible reading. A theme whose colours are wholly inverted still falls back to the
    accent alone, since clamping would flatten every surface onto one colour.
  - **Light and dark come from the theme file, not from GNOME's copy of it.** Omarchy writes
    the mode into GNOME's settings as a separate step, and that copy was found stale —
    `prefer-light` under a dark theme, a state no single source can produce. Reading the mode
    where the colours are read means the two cannot disagree. GNOME's settings still answer on
    every other desktop.
  - **The Appearance picker is gone.** Light and dark belong to the desktop, not to one
    application, and choosing Light under a dark desktop theme put that theme's dark surfaces
    under libadwaita's light text. A read-only row says what is being followed.
  - Three bugs made this look like a theming problem when it was not: a `GSettings` dropped at
    the end of `init` took its subscription with it, so the scheme was read once and never
    again; the mode came from that stale copy; and the provider sat at `PRIORITY_SETTINGS`,
    where libadwaita defines the same names, so a tie broken by insertion order put a stock
    blue accent back. The last is only visible in the balance mark, which now reads
    `@accent_bg_color` — mainnet takes the desktop's accent while the test networks keep
    colours written down, since a theme with a green accent would otherwise make a mainnet
    wallet look like testnet.

- **Fee bump (RBF).** [M4] "Raise the fee" on the detail page of an unconfirmed payment you
  made — its own row, not a button crushed into a suffix. Two dialogs: a rate, floored at what
  the network will actually relay (the original's fee *plus* a satoshi per virtual byte for the
  replacement's own size — below that every node drops it, which looks exactly like being
  ignored), then every number restated and a password. `build_fee_bump` does the construction;
  signing and broadcast reuse the send path unchanged.
  - **The label follows to the new id without leaving the old one**, because either
    transaction can still be the one that confirms, and moving it would leave the winner
    unnamed.
  - **The original winning the race needs no handling.** Both spend the same coins, so only
    one can ever confirm; a confirmed transaction outranks an unconfirmed conflict in BDK's
    canonicalisation, and BIP-158 filters match on scripts spent as well as created, so the
    block that settles it arrives like any other. The wallet corrects itself.
  - **"Already raised" is derived, not remembered.** The graph keeps the transaction that
    lost, so `direct_conflicts()` reads the fact straight back: it survives restarts with no
    bookkeeping of ours and cannot drift from what happened.
  - A successful raise leaves the page showing the payment that no longer exists and opens the
    replacement's own page, rather than toasting over a stale screen.
  - *Still missing: cancel-by-replacement, and bumping on a hardware wallet, which needs
    device signing first.*
- **Coin control.** [M6] A Coins row on the send form pushes a picker: every coin on the path
  being spent from, largest first, each named by the label on the payment that brought it in
  or on the address it landed on — which is what the label work bought. Automatic stays the
  default and says so in its own row; **Choose for me** puts it back. The tally answers the
  question that matters at the moment of choosing: what is selected, whether it covers the
  amount **plus the fee** (an exact-amount payment adds the fee on top, so checking against
  the amount alone would tick and then fail to build), and — the point of the screen — that
  spending two coins together tells anyone reading the chain that one person held both, said
  in the names given to those coins. `manually_selected_only()` is what makes a selection
  binding; without it BDK treats it as a starting point and tops it up, silently undoing the
  decision. Max drains exactly the chosen coins, and "available" means the selection once
  there is one.
- **Idle auto-lock, and lock on sleep.** [M7] Preferences → Locking: never, 5, 15, 30 minutes
  or an hour, defaulting to **five** — a protection that ships off protects nobody who never
  opens preferences — plus a **Lock now** row. One `EventControllerLegacy` on the window in the
  capture phase counts as being there: every key, click, scroll and pointer move, passed
  straight through, so every screen added later is covered by construction. The clock is an
  `Instant` polled every fifteen seconds rather than a timer rebuilt on each keystroke. logind's
  `PrepareForSleep` over GIO's own D-Bus locks on the way down, because a closed laptop beats
  every idle timeout to it. Locking shuts the view and clears the reveal screen but leaves the
  node running: syncing is watch-only work, and stopping it would mean re-downloading filters
  to see a balance that was on screen a minute ago. The wallet says *why* it locked, since one
  that shuts itself silently reads as a fault.
- **BIP-21 payment URIs.** [M4] `wallet::uri` reads them. Pasting one into Pay to unpacks the
  address and amount into the fields, shows what the request said about itself above them, and
  names the payment after whoever the request said was being paid once it is sent. A `req-`
  parameter Sieve does not implement refuses the whole URI, as BIP-21 requires.
  `parse_address` unpacks one too, so nothing reaches a signature with a URI in the field —
  and the amount is never read from that path, so a request cannot quietly change what is
  being sent. *Still missing: reading one from a camera.*
- **Labels, on transactions and addresses.** [M5] `wallet::labels`, BIP-329 JSONL beside the
  wallet. A payment's name leads its activity row beside the direction (`Sent · Rent`) and is
  edited from a line on the detail page rather than a field standing open; an address is named
  on the receive screen before it is handed out, and that name then appears against it
  wherever it shows up in a transaction. Import and export from Preferences, so they are not
  trapped here — fields Sieve does not display survive the round trip. Unencrypted with
  `0600`, and the UI says so: a watch-only wallet has no password to encrypt them with, and
  the history beside them is readable anyway. *Still missing: input and output labels, which
  BIP-329 defines and Sieve preserves but does not show.*
- **Address list.** [M3] A page pushed from Receive: every address handed out, oldest first,
  with the label if it has one, the address monospaced, its full derivation path dimmed
  underneath, and either **Unused** or what it received. An address paid more than once says
  **Paid 3 times** in the warning colour — the privacy fact this screen can state that no
  other screen can. Derived from each keychain's revealed index rather than remembered, so it
  cannot drift from what the wallet is actually watching. Receive addresses only: change
  belongs to a payment rather than to anybody.
- **Rescan.** [M2] A button on the Sync group, behind an alert dialog that says what it costs.
  Clears each path's chain data and scans again from the birthday, keeping the descriptors and
  replaying the revealed index — without which a rescan would stop watching addresses already
  handed out. The old Refresh button, which re-read the chain view that a tick already
  re-reads every eight seconds, is gone.
- **An About window.** A real menu behind the header's hamburger (it used to open Preferences
  directly, which made About unreachable and the icon a lie). `adw::AboutDialog` with what
  Sieve is, the dual licence, a Credits section of rows that open each library's repository,
  and Legal split per component instead of one wall of text. The list is generated by
  `scripts/licenses.sh` from `cargo metadata`, so versions, licences and URLs are read from
  the crates themselves; a test asserts every named component is still a dependency.
- **Honest sync reporting, end to end.** The block-header phase has a real progress bar
  (estimated tip from a checkpoint plus the clock); the filter phase has kyoto's own; and the
  final phase — fetching the blocks the filters matched, which is where the last twenty
  seconds went with nothing on screen — now names itself and fills a bar against the count the
  last scan measured, recorded in `Meta.matched_blocks`. A wallet that has never finished a
  scan gets a spinner, because there is nothing honest to draw yet.
- **Money reads like money.** Dollar figures are grouped to the thousand everywhere
  (`price::usd`); addresses, transaction ids and block hashes are monospaced everywhere,
  including in the send confirmation; the activity list filters by derivation path; and the
  transaction detail shows the full path of each of the wallet's own outputs
  (`m/84'/0'/0'/1/7`), read from the descriptor's origin rather than assumed.
- **Review payment is disabled until the form describes a payment**, with a tooltip saying
  what is still missing, rather than being pressable and answering with an error.

## Milestones

### M0 — Scaffold — SHIPPED
Adwaita shell, vault (Argon2id KEK wrapping a random DEK at 256 MiB / 3 passes / 4 lanes,
XChaCha20-Poly1305, header bound as AAD), atomic writes, process hardening, eight vault
tests.

### M1 — Wallet creation and unlock — MOSTLY DONE
*Done when: create a wallet, close the app, reopen, unlock back into the same wallet.*

- [x] First-run detection routes to onboarding instead of unlock
- [x] A 12- or 24-word mnemonic via `bdk_wallet::keys::bip39`, chosen when the wallet is made
- [x] Display-once screen; three-word verification challenge before the wallet is created
- [x] The wallet *password* typed twice and at least eight characters — not the BIP-39
      passphrase, which is a separate field on a separate group and is confirmed separately
- [x] Seal to `vault.sieve`, derive BIP86 descriptors, initialise the BDK SQLite store
- [x] Unlock loads watch-only from the database; a lost database is rebuilt from the vault
- [x] KDF retuned to 256 MiB / 3 passes (~0.7s) after measuring; params travel in the header
- [x] Database is owner-only — it holds the xpub and full transaction graph
- [x] Restore from a recovery phrase, a WIF key, or (stub) a descriptor
- [x] Optional BIP-39 passphrase on restore, kept distinct from the wallet password
- [x] All four standard derivation paths searched on import, with a per-path breakdown
- [x] Mainnet selectable on import behind an explicit unreviewed-software acknowledgement,
      and **selectable when creating a wallet too** — which it was not, so every wallet made
      in Sieve was a signet wallet with no way to say otherwise. Bitcoin is first and default
      on both.
- [x] A BIP-39 passphrase when *creating* a wallet, and a choice of 12 or 24 words. Both on
      the first step, in a group of their own away from the password — the two things this
      wallet must never let anybody confuse. Both conditions this was waiting on are met: the
      phrase screen changes what it says when a passphrase is in play, because the words alone
      then restore an empty wallet, and verification asks for the passphrase back, compared
      exactly — spaces and capitals are part of the key. An empty passphrase with the switch
      on is refused rather than quietly meaning "none", since BIP-39 derives a different seed
      for `""` than for absent.
- [x] The wallet name reaches `create`. It was collected on the first step and passed as
      `None`, so naming a wallet while making it did nothing at all.
- [x] Show the recovery phrase again, for backing it up later — `ui/reveal.rs`, reached from
      the Recovery phrase row in preferences. Asks for the password, because the vault is the
      only place the phrase exists. The row is insensitive until the wallet is unlocked, and
      the words are dropped when preferences closes. A wallet imported from a key shows that
      key, with copy saying so, rather than appearing broken.
- [x] Descriptor / xpub watch-only import — no vault, no password, and the send tab says
      plainly that signing happens wherever the keys are.
- [ ] Signer worker owning the decrypted descriptor, one message at a time

The mnemonic gets the same treatment as `Passphrase`: `Zeroizing`, redacted `Debug`, never
crosses a component boundary as a message.

### M2 — Compact filter sync — DONE, except keeping what it downloads
*Done when: a funded signet wallet shows the right balance after a cold start.* It does, on
mainnet as well, from a cold start and from an interrupted one.

- `CbfBuilder` wiring; peer discovery via `lookup_host` (prefixes seeders with `x849`)
- `CbfNode::run()` on its thread; `CbfClient::update()` awaited in a Relm4 command
- Apply each `Update`, persist the changeset
- `ScanType::Recovery` with lookahead sized to wallet history — undersizing silently misses
  transactions rather than erroring
- Progress from the `Info` stream; `Warning` stream to an `adw::Banner`
- [x] A resume point (`Meta.scanned_to`), so an interrupted scan restarts where it stopped
      rather than at the birthday.
- [x] Rescan on demand, behind a dialog that says what it costs.
- [ ] Somewhere to keep what the node downloads. The library's own data directory is
      discarded, so this means either a newer kyoto that honours it, or taking over the glue
      between filters and wallet updates — which `node.rs` argues against, and which the
      arithmetic may eventually justify.

### M3 — Receive — DONE
- [x] `reveal_next_address` with persistence, and a QR rendered in-process — handing an address
      to an image service would disclose exactly what a block explorer lookup would.
- [x] BIP-21 URIs, written on this screen and read on the send side.
- [x] The issued-address list, with used/unused state, what each received, and reuse called out.
- [x] A name on an address, set before it is handed out.

### M4 — Send
- [x] Address and amount validation — wrong-network addresses get their own message, and
      amounts are read with integer arithmetic in whichever unit is on display.
- [x] Watch-only PSBT construction, so the form and the review cost nothing secret.
- [x] Password only at signing, in an `adw::AlertDialog` that restates every number.
- [x] Signing from the vault, checked against the account's descriptor first.
- [x] Broadcast via `Requester::submit_package`, then recorded locally as unconfirmed.
- [x] **Broadcast again**, on an unconfirmed payment. kyoto announces to exactly one random
      peer, which is plenty for an ordinary payment and a coin toss for anything a peer might
      refuse on policy — and the refusal is silent, BIP-61 `reject` being long gone. The first
      transaction Sieve sent carrying an `OP_RETURN` never reached a mempool; one press of
      this put it there. Above the fee rows on purpose: a payment nobody has seen usually does
      not need to pay more. Asked for rather than done on a timer, since each announcement is
      another peer told the transaction is probably ours.
- [x] Say what a broadcast actually knows. `submit_package` returns once a transaction is
      queued and announced; nothing ever reports acceptance. The error claimed "no peer
      accepted the transaction", which is a conclusion that call cannot reach.
- [x] Drain the wallet ("Max"), where the fee comes out of the amount.
- [x] Fee suggestion from `average_fee_rate`, fetched once per tip when the send form comes
      into view, with the block it came from named under the field.
- [x] Optional fee rates from mempool.space, off by default, disclosed where it is switched on.
- [x] A BIP-39 passphrase at signing time. `Meta` records *that* one exists — never its
      value — and both signing dialogs ask for it beside the password: the send confirmation
      and the fee bump, which is a second signing path that does not go through the send form.
      A wallet imported with a passphrase can now spend, which it could not before. `unlock`
      also refuses to rebuild missing databases for such a wallet rather than deriving an
      empty one from the vault alone: that rebuild *succeeding* is the failure worth
      preventing, since it hands back a working wallet with a zero balance.
- [x] Unconfirmed coins excluded from selection.
- [x] BIP-21 payment requests read on paste, including the amount and who is being paid.
- [x] Fee bump, with the race against the original explained rather than hidden.
- [x] Both replacement paths go through one builder, which enforces the rule BDK does not.
      BDK checks that a replacement's *rate* beats the original's by a satoshi per virtual
      byte; the network's rule is about the absolute fee — the original's, plus a satoshi for
      every virtual byte of the replacement. Those agree only when the sizes do, which is why
      a fee bump never tripped over it and a cancellation always would, having one output
      where there were two. Paying under it is invisible: every node drops the transaction and
      it looks exactly like being ignored.
- [x] Coin control, on its own screen, with the linking it avoids stated in plain words.
- [x] Cancel a payment by replacing it with one that pays yourself — a row beside "Raise
      the fee". Called "Try to cancel it" everywhere, because a sent payment is already out on
      the network and anybody can mine it; if the original wins, the money is gone as intended
      and the attempt costs nothing. The money returns on the *change* keychain: nobody sent
      it, so it is not a payment, and a receive address meant to be handed out should not be
      spent on it.
- [x] Data outputs (`OP_RETURN`), on a payment or on a transaction that pays nobody — one
      folded-away field, so the two cases fall out of whether a recipient was filled in rather
      than being a mode to choose. Capped at 80 bytes, counted in *bytes* rather than
      characters, because an emoji is four of them. Submitted exactly as typed and never
      trimmed: silently altering something somebody is about to make permanent is the one
      thing the field must not do. The warning changes with the form, since the two cases are
      not equally bad and the worse one looks safer — a transaction that pays nobody proves
      which outputs are yours, where a payment only lets somebody guess.
- [ ] Pay a silent payment address (BIP-352). Contained, needs no server, and the ordering
      falls out of coin control — see `SILENT_PAYMENTS.md`, which also records why *receiving*
      is blocked on tweak data a filter wallet cannot compute.
- [x] More than one recipient in a transaction. The first keeps its own row — it carries a
      pasted BIP-21 request and Max, which only means something with one person to send
      everything to — and the rest are "Also pay" rows that can be taken away again. Adding one
      releases Max and says why. The review dialog names every recipient with its own amount:
      a total without the addresses is a number nobody can check.

Exercised end to end on signet: built, signed, broadcast, shown as pending, and confirmed on
its own through ordinary filter sync — no explorer, no server told which transaction to watch.

### M4a — Hardware signers
*Done when: a payment can be built in Sieve, confirmed on a device screen, and broadcast, with
Sieve never holding a key.*

Working today:

- [x] Discovery over USB with `async-hwi` — pure Rust, no Python, no HWI install, no daemon.
      Ledger, Coldcard, Specter and Jade are compiled in; only Ledger has been exercised.
- [x] Reading `m/purpose'/coin'/0'` on all four script types and assembling the same descriptor
      a person could have pasted, so a device import and a descriptor import land in the same
      place. `display_xpub(false)` keeps it to one prompt instead of four.
- [x] Watch-only wallet from those descriptors: balance, activity, receive, and the whole send
      form up to the moment of signing.
- [x] A udev hint when the device is plugged in and invisible, and a plain message when a
      Ledger's Bitcoin app refuses a coin-type-1 path because the wallet is on signet.

Left to build:

- [ ] **Signing over USB** — `HWI::sign_tx(&mut Psbt)`. The PSBT is already built and reviewed
      by the watch-only path; what is missing is handing it to the device, a "confirm on your
      device" state that can be cancelled, finalizing, and broadcast. Today the send flow stops
      at an explanatory page for a watch-only wallet.
- [ ] **Verify address on device** — `display_address`. `AddressScript::P2TR(path)` works on a
      Ledger with no setup, so taproot wallets could have it immediately. The other three paths
      go through `AddressScript::Miniscript { index, change }`, which the device only answers
      for a **registered** wallet policy.
- [ ] **Wallet policy registration**, which the previous item needs: a one-time on-device
      confirmation that returns an HMAC, stored per wallet in `Meta`. It is also what lets a
      Ledger recognise its own change outputs when signing a non-taproot payment.
- [ ] **Record the device fingerprint in `Meta`**, so signing can refuse a device that is not
      the one this wallet was imported from instead of producing signatures that do not verify.
- [ ] **PSBT export and import as files**, for air-gapped use — a Coldcard on an SD card, a
      Jade over QR. No USB at all, and the only way some people will sign.
- [ ] **Accounts past `0'`**, and a passphrase-derived device wallet, both of which currently
      have no way in.
- [ ] Handle the device being unplugged, locked, or switched to another app mid-flow, rather
      than surfacing the raw `async-hwi` error.
- [ ] Exercise Coldcard, Specter and Jade at all. Their code paths compile and have never run.

Sieve sends a device five commands, all of them reads: `enumerate`, `get_version`,
`get_master_fingerprint`, `get_extended_pubkey`, and `display_xpub(false)` to suppress a
prompt. Nothing it can send writes to a device — a wiped Ledger showing "set up as new /
restore" was wiped by its own firmware, which is what three wrong PIN entries do.

### M5 — Transaction history
- [x] `adw::ActionRow` list, detail page on an `adw::NavigationView`, confirmation depth, fee
      paid, pending state, and the fee rate the payment actually got.
- [x] Filter the list by derivation path; the path named on each row when more than one is
      being watched.
- [x] Labels, in BIP-329's format, importable and exportable.
- [x] Search the activity list — by amount (as written on screen and as plain satoshis),
      address, transaction id, the name given to it, or anything it published. Matched
      anywhere in the value, not only at the start: an address is usually copied from
      something showing only its middle.
- [x] Amounts typed in dollars, not only read in them — a `$` toggle on the amount field,
      with the bitcoin figure shown under it. Switching converts rather than reinterprets:
      leaving "0.0002" in the box and relabelling it dollars would be a very different
      payment with no character changed on screen.
- [x] Export the public descriptors — the backup that risks nothing. BIP-380 form with its
      checksum and nothing wrapped around it, so there is no Sieve format to learn.
- [ ] Replaced state, which needs RBF to exist first.

### M6 — Privacy controls
- [x] Tor for every outbound connection — peers, price, fees — through a system SOCKS5 proxy,
      with the proxy verified as actually being Tor (the `RESOLVE` extension), and kyoto's
      unproxied DNS seeding replaced by seeds resolved through Tor.
- [x] Tor without asking the user to install it — Sieve starts one itself when nothing is
      listening, and `tor::daemon::find` looks on `PATH` as well as beside the binary, so a
      distribution's own `tor` package is all it takes. With no Flatpak in the plan, nothing
      bundles Tor any more: `packaging/com.galaxoidlabs.Sieve.yml` and `scripts/fetch-tor.sh`
      are dead and `PACKAGING.md` says when to delete them.
- [ ] `arti` instead of a child process, if its embedding story stabilises: today its SOCKS
      listener is behind `experimental-api` and outside semver, and an arti client terminates
      the process on an obsolete consensus.
- [ ] Onion peers: `peers.rs` stores `IpAddr`, so a remembered peer cannot be an onion
      address. kyoto dials them happily; only our own memory of them is missing.
- [x] Coin control, with the linking it avoids stated in the names given to the coins.
- [x] BIP-329 labels, importable and exportable.
- [ ] Coin freezing — BIP-329 already defines `spendable: false`, and the label file is
      already written, so this is a flag and a filter rather than a new store.
- [ ] Manual peer pinning with `whitelist_only`, and an audit that nothing but Bitcoin p2p
      leaves the machine.

### M7 — Lock and key hygiene

- [x] `PR_SET_DUMPABLE(0)` is lifted for the length of a file dialog and restored after.
      Setting it makes the kernel re-own `/proc/<pid>` to root, so `xdg-desktop-portal`
      cannot identify the caller and refuses it everything — the file chooser included, in
      silence. Every file dialog in Sieve did nothing until this was found, and it would have
      made PSBT export appear broken on arrival. `RLIMIT_CORE=0` is not lifted, and signing is
      never inside the window; see `SECURITY.md`.
- [x] Idle auto-lock, with the interval a preference and a "Lock now" beside it.
- [x] Lock on suspend, via logind `PrepareForSleep` over the D-Bus GIO already provides.
- [x] A lock for watch-only wallets, which had none at all: `lock.sieve` seals a known
      constant, so a wallet with no keys still has a password to fail against. Per-wallet
      passwords stay — a single application password was considered and rejected, since one
      secret opening every wallet is a poor trade for typing one fewer password.
- [ ] Opt-in Secret Service storage, labelled as convenience and not as a boundary.
- [ ] FIDO2 `hmac-secret` as a second wrap.
- [ ] Lock on screensaver as well as on sleep — a locked session is the other ordinary way a
      machine is left unattended.
- PSBT export/import and device signing moved to M4a.

### M8 — Package and release
Native packages, three of them, and no Flatpak. See `PACKAGING.md` for why, what it costs,
and the order of work.

- [ ] udev rules for the hardware signers, shipped by the package — the thing a sandbox
      cannot do, and what `hardware::udev_hint()` already promises.
- [x] A PKGBUILD for Arch and Omarchy, built with `makepkg` on an Omarchy machine: the
      package installs the binary, the desktop entry, the icon under the name the entry asks
      for, the udev rules and the docs, and the binary links against exactly the four declared
      dependencies. `options=(!lto)` is load-bearing — see `PACKAGING.md`.
- [x] `--version` and `--help`, answered before a display is opened, so a container can check
      the package it just installed.
- [ ] A release workflow: `.github/workflows/release.yml` is written and has never run, for
      want of a remote.
- [ ] Install the icon and desktop entry properly: the gresource is for the app's own use and
      does nothing for the desktop's icon theme.
- [ ] `cargo-deb` and `cargo-generate-rpm` metadata, built in containers of the oldest target
      of each family.
- [ ] Verify the libadwaita 1.5 floor against real containers. Ubuntu 22.04 and Debian 12 are
      expected to fail it, and there is no fix short of lowering the baseline.
- [ ] A signed tag, GitHub Releases and a signed `SHA256SUMS`, produced by a tag-triggered
      workflow rather than by hand — including installing each artefact in a clean container
      and running it, which is the only test the dependency lists ever get. Blocking on
      `sieve-bin`, not parallel to it.
- [ ] Re-release `sieve-bin` when a shared library it links against bumps its soname. Arch is
      rolling and a source package is simply rebuilt; a binary one installs and then fails to
      start, with nothing to warn you. The check that the binary links against exactly its
      declared dependencies is the only thing that catches it.
- [ ] Publish to the AUR — **`sieve-bin` first**, since nobody should have to compile a
      wallet to run it, and on a rolling distribution the alternative is a full Rust build of
      this dependency tree on somebody else's machine every time a version is tagged. That
      makes the signing work release-blocking rather than deferred: a prebuilt binary asks
      people to run bytes they did not produce, and a checksum in a recipe published by the
      same person answers none of that. `makepkg` has to verify a **signature**.
- [ ] Then a pull request adding one line to Omarchy's install menu — `PACKAGING.md` has the
      shape of the entry and why the category is the awkward part.
- [ ] Delete the Flatpak manifest and `scripts/fetch-tor.sh` once the AUR package is proven.
- [ ] `org.freedesktop.portal.Secret`, reproducible builds, and external review of the vault
      format and the signing path.

**Mainnet is not gated, and this milestone no longer claims it is.** Both making a wallet
and importing one offer bitcoin first and by default, behind a switch acknowledging that the
software is unreviewed. The gate was never real — `ui/restore.rs` had let mainnet through for
a long time, and the wallet has been run against it with real coins — and a signet default
that somebody had to change to reach their own money taught nobody anything. The
acknowledgement is a sentence people read; a wrong default is a step people click past.

## Before anyone else runs it

- [ ] Remove the **Welcome screen (preview)** item from the header menu. It exists to look at
      the first-run screen without starting over, is marked TEMPORARY in five places, and is
      the one thing in the UI that is there for the person building it rather than the person
      using it.

## Running alongside

- regtest harness with `bitcoind -blockfilterindex=1` for integration tests
- CI: `cargo test`, `clippy -D warnings`, `cargo fmt --check` — the baseline is clean now, so `fmt --check` can be switched on
- `cargo audit` and `cargo deny`
- Keep CLAUDE.md current as milestones land

## Known risks

- **bdk_kyoto is pre-1.0** — pin exact versions; expect M2 wiring to churn on upgrades
- **Mainnet recovery is slow** — needs honest progress, not a spinner that looks hung
- **Argon2 at 512 MiB** — comfortable on desktop, hostile on a 2 GB machine; measure before locking
- **X11 leaks keystrokes** — Wayland is the supported target; say so in the docs
