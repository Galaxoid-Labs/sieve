#!/usr/bin/env bash
# Regenerate the third-party license list shown in About → Legal.
#
# Run after adding, removing or bumping a dependency. The list is generated
# rather than maintained by hand because a hand-maintained one is wrong within
# a week, and a wrong acknowledgement is worse than none.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo metadata --format-version 1 | python3 -c '
import json, sys

meta = json.load(sys.stdin)
rows = []
for package in meta["packages"]:
    if package["name"] == "sieve":
        continue
    licence = package.get("license")
    if not licence:
        # Two crates state their licence in a file rather than the manifest.
        licence = {
            "async-hwi": "BSD-3-Clause",
            "bip324": "Apache-2.0 OR MIT",
        }.get(package["name"], "see the crate")
    home = package.get("repository") or package.get("homepage") or ""
    rows.append((package["name"], package["version"], licence.replace("/", " OR "), home))

rows.sort(key=lambda row: row[0].lower())
for name, version, licence, home in rows:
    print("\t".join([name, version, licence, home]))
' > data/third-party.txt

echo "wrote data/third-party.txt ($(wc -l < data/third-party.txt) lines)"
