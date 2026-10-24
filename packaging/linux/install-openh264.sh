#!/bin/sh
# Downloads Cisco's prebuilt OpenH264 2.6.0 library, which OpenDesk on Linux loads at runtime for H.264 video.
# Without it OpenDesk still works and streams JPEG only.
#
# Why a separate download: Cisco pays the H.264 patent royalties for its own binary, but only when the user
# downloads it from Cisco. OpenDesk therefore never bundles or compiles the codec. See https://www.openh264.org/faq.html
#
#   install-openh264.sh            installs for the current user  (~/.local/share/opendesk)
#   sudo install-openh264.sh --system   installs for every user    (/usr/lib/opendesk)
set -eu

VERSION=2.6.0
case "$(uname -m)" in
  x86_64 | amd64) ARCH=linux64; SHA=2f0cde7c6a6abcf5cae76942894ea42897fa677bce4ed6c91a24dd1b041d5f04 ;;
  aarch64 | arm64) ARCH=linux-arm64; SHA=12e7b33623667cdab0e575170c147b1b36eadb77d0d2aa7ceb5afd3e58902140 ;;
  armv7l | armhf) ARCH=linux-arm; SHA=df91866de0e93773019e30a8f2bdee8b15de4abe2bf89a228ae9f064ff1e85bb ;;
  i386 | i686) ARCH=linux32; SHA=a46589ccc95df7565ff8b1722d3dead29c0809be28322dc763767e0aa35a6443 ;;
  *) echo "No Cisco OpenH264 build for $(uname -m); OpenDesk will use JPEG only." >&2; exit 1 ;;
esac
FILE="libopenh264-$VERSION-$ARCH.8.so"

if [ "${1:-}" = "--system" ]; then
  DEST=/usr/lib/opendesk
else
  DEST="${XDG_DATA_HOME:-$HOME/.local/share}/opendesk"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
URL="http://ciscobinary.openh264.org/$FILE.bz2"
echo "Downloading $URL"
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$URL" -o "$TMP/$FILE.bz2"
else
  wget -q "$URL" -O "$TMP/$FILE.bz2"
fi
bunzip2 "$TMP/$FILE.bz2"
# The download is plain HTTP (Cisco serves no HTTPS): accept only the exact binary OpenDesk checks for too.
echo "$SHA  $TMP/$FILE" | sha256sum -c - >/dev/null || { echo "Checksum mismatch, not installing." >&2; exit 1; }
mkdir -p "$DEST"
install -m 0644 "$TMP/$FILE" "$DEST/$FILE"
echo "Installed $DEST/$FILE. Restart OpenDesk to enable H.264."
