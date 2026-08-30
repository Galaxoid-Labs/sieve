# Packaging

## Why this exists

`com.galaxoidlabs.Sieve.yml` builds Sieve **with its own Tor**. That is the whole
reason to package rather than hand out a binary: Tor lands at `/app/bin/tor`,
beside the app, where `tor::daemon::find_binary` looks first, so Tor works for
someone who has never installed it and never opens a terminal.

The lookup order at runtime, for reference:

1. a proxy already listening on 9050 or 9150 — the system service, or Tor
   Browser. Sieve borrows it and never touches it.
2. `$SIEVE_TOR`, an explicit path. Also how the tests inject a stand-in.
3. `tor` next to the Sieve executable — this package, or a release tarball.
4. `tor` on `PATH` — installed on the machine but not running.

Only 3 needs packaging, and only 3 covers a person with nothing installed.

## Development builds

A `cargo run` build has no Tor beside it, so the switch in preferences has
nothing to start. One command fixes that:

```sh
scripts/fetch-tor.sh          # or: scripts/fetch-tor.sh release
```

It fetches the Tor Project's official Expert Bundle, checks it against a digest
pinned in the script (and its signature, when gpg has the key), and unpacks it
into `target/debug/tor/`, which is where the binary looks. The bundle carries
the libevent and OpenSSL Tor was built against; Sieve sets `LD_LIBRARY_PATH`
when it starts a Tor that has libraries beside it, because the system ones are
not necessarily compatible — the binary dies on an unresolved symbol otherwise.

Verify it end to end with:

```sh
cargo test -- --ignored --nocapture tor_actually_starts
```

which starts Tor, waits for the bootstrap, proves the proxy answers `RESOLVE`
(so it is Tor and not merely something listening), and resolves Bitcoin DNS
seeds through it.

## Building the Flatpak

```sh
flatpak install org.gnome.Sdk//48 org.gnome.Platform//48 \
                org.freedesktop.Sdk.Extension.rust-stable//24.08
flatpak-builder --user --install --force-clean build packaging/com.galaxoidlabs.Sieve.yml
flatpak run com.galaxoidlabs.Sieve
```

Flathub builds are offline, so the Rust module needs its dependencies listed
up front:

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o packaging/generated-sources.json
```

(from [flatpak-builder-tools](https://github.com/flatpak/flatpak-builder-tools),
`cargo/` directory), then uncomment the source in the manifest.

## Verifying Tor before bumping it

The manifest pins a sha256 that matches the digest Tor publishes beside the
tarball. That proves the download was not corrupted in transit. It does not
prove the Tor Project published it — for that, check the GPG signature:

```sh
gpg --auto-key-locate nodefault,wkd --locate-keys ahf@torproject.org
curl -O https://dist.torproject.org/tor-<version>.tar.gz
curl -O https://dist.torproject.org/tor-<version>.tar.gz.sha256sum.asc
gpg --verify tor-<version>.tar.gz.sha256sum.asc
```

A wallet that starts a network daemon it did not verify has undone the point of
the wallet.

## Not done yet

- AppStream metainfo (`com.galaxoidlabs.Sieve.metainfo.xml`), required by Flathub.
- Reproducible builds and signed tags — M8 in `ROADMAP.md`.
- This manifest has never been built: there is no flatpak-builder on the
  machine it was written on.
