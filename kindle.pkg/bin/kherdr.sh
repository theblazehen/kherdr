#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
PACKAGE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P) || exit 1
DATA=${KHERDR_DATA_DIR:-/mnt/us/kherdr}
case "$DATA" in /*) ;; *) printf 'kherdr: data directory must be absolute\n' >&2; exit 1 ;; esac
export HOME="$DATA/etc" DISPLAY="${DISPLAY:-:0}" TERM=xterm-256color
umask 077

report() {
    printf 'kherdr: %s\n' "$*" >&2
}

mkdir -p "$DATA/var" || exit 1
exec 2>>"$DATA/var/launcher.log"
cd "$DATA" || exit 1

# This lock survives package replacement and is shared by all KPM versions.
exec 9>"$DATA/var/launcher.lock" || exit 1
if ! flock -n 9; then
    report 'Cannot acquire launcher lock; another instance may already be running'
    exit 1
fi

# Do not snapshot credentials while the old UI can still change them. Keep its
# lock until this UI exits too; retained files are a backup, not a second writer.
legacy=/mnt/us/extensions/kherdr-dev
if [ -d "$legacy/var" ]; then
    exec 8>"$legacy/var/launcher.lock" || exit 1
    if ! flock -n 8; then
        report 'Quit the manual kherdr installation before launching the KPM package'
        exit 1
    fi
fi

# Kindle's fsp can SIGBUS a process after its executable is unlinked. Run both
# the UI/SSH helper and Herdr from an immutable pair outside KPM's package tree.
# Never overwrite or remove an old pair: live sessions can still execute it.
runtime_id=$(sha256sum < "$PACKAGE/BINARY_SHA256SUMS") || exit 1
runtime_id=${runtime_id%% *}
runtime="$DATA/runtime/$runtime_id"
if [ ! -d "$runtime" ]; then
    mkdir -p "$DATA/runtime" || exit 1
    (
        set -e
        staging=$(mktemp -d "$DATA/runtime/.prepare.XXXXXX")
        trap 'rm -rf "$staging"' 0
        trap 'exit 1' HUP INT TERM
        cp "$PACKAGE/bin/kherdr" "$staging/kherdr"
        cp "$PACKAGE/bin/herdr" "$staging/herdr"
        cp "$PACKAGE/BINARY_SHA256SUMS" "$staging/SHA256SUMS"
        chmod 755 "$staging/kherdr" "$staging/herdr"
        (cd "$staging" && sha256sum -c SHA256SUMS >/dev/null)
        mv "$staging" "$runtime"
    ) || exit 1
fi
if [ ! -f "$runtime/kherdr" ] || [ ! -f "$runtime/herdr" ] ||
    ! cmp -s "$runtime/SHA256SUMS" "$PACKAGE/BINARY_SHA256SUMS"; then
    report 'Retained runtime is incomplete; refusing to overwrite potentially running executables'
    exit 1
fi
export PATH="$runtime:${PATH:-/usr/bin:/bin:/usr/sbin:/sbin}"
if [ ! -e "$HOME" ] && [ -d "$legacy/etc" ]; then
    "$runtime/kherdr" --prepare-state "$HOME" "$legacy/etc" || exit 1
else
    "$runtime/kherdr" --prepare-state "$HOME" || exit 1
fi

child=
pending_signal=
signal_status=0
wait_interrupted=0
restore_needed=0

forward_signal() {
    pending_signal=$1
    signal_status=$2
    wait_interrupted=1
    if [ -n "$child" ]; then
        kill -s "$pending_signal" "$child" 2>/dev/null || :
    fi
}

cleanup() {
    status=$?
    trap - 0
    trap '' HUP INT TERM
    if [ "$restore_needed" -eq 1 ]; then
        if ! lipc-set-prop com.lab126.powerd preventScreenSaver "$screensaver_value"; then
            report "Failed to restore preventScreenSaver to $screensaver_value"
            [ "$status" -ne 0 ] || status=1
        fi
    fi
    exit "$status"
}

trap cleanup 0
trap 'forward_signal HUP 129' HUP
trap 'forward_signal INT 130' INT
trap 'forward_signal TERM 143' TERM

if ! screensaver_value=$(lipc-get-prop com.lab126.powerd preventScreenSaver); then
    report 'Cannot read preventScreenSaver; refusing to change an unknown value'
    exit 1
fi
case "$screensaver_value" in
    \[*\]) screensaver_value=${screensaver_value#\[}; screensaver_value=${screensaver_value%\]} ;;
esac
case "${screensaver_value#-}" in
    ''|*[!0-9]*) report 'Unexpected preventScreenSaver value; leaving it unchanged'; exit 1 ;;
esac
restore_needed=1
if ! lipc-set-prop com.lab126.powerd preventScreenSaver 1; then
    report 'Failed to inhibit the screensaver'
    exit 1
fi
[ "$signal_status" -eq 0 ] || exit "$signal_status"

# Never stop Herdr servers here: their PTYs outlive the client and its package.
"$runtime/kherdr" --config "$HOME/connection.ini" "$@" 9>&- 8>&- &
child=$!
if [ -n "$pending_signal" ]; then
    kill -s "$pending_signal" "$child" 2>/dev/null || :
fi
while :; do
    wait_interrupted=0
    wait "$child"
    app_status=$?
    [ "$wait_interrupted" -eq 1 ] || break
done
child=
if [ "$app_status" -eq 0 ] && [ "$signal_status" -ne 0 ]; then
    app_status=$signal_status
fi
if [ "$app_status" -ne 0 ]; then
    report "App exited with status $app_status (configuration: $HOME/connection.ini)"
fi
exit "$app_status"
