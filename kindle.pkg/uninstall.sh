#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
set -eu
PACKAGE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
# Leave the launcher in place throughout upgrades. KPM owns package deletion;
# neither upgrading nor uninstalling owns the user's data or running servers.
if [ "${1:-}" != upgrade ]; then
    launcher=/mnt/us/documents/kherdr.sh
    if [ -f "$launcher" ] && cmp -s "$launcher" "$PACKAGE/scriptlet.sh"; then
        rm -- "$launcher"
    fi
    printf 'Removed the kherdr launcher. Settings and SSH credentials remain in /mnt/us/kherdr; Herdr sessions were not stopped.\n'
fi
