#!/usr/bin/env bash
# Install the mctl display service on a Raspberry Pi: runs `cage -- mctl --kiosk` on the first screen at boot.
#
#   sudo packaging/pi/install-display.sh --user pi --bin /home/pi/mctl/bin/mctl [--enable] [--dry-run]
#
# --user    the account the display runs as (it needs to be in the video, render and input groups, as the
#           first user Raspberry Pi OS creates is)
# --bin     the mctl binary to run (built for aarch64; see docs/pi.md for cross-building)
# --enable  also start it at every boot (otherwise only install it: `systemctl start mctl-display` to try)
# --dry-run print the unit instead of installing it
#
# It needs `cage` and `seatd` (apt install cage seatd). For a display-only Pi also boot to the console:
#   sudo systemctl set-default multi-user.target
set -euo pipefail

user="" bin="" enable=0 dry=0
while [ $# -gt 0 ]; do
    case "$1" in
        --user) user="${2:?--user needs a name}"; shift 2 ;;
        --bin) bin="${2:?--bin needs a path}"; shift 2 ;;
        --enable) enable=1; shift ;;
        --dry-run) dry=1; shift ;;
        *) echo "install-display.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done
if [ -z "$user" ] || [ -z "$bin" ]; then
    echo "install-display.sh: --user and --bin are required" >&2
    exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
unit="$(sed -e "s|@USER@|$user|g" -e "s|@BIN@|$bin|g" "$here/mctl-display.service.in")"

if [ "$dry" = 1 ]; then
    printf '%s\n' "$unit"
    exit 0
fi

[ "$(id -u)" = 0 ] || { echo "install-display.sh: run as root (sudo)" >&2; exit 1; }
id "$user" >/dev/null 2>&1 || { echo "install-display.sh: no such user: $user" >&2; exit 1; }
[ -x "$bin" ] || { echo "install-display.sh: $bin is not an executable file" >&2; exit 1; }
command -v cage >/dev/null 2>&1 || { echo "install-display.sh: cage is not installed (apt install cage seatd)" >&2; exit 1; }

printf '%s\n' "$unit" > /etc/systemd/system/mctl-display.service
systemctl daemon-reload
if [ "$enable" = 1 ]; then
    systemctl enable mctl-display.service
    echo "installed and enabled: it starts at the next boot (or: systemctl start mctl-display)"
else
    echo "installed: systemctl start mctl-display to try it, or re-run with --enable"
fi
