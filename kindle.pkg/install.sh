#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
PACKAGE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
cd "$PACKAGE"
sha256sum -c SHA256SUMS >/dev/null
launcher=/mnt/us/documents/kherdr.sh
icon=/mnt/us/kherdr/var/kherdr-icon.png
if [ -e "$launcher" ] || [ -L "$launcher" ]; then
    if ! cmp -s "$launcher" "$PACKAGE/scriptlet.sh"; then
        printf 'kherdr: %s already exists and is not this package launcher; leaving it unchanged\n' "$launcher" >&2
        exit 1
    fi
fi
mkdir -p /mnt/us/kherdr/var
temporary=
icon_temporary=$(mktemp /mnt/us/kherdr/var/.kherdr-icon.XXXXXX)
trap 'rm -f -- "$temporary" "$icon_temporary"' 0
cp "$PACKAGE/kherdr-icon.png" "$icon_temporary"
chmod 644 "$icon_temporary"
mv -f "$icon_temporary" "$icon"
mkdir -p /mnt/us/documents
temporary=$(mktemp /mnt/us/documents/.kherdr-launcher.XXXXXX)
trap 'exit 1' HUP INT TERM
cp "$PACKAGE/scriptlet.sh" "$temporary"
chmod 755 "$temporary"
mv -f "$temporary" "$launcher"
printf 'Installed kherdr. Launch kherdr from the library or run: ;kpm launch kherdr\n'
printf 'Settings and SSH credentials live outside the package in /mnt/us/kherdr.\n'
