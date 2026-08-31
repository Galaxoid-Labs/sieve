# Getting Sieve onto other people's machines

Written after checking the target rather than assuming it. The headline: the
plan in `ROADMAP.md` M8 was Flatpak-first, and for the machine this is being
built on that is the wrong first step.

## What is actually on an Omarchy machine

```
flatpak      — not installed, no remotes configured
yay          /usr/bin/yay
pacman       /usr/bin/pacman
makepkg      /usr/bin/makepkg
gtk4         4.22.4          libadwaita  1.9.3
sqlite       3.53.4          openssl     3.6.3
tor          0.4.9.11 in extra, not installed
```

Omarchy is Arch (`ID_LIKE=arch`) and ships an AUR helper but **no Flatpak at
all**. So a Flatpak-only release means every Omarchy user runs
`pacman -S flatpak`, adds Flathub, restarts their session for the exports to
appear, and downloads a GNOME runtime of a few hundred megabytes — to run a
native GTK4 application on a machine that already has GTK4 4.22 and
libadwaita 1.9 installed.

That is the wrong trade for the distro this is being written on. It is the
right trade for Fedora Silverblue.

**So: two channels, and the native one comes first.**

## Channel one — AUR, for Arch and Omarchy

`yay -S sieve`. One command, no runtime, uses the system's own GTK.

Everything Sieve links is already in the repositories at a compatible
version. The `gnome_46` feature pins libadwaita 1.5 as the *baseline*, and
1.9.3 satisfies it.

### The PKGBUILD, in outline

```bash
pkgname=sieve
pkgdesc="A privacy-focused Bitcoin wallet"
arch=('x86_64')
url="https://github.com/galaxoidlabs/sieve"        # once there is a remote
license=('MIT' 'Apache-2.0')
depends=('gtk4' 'libadwaita' 'sqlite' 'openssl')
makedepends=('cargo' 'git')
optdepends=('tor: route connections through Tor')

build()   { cargo build --release --frozen }
check()   { cargo test --frozen }
package() {
  install -Dm755 target/release/sieve            "$pkgdir/usr/bin/sieve"
  install -Dm644 data/com.galaxoidlabs.Sieve.desktop \
                 "$pkgdir/usr/share/applications/com.galaxoidlabs.Sieve.desktop"
  install -Dm644 data/icons/hicolor/scalable/apps/bitcoin-logo.svg \
                 "$pkgdir/usr/share/icons/hicolor/scalable/apps/com.galaxoidlabs.Sieve.svg"
}
```

**Tor needs no bundling here.** `tor::daemon::find` already looks beside the
executable *and* on `PATH`, so `pacman -S tor` is all it takes; the optdepend
says so. The Expert Bundle in `packaging/` exists for the Flatpak, where
there is no system Tor to find.

**The icon has to be installed by hand.** It is compiled into the binary as a
gresource for the app's *own* use, which does nothing for the desktop
environment: the `.desktop` file says `Icon=com.galaxoidlabs.Sieve`, and that
name has to exist in the hicolor theme on disk. Cargo installs no data files,
so the package does it.

### Three variants, in the order they are worth making

1. **`sieve`** — builds from a tagged release tarball. The honest default: the
   source is what is being audited, and the build is reproducible by whoever
   installs it.
2. **`sieve-git`** — builds from `master`. Cheap to add, and it is what early
   testers want.
3. **`sieve-bin`** — a prebuilt binary, once there are signed releases to
   point at. Saves a five-minute Rust build; costs a trust decision, so it
   comes last and only with checksums and a signed tag behind it.

## Channel two — Flathub, for everyone else

This is where a sandbox actually means something, and where Fedora, Ubuntu,
Mint and the immutable distributions get it. `packaging/com.galaxoidlabs.Sieve.yml`
already builds Tor and libevent from source into the app.

**It has never been built.** There is no `flatpak-builder` on this machine —
that is the first task, not the last, because a manifest that has never run
is a guess.

Still missing before it can be submitted:

- **AppStream metainfo** (`com.galaxoidlabs.Sieve.metainfo.xml`) — Flathub
  requires it, and it is what puts a description and screenshots in every
  software centre.
- **An icon that is Sieve's own.** The Bitcoin symbol is a placeholder; it
  says "a bitcoin thing", not "Sieve", and Flathub's review will say so.
- **A built, tested manifest**, including the bundled Tor actually starting
  inside the sandbox — the one thing about that manifest nobody has observed.

### The permission list, and the one hard problem

| Permission | Why |
|---|---|
| `--share=network` | Bitcoin peer-to-peer, and Tor |
| `--socket=wayland`, `--socket=fallback-x11` | the display |
| `--device=dri` | rendering |
| *(no `--filesystem`)* | `gtk::FileDialog` goes through the portal, which is how label import and PSBT files should work anyway |

**Hardware wallets are the hard problem.** Talking to a Ledger means raw USB
HID, which inside a sandbox needs `--device=all` — the escape hatch that
gives the app every device on the machine, and which Flathub reviewers push
back on for good reason. It also still needs udev rules on the host, which a
Flatpak cannot install.

That is a real argument for the native package being the *better* experience
for anyone with a hardware wallet, not merely the more convenient one. The
Flatpak can ship without USB signing and say so.

## What is not worth doing

- **Snap.** Not on Arch, and the sandbox story is no better than Flatpak's.
- **AppImage.** Bundles GTK4 and libadwaita to run on a machine that has them,
  and gets no sandbox for it. The one case it wins — a distro too old for the
  libadwaita baseline — is a case this app does not need to serve.
- **A distro repository of our own.** Signing, hosting and key rotation for an
  audience that has the AUR already.

## Order of work

1. **Write the PKGBUILD and build it locally** with `makepkg -si`. Prove the
   desktop entry, the icon and `yay -S tor` interoperation on this machine.
2. **Tag a release.** `sieve` needs something to point at, and a signed tag is
   the thing a reviewer checks.
3. **Install `flatpak-builder` and build the manifest.** Find out what is
   broken in a file that has never run, especially bundled Tor inside the
   sandbox.
4. **AppStream metainfo and a real icon**, which are Flathub's gate and also
   simply overdue.
5. **Submit to AUR**, then Flathub.

## What "works well on Omarchy" means beyond installing

- Wayland under Hyprland, which is how it already runs here.
- libadwaita follows the system colour scheme, and Sieve reads it directly
  rather than only through libadwaita's backend — that fix is already in.
- No tray icon, no background service, no autostart. Sieve is a window
  somebody opens.
- The idle lock and lock-on-sleep matter more on a laptop that suspends often
  than they do on a desktop, and both are in.
