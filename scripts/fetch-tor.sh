#!/usr/bin/env bash
# Put a Tor in the build, so a development build behaves like a packaged one.
#
# The Flatpak builds Tor from source into /app/bin. This is the equivalent for
# `cargo run`: it fetches the Tor Project's official Expert Bundle, checks it
# against a pinned digest, and unpacks it next to the built binary, where
# `tor::daemon::find_binary` looks. Without this, a development build has no
# Tor to start and the switch in preferences refuses, correctly but uselessly.
#
# Usage: scripts/fetch-tor.sh [debug|release]
set -euo pipefail

VERSION="15.0.20"
# Published at https://dist.torproject.org/torbrowser/$VERSION/sha256sums-unsigned-build.txt
declare -A DIGESTS=(
  [x86_64]="3b39a2a7fbf43ef28b9ae0a6afca02a12935232f81769e4fef7472d6b5676eaf"
)

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/target/$PROFILE/tor"

case "$(uname -m)" in
  x86_64) ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) echo "no Tor Expert Bundle for $(uname -m)" >&2; exit 1 ;;
esac

DIGEST="${DIGESTS[$ARCH]:-}"
if [ -z "$DIGEST" ]; then
  echo "no pinned digest for $ARCH — add one from the published sha256sums" >&2
  exit 1
fi

TARBALL="tor-expert-bundle-linux-$ARCH-$VERSION.tar.gz"
URL="https://dist.torproject.org/torbrowser/$VERSION/$TARBALL"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "Fetching $TARBALL"
curl -fsSL --max-time 300 -o "$WORK/$TARBALL" "$URL"

echo "$DIGEST  $WORK/$TARBALL" | sha256sum --check --status || {
  echo "digest mismatch — refusing to install this Tor" >&2
  exit 1
}

# The signature is the part that says the Tor Project published it; the digest
# above only says the download arrived intact. Checked when gpg has the key,
# reported when it does not, rather than skipped in silence.
if command -v gpg >/dev/null 2>&1; then
  if curl -fsSL --max-time 60 -o "$WORK/sums.txt" \
       "https://dist.torproject.org/torbrowser/$VERSION/sha256sums-unsigned-build.txt" &&
     curl -fsSL --max-time 60 -o "$WORK/sums.txt.asc" \
       "https://dist.torproject.org/torbrowser/$VERSION/sha256sums-unsigned-build.txt.asc" 2>/dev/null; then
    if gpg --verify "$WORK/sums.txt.asc" "$WORK/sums.txt" 2>/dev/null; then
      echo "Signature verified."
    else
      echo "NOTE: could not verify the signature (the signing key may not be in your keyring)."
      echo "      See https://support.torproject.org/tbb/how-to-verify-signature/"
    fi
  fi
fi

tar xzf "$WORK/$TARBALL" -C "$WORK"
mkdir -p "$DEST"
# The binary and the libraries it was built against travel together.
cp "$WORK/tor/tor" "$DEST/tor"
cp "$WORK"/tor/*.so.* "$DEST/" 2>/dev/null || true
# Not required, but Tor complains on every start without them.
[ -f "$WORK/data/geoip" ] && cp "$WORK/data/geoip" "$DEST/geoip"
[ -f "$WORK/data/geoip6" ] && cp "$WORK/data/geoip6" "$DEST/geoip6"
chmod +x "$DEST/tor"

echo "Installed Tor $VERSION into $DEST"
# With its own libraries: the bundle ships the libevent and OpenSSL it was
# built against, and the system ones are not necessarily compatible — the
# binary fails to resolve a symbol if it picks those up instead. Sieve sets
# the same variable when it starts Tor.
LD_LIBRARY_PATH="$DEST" "$DEST/tor" --version | head -1
