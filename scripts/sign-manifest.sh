#!/usr/bin/env bash
# Writes the auto-update manifest of one Windows installer and its detached ed25519 signature.
# Usage: scripts/sign-manifest.sh <key.pem> <installer.exe> <version> <installer-url> <out.json> [rollout%] [min-version]
# Produces <out.json> and <out.json>.sig (hex of the 64-byte signature over the exact bytes of <out.json>).
# The key is an OpenSSL ed25519 private key (openssl genpkey -algorithm ed25519); see docs/auto-update.md.
set -euo pipefail

if [ $# -lt 5 ]; then
  sed -n '3,5p' "$0" >&2
  exit 2
fi
# min-version: oldest version a device may roll back to without --force (default: any).
key=$1 installer=$2 version=$3 url=$4 out=$5 rollout=${6:-100} min=${7:-0.0.0}

case $url in https://*) ;; *) echo "installer URL must be https" >&2; exit 2 ;; esac
sha=$(sha256sum "$installer" | cut -d' ' -f1)
size=$(wc -c < "$installer" | tr -d ' ')

printf '{"version":"%s","channel":"stable","min_version":"%s","rollout_percent":%d,"installer_url":"%s","sha256":"%s","size":%d}\n' \
  "$version" "$min" "$rollout" "$url" "$sha" "$size" > "$out"

sig=$(mktemp)
trap 'rm -f "$sig"' EXIT
openssl pkeyutl -sign -inkey "$key" -rawin -in "$out" -out "$sig"
od -An -v -tx1 "$sig" | tr -d ' \n' > "$out.sig"
echo "$out: $version, $size bytes, sha256 $sha"
