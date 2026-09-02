# Getting Sieve onto other people's machines

Native packages, three of them: **AUR** for Arch and Omarchy, **`.deb`** for
Debian and Ubuntu, **`.rpm`** for Fedora. No Flatpak.

Written after checking the target rather than assuming it.

## Why native, and what it costs

The machine this is built on says most of it:

```
flatpak      — not installed, no remotes configured
yay          /usr/bin/yay          pacman   /usr/bin/pacman
gtk4         4.22.4                libadwaita  1.9.3
sqlite       3.53.4                openssl     3.6.3
tor          0.4.9.11 in extra
```

Omarchy is Arch and ships an AUR helper with **no Flatpak at all**. Shipping
only a Flatpak would ask every user here to install Flatpak, add Flathub,
restart their session and download a few hundred megabytes of GNOME runtime —
in order to run a native GTK4 application on a machine that already has GTK4
4.22 and libadwaita 1.9.

Three things native packaging wins outright:

- **Nothing to install first.** One command, using the GTK the machine
  already has.
- **Hardware wallets actually work.** USB HID in a sandbox needs
  `--device=all`, the escape hatch that hands the app every device on the
  machine — and it *still* needs udev rules the sandbox cannot install. A
  native package installs those rules itself. `hardware::udev_hint()` already
  promises this: *"the packaged build of Sieve will ship them."*
- **No bundled Tor anywhere.** `tor::daemon::find` looks on `PATH` as well as
  beside the executable, and `tor` is a package on all three families. The
  Expert Bundle machinery existed for the sandbox and now has no user.

And three things it costs, which are real:

- **A build matrix.** One artefact per distribution family and per release,
  built against that release's glibc and GTK. Flatpak's whole pitch is
  building once.
- **A version floor with no way around it.** `gnome_46` means **libadwaita
  1.5 or newer**. Anything older cannot run this binary, and a Flatpak would
  have carried its own libadwaita for exactly those machines. Ubuntu 22.04
  and Debian 12 are out; there is no fix short of lowering the baseline, and
  `CLAUDE.md` records why it is where it is.
- **No sandbox.** Sieve reads and writes its own data directory and talks to
  the network, and that is now bounded by the user account rather than by a
  manifest.

That is a fair trade for a wallet whose users are the sort of people who
already have `yay`. It is worth writing down that it *is* a trade.

## What three packages cover, and what they miss

| Family | Reaches |
|---|---|
| AUR | Arch, Omarchy, Manjaro, EndeavourOS, CachyOS, Garuda |
| `.deb` | Debian, Ubuntu, Mint, Pop!_OS, elementary, Zorin, KDE neon, Raspberry Pi OS |
| `.rpm` | Fedora, RHEL, Alma, Rocky, Nobara |

That is most of what people actually run. What it misses, in the order it is
likely to matter:

- **Immutable distributions** — Silverblue, Kinoite, Bazzite, SteamOS. Flatpak
  is the native idiom there and layering an `.rpm` with `rpm-ostree` works but
  is discouraged. This is the real cost of not shipping a Flatpak, and it is
  the one that would bring the decision back if people ask.
- **openSUSE**, which is RPM with different package names, so a Fedora `.rpm`
  may not resolve. A second spec rather than a new format; cheap when someone
  wants it.
- **NixOS**, which needs a derivation of its own and whose users usually write
  one.
- **Ubuntu 22.04 and Debian 12**, excluded by the libadwaita floor in any
  format. Only a Flatpak carrying its own libadwaita, or lowering the
  baseline, would reach them.
- **Gentoo, Alpine, Void** and the rest, who build from source — which works
  anywhere the libraries are new enough, and is what those users expect.

## The version floor, which decides the matrix

Sieve needs **libadwaita ≥ 1.5** (GNOME 46) and **GTK ≥ 4.14**. Every target
below is a claim to be checked by building in a container of that release,
not from memory:

| Target | Expected to clear the floor |
|---|---|
| Arch, Omarchy | yes, rolling |
| Fedora 40+ | yes |
| Ubuntu 24.04 LTS, 24.10+ | yes |
| Debian 13 (trixie) | yes |
| Ubuntu 22.04 LTS, Debian 12 | **no** — libadwaita too old |

The build itself must happen *on* the oldest target of each family, in a
container, because a binary linked against newer glibc will not run on older.

## Channel one — AUR

`yay -S sieve`.

```bash
pkgname=sieve
pkgdesc="A privacy-focused Bitcoin wallet"
arch=('x86_64')
license=('MIT')
depends=('gtk4' 'libadwaita' 'sqlite' 'openssl' 'systemd-libs')
makedepends=('cargo' 'git')
optdepends=('tor: route every connection through Tor')

build()   { cargo build --release --frozen }
check()   { cargo test --frozen }
package() {
  install -Dm755 target/release/sieve "$pkgdir/usr/bin/sieve"
  install -Dm644 data/com.galaxoidlabs.Sieve.desktop \
    "$pkgdir/usr/share/applications/com.galaxoidlabs.Sieve.desktop"
  for size in 16 24 32 48 64 128 256 512; do
    install -Dm644 "data/icons/hicolor/${size}x${size}/apps/com.galaxoidlabs.Sieve.png" \
      "$pkgdir/usr/share/icons/hicolor/${size}x${size}/apps/com.galaxoidlabs.Sieve.png"
  done
  install -Dm644 packaging/udev/51-sieve-hardware.rules \
    "$pkgdir/usr/lib/udev/rules.d/51-sieve-hardware.rules"
}
```

Three variants. **`sieve-bin`** is the one to recommend and the one to build
first: nobody should have to compile a wallet to run it, and on the AUR the
alternative is not a one-off cost but a full Rust build of this dependency tree
on somebody else's machine *every time a version is tagged*. **`sieve`** from a
tagged release tarball stays, for people who would rather build what they can
read. **`sieve-git`** from `master` is for early testers.

That ordering moves work rather than skipping it. A prebuilt binary is a trust
decision — it asks people to run bytes they did not produce — and the answer to
that is signed tags, a signed `SHA256SUMS`, and eventually reproducible builds.
Those were "later" while `sieve-bin` was last. They are now **release-blocking**,
because they are the whole of what makes a binary package honest:

- `source=(... .sig)` and `validpgpkeys=()` in the PKGBUILD, so `makepkg`
  verifies a **signature** rather than a checksum. A `sha256sums` line in a
  recipe the same person publishes proves only that the download was not
  corrupted in transit; it says nothing about who built it.
- The signing key published somewhere a person can check it against, and
  rotation announced in release notes rather than happening quietly.

`sieve-bin` also carries a maintenance cost the source package does not. Arch
is rolling, so a shared library it links against can bump its soname at any
time; a source package is simply rebuilt, while a binary one has to be
re-released. Nothing warns you — the package installs and then fails to start.
The dependency-audit check that the binary links against exactly its declared
dependencies is what catches this, and for `sieve-bin` it is the only thing
that does, since there is no build on the user's machine to fail loudly.

**`options=(!lto)` is not optional.** makepkg compiles C with `-flto=auto` by
default — `OPTIONS=(... lto)` in `/etc/makepkg.conf` — and secp256k1 ships a
bundled C library. Building it to LLVM bitcode leaves the Rust link unable to
resolve its symbols:

```
rust-lld: error: undefined symbol: rustsecp256k1_v0_10_0_ec_pubkey_parse
  >>> referenced by async_hwi::ledger::Ledger<TransportHID>::sign_tx
```

Rust's own LTO is untouched and stays on; `cargo build --release` links
perfectly well outside makepkg. This is the standard measure for a Rust
package with a C dependency, and it is the sort of thing that is only ever
found by building the package rather than by reading the recipe.

**The icon must be installed by hand.** It is compiled into the binary as a
gresource for the app's own use, which does nothing for the desktop's icon
theme: the `.desktop` file says `Icon=com.galaxoidlabs.Sieve`, and that name
has to exist in hicolor on disk. Cargo installs no data files.

## Being found on Omarchy

Installing and being discovered are different problems, and Omarchy has four
surfaces for the second. They are a ladder: each rung needs the one below it,
and only the first is self-serve.

Read off the machine rather than guessed — the catalogue is
`/usr/share/omarchy/default/omarchy/omarchy-menu.jsonc`, the repo is
configured in `/etc/pacman.conf`.

### 1. The AUR, which needs nobody's permission

Publishing `sieve` makes it findable immediately:

```
yay -Ss bitcoin wallet
```

Omarchy's own menu has a generic **Install → AUR** entry
(`omarchy-pkg-aur-install`) that drops into an AUR search, so this rung alone
means somebody looking for a Bitcoin wallet on Omarchy can find one. Search
matches on `pkgdesc`, so it is worth writing for a person rather than for a
manifest.

### 2. The Omarchy menu, which is the real path

`Super+Alt+Space` → **Install** → a category. The catalogue is curated JSONC
and an entry is one line:

```jsonc
"install.ai.lm-studio": {"icon":"","label":"LM Studio",
  "when":"! omarchy-pkg-present lmstudio-bin",
  "action":"omarchy-install-app 'LM Studio' lmstudio-bin"}
```

Sieve's would be the same shape, guarded by `! omarchy-pkg-present sieve` so
it disappears once installed. Getting there is a **pull request to Omarchy**,
which is a human decision and wants a package that already exists and works.

The categories today are `ai`, `browser`, `development`, `editor`, `gaming`,
`service`, `style`, `terminal`, `tui`, `webapp`, `windows`. There is no
finance category, so the PR either proposes one or argues for a home in an
existing one — and being the reason a category exists is a harder sell than
being the third entry in one.

### 3. The `[omarchy]` binary repository

```
[omarchy]
Server = https://pkgs.omarchy.org/stable/$arch
```

Already configured on every Omarchy machine. A package there installs as fast
as any system package with no AUR build, and it is where most menu entries
point. Also a curation decision, and the top rung rather than the first.

### 4. The launcher, after installation

`data/com.galaxoidlabs.Sieve.desktop` already carries
`Categories=GNOME;GTK;Office;Finance;` and `Keywords=Bitcoin;Wallet;Privacy;`,
so Sieve answers a launcher search for "bitcoin" or "wallet" — **provided the
package installs the desktop entry and the icon under the name the entry
asks for**. That is the gap noted above, and it is what makes this rung work
at all.

### The order

Publish to the AUR → install it on Omarchy from the AUR and confirm the menu,
launcher and udev rules all behave → open a pull request adding one line to
the menu → and if it earns a place, the binary repository follows.

## Channels two and three — `.deb` and `.rpm`

Both generated from `Cargo.toml` metadata rather than hand-written packaging
trees:

- **`cargo-deb`** — `[package.metadata.deb]` for depends, the desktop entry,
  the icon and the udev rules.
- **`cargo-generate-rpm`** — `[package.metadata.generate-rpm]`, same list.

Runtime dependencies by family:

| | Debian / Ubuntu | Fedora |
|---|---|---|
| GTK | `libgtk-4-1` | `gtk4` |
| Adwaita | `libadwaita-1-0` | `libadwaita` |
| SQLite | `libsqlite3-0` | `sqlite-libs` |
| TLS | `libssl3` | `openssl-libs` |
| Tor | `Suggests: tor` | `Recommends: tor` |

Built in containers — `debian:trixie`, `ubuntu:24.04`, `fedora:41` — from a
release tag, in CI, so the artefacts are reproducible by someone else and not
by this laptop.

## Releases, by CI rather than by hand

Three artefacts built on three distributions, checksummed, signed and
published every time — that is exactly the work a person does badly and a
machine does the same way twice. There is no remote yet, so this is the shape
to build when there is one.

**One trigger: pushing a tag.** `v0.2.0` and nothing else. No release from a
branch, no manual dispatch that quietly builds something other than what the
tag says.

```
.github/workflows/release.yml

  check     tag matches the version in Cargo.toml, or stop
            cargo fmt --check · cargo clippy -D warnings · cargo test

  arch      container: archlinux:base-devel
            makepkg --syncdeps, then install the result and run `sieve --version`

  deb       container: debian:trixie      → cargo-deb
            container: ubuntu:24.04       → cargo-deb
            install into a clean container of the same image, and run it

  rpm       container: fedora:41          → cargo-generate-rpm
            install into a clean container, and run it

  publish   SHA256SUMS over every artefact, signed
            GitHub Release with the notes from the tag
            push the PKGBUILD to the AUR over SSH
```

Five things worth getting right, each of which is a way this goes wrong
quietly:

- **The version guard comes first.** A tag that disagrees with `Cargo.toml`
  produces packages whose filename lies about their contents. Cheapest
  possible check, first job, everything else depends on it.
- **Build in a container of the *oldest* target of each family.** glibc is
  forwards compatible and not backwards: a binary linked on Ubuntu 24.10 will
  not run on 24.04. Pin the images by digest so a base image moving does not
  silently change what shipped.
- **Install what was built, in a clean container, and run it.** Not `--help`
  in the build container that already has every dev library — a fresh
  container with only the declared dependencies, which is the only way the
  dependency list is ever actually tested. It needs a headless display to get
  past GTK initialisation; `xvfb-run` or `WAYLAND_DISPLAY=` with a nested
  compositor, whichever proves less trouble.
- **`--locked` everywhere**, and commit `Cargo.lock`. A release that resolves
  its own dependencies is a release nobody can reproduce.
- **Sign the checksums, not the files.** One signature over `SHA256SUMS` is
  what `sieve-bin`'s PKGBUILD verifies and what a person can check by hand.

## The signing key

**GPG, and not because it is pleasant.** minisign is simpler and Sigstore
needs no long-lived key at all, but `makepkg` verifies OpenPGP signatures and
nothing else: `validpgpkeys` in a PKGBUILD is the mechanism, so GPG is what an
AUR package can actually check. A signature nobody's tooling verifies is
decoration.

**One key, two halves.** Make a primary key and keep it offline — on a machine
that is not this one, or on paper. Give it a signing **subkey**, and put only
that in CI. If a runner is ever compromised the subkey is revoked and the
identity survives; a leaked primary means starting again and asking everybody
to trust something new.

On your own machine, not in CI. Nothing below is a placeholder to paste
verbatim — the two ids are read out of the first command's output and put in a
variable, because `<LIKE-THIS>` in a shell is a redirect and not a blank to
fill in.

```sh
# 1. The primary. This is the one that goes offline afterwards.
gpg --quick-generate-key "Your Name <you@example.com>" ed25519 sign 2y

# 2. Read its fingerprint out — the 40 characters under `sec`.
gpg --list-secret-keys --keyid-format=long --with-subkey-fingerprints
PRIMARY=paste-the-fingerprint-here

# 3. The signing subkey, which is the half CI gets.
gpg --quick-add-key "$PRIMARY" ed25519 sign 1y

# 4. Read the subkey's id — the `ssb` line, the one dated today.
gpg --list-secret-keys --keyid-format=long "$PRIMARY"
SUBKEY=paste-the-ssb-key-id-here

# 5. Export that subkey and nothing else. The `!` is what limits it to this
#    key; without it you export everything under the primary, which is the
#    thing you are trying not to hand over. It is quoted separately because
#    an unquoted `!` is history expansion to an interactive bash.
gpg --export-secret-subkeys --armor "$SUBKEY"'!' > release-subkey.asc

# 6. And the public half, for anybody verifying a download.
gpg --export --armor "$PRIMARY" > sieve-signing-key.asc
```

Check step 5 before trusting it — an export that quietly included the primary
looks identical from the outside. `--show-keys` is *not* the way: it prints
`sec` for the stub and for the real thing alike, which reads as a disaster that
has not happened. Ask what packets are actually in the file:

```sh
gpg --list-packets release-subkey.asc | grep -E 'secret|gnu-dummy'
```

What you want:

```
:secret key packet:
	gnu-dummy, algo: 0, ...      <- the primary, present in name only
:secret sub key packet:
	iter+salt S2K, ...           <- the subkey, with real protected material
```

`gnu-dummy` on the primary is the whole point: the packet is a placeholder and
the secret is not in this file. If the primary's packet shows an S2K and
protected material like the subkey's does, you have exported the key you meant
to keep offline — start again at step 5.

The files hold a secret and should be `600` in a `700` directory, which is not
what a shell redirect leaves behind:

```sh
chmod 700 ~/sieve_keys && chmod 600 ~/sieve_keys/*.asc
```

Once `release-subkey.asc` is in the repository's secrets, delete it. It has no
second use, and a private key sitting in a home directory is one more place it
can leak from. The primary stays wherever offline means for you.

Then, in the repository's **Settings → Secrets and variables → Actions**:

| Secret | What goes in it |
|---|---|
| `SIGNING_KEY` | the contents of `release-subkey.asc` |
| `SIGNING_KEY_PASSPHRASE` | the passphrase protecting it |

A key with no passphrase would sign just as well and be worth less: the file
alone would be enough for anybody who read it.

**What the secret is worth is what the signature claims.** Anybody who can push
to this repository can write a workflow that prints that secret, so the key is
as protected as write access is. Three things narrow that, and none of them
cost anything:

- Put the secrets in a GitHub **Environment** with required reviewers, so a run
  has to be approved before it can read them, rather than leaving them readable
  by every workflow in the repository.
- **Protect the tags.** `v*` should be pushable by you and nothing else; the
  release only runs on a tag, so that is the whole trigger surface.
- **Never use `pull_request_target`.** A `pull_request` from a fork gets no
  secrets, which is the behaviour you want; `pull_request_target` runs with
  them and with the fork's code in reach.

If that is not enough for you — and for a wallet it reasonably might not be —
the alternative is to sign `SHA256SUMS` on your own machine after CI publishes
it and attach the `.asc` by hand. Slower, and the key never touches a server.
That is a decision to make once and write down here, because changing it later
looks identical to a compromise.

**Publish the fingerprint where it is not the release.** In the README, and in
the repository description. A fingerprint that only ever appears next to the
files it signs is one an attacker can replace along with them.

Rotating is a release note, not a silent event.

**The AUR step is different from the others** and worth separating. CI does
not build the AUR package — the AUR builds it, on the user's machine, from
source. What CI pushes is the `PKGBUILD` and `.SRCINFO` for the new version,
over SSH with a deploy key. So the Arch job's purpose is not to produce an
artefact but to **prove the PKGBUILD still works** before that push: build it,
install it, run it. A broken PKGBUILD in the AUR is broken for everybody who
tries to install until somebody notices.

`sieve-bin` is the exception — it points at the release tarball and its
checksum, both of which the publish job has just produced, so its PKGBUILD is
generated rather than maintained.

## Hosting

**GitHub Releases first**: three files and a `SHA256SUMS`, signed. Enough for
`sieve-bin`, enough for anyone who wants to install by hand, and it costs
nothing to maintain.

**A real repository later**, if there is demand. apt and dnf repositories
mean signing keys, key rotation and hosting; the openSUSE Build Service will
build and host both for free from one spec, which is the least-effort route
if it comes to that. Not before there are users asking.

## The udev rules, which are new work

Hardware wallets are invisible to a non-root process until udev says
otherwise, and this is the thing a Flatpak cannot fix and a native package
can. `packaging/udev/51-sieve-hardware.rules` needs writing, taking the
vendor and product ids the vendors publish for Ledger, Coldcard, Trezor,
BitBox and Jade, and tagging them `uaccess` so the logged-in user can open
them.

Then `hardware::udev_hint()` can stop promising and start pointing: the rules
are installed, so if a device is still invisible the answer is to replug it
or check the device is unlocked.

## Order of work

1. **Write the udev rules**, and confirm a Ledger appears without root once
   they are installed.
2. **Write the PKGBUILD and `makepkg -si` it here.** Prove the binary, the
   desktop entry, the icon and the rules all land where they should.
3. **Tag a release**, signed, so there is something for a package to point at.
4. **Add `cargo-deb` and `cargo-generate-rpm` metadata**, and build both in
   containers.
5. **Verify the floor claims** by installing the artefacts in
   `ubuntu:24.04`, `debian:trixie` and `fedora:41` containers and running the
   binary under a nested Wayland session.
6. **Publish to GitHub Releases, then submit to AUR** — `sieve-bin` first,
   since it is the one to recommend, and `sieve` alongside it.

## How an update reaches somebody

Worth writing down, because the rest of this file is about *producing* a
release and none of it is about somebody receiving one.

**On the AUR, a release is a git push.** The AUR hosts recipes, not binaries,
so publishing a new version means pushing a bumped `pkgver` to
`ssh://aur@aur.archlinux.org/sieve.git`. A helper — `yay`, `paru` — compares
what is there against what is installed and rebuilds on the next `-Syu`. On
Omarchy, `omarchy update` runs the system update, so it comes along with
everything else. Nothing else has to happen, and Sieve is never involved.

Three things about that are easy to get wrong:

- **`.SRCINFO` is what helpers read**, not the `PKGBUILD`. A version bump
  without `makepkg --printsrcinfo > .SRCINFO` is an update nobody is offered.
  The AUR refuses a push where the two disagree, which catches it — but only if
  CI regenerates the file rather than committing a stale one.
- **`sieve-git` does not auto-update.** Its `pkgver()` is computed at build
  time from `git describe`, so a helper cannot see a new commit without a devel
  check — `paru -Sua --devel`, `yay -Syu --devel` — which most people never
  run. Say so on the package page rather than letting early testers believe
  they are current.
- **Do not checksum GitHub's generated tarball.** `archive/refs/tags/*.tar.gz`
  is produced on demand and its bytes have changed before, breaking
  `sha256sums` for everybody at once. Point at a tarball attached to the
  release, which is what the publish job already produces a `SHA256SUMS` for.

**`.deb` and `.rpm` have no update path at all.** A downloaded package is a
dead end: apt and dnf never hear about a version they were not told about. Both
are install-once, update-never until there is a repository to point at, which
is the real cost of "a real repository later" above. Anybody on those channels
gets a new version only by coming back to look.

**Sieve must not check for updates itself.** An update check is a network
request that tells whoever answers it that this machine runs this wallet, at
this version, at this time — a beacon, from a program whose whole claim is that
no server is told anything. Package managers exist so that applications do not
do this. If a release ever carries a security fix, the channel is the release
notes and the distribution, not a dialog that phones home.

## What uninstalling does not remove

**No package Sieve ships will ever delete a wallet, and none of them could.**
Worth stating in the file about packaging, because it is the one question a
packager is tempted to answer with a script.

`dpkg`'s `postrm purge`, rpm's `%postun` and pacman's `post_remove` all run as
root against system paths. None of them has a reliable notion of *which* users
installed anything, and Debian policy forbids touching `$HOME` outright. There
is no per-user uninstall hook on Linux to implement. The single exception in
the ecosystem is Flatpak — `flatpak uninstall --delete-data`, which works only
because the data is corralled inside `~/.var/app/<id>/` — and Sieve does not
ship a Flatpak.

**That is the right behaviour, and it should not be worked around.** For a
wallet, uninstall-deletes-data is a money bug waiting for an ordinary event: a
repository change, a reinstall, a distribution upgrade that removes and re-adds
a package. Anybody who did not write their recovery phrase down would lose the
coins, and the operation that did it would look routine. Data outliving the
package is the property being defended.

So the handling is documentation, in the two places somebody looks:

- **In the app.** Preferences has a Files group naming both directories, with a
  copy button and a button that opens each one, under a description saying that
  removing Sieve leaves them in place and that the wallet directory should be
  deleted only by somebody holding the recovery phrase.
- **In the package.** The `.deb`'s `extended-description` and the `.rpm`'s
  `description` both carry it, and the AUR package prints it from
  `post_remove` — which is the one moment it is useful, right after
  `pacman -R sieve`. A packager reading this file should not add a cleanup hook
  to be helpful.

  **`packaging/aur/sieve.install` prints and must never delete.** It is the
  obvious place to be helpful and the worst one: a hook that removed
  `~/.local/share/sieve` would destroy coins for anybody who had not written
  their recovery phrase down, during an operation — a repository change, a
  distribution upgrade — that looks entirely routine. The release workflow
  extracts the `.INSTALL` from the built package and fails if the message is
  missing or if anything resembling a removal has appeared in it.

Deleting a wallet from inside Sieve is a separate thing and already exists —
**Remove this wallet** in preferences, `destructive-action` styled and behind a
confirmation, which can warn about the phrase in a way a `postrm` script never
could.

Not attempted, and worth saying why: there is no secure erase. Overwriting a
file guarantees nothing on an SSD or a copy-on-write filesystem, and the vault
is encrypted, which is the protection that actually holds once the bytes are
somewhere Sieve cannot reach.

## What is now dead

`packaging/com.galaxoidlabs.Sieve.yml` and `scripts/fetch-tor.sh` exist to
build Tor into a Flatpak. With no Flatpak, nothing uses them. Keep them until
the AUR package is proven — they are the only record of how to bundle Tor —
then delete them and say so in `ROADMAP.md`, because a manifest nobody builds
is a manifest that rots.
