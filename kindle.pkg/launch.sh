#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
PACKAGE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P) || exit 1
exec "$PACKAGE/bin/kherdr.sh" "$@"
