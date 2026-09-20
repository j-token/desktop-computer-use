#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
UUID='desktop-computer-use@local'
requested_variant="${1:-auto}"

major=''
if command -v gnome-shell >/dev/null 2>&1; then
    major="$(gnome-shell --version | sed -nE 's/.* ([0-9]+)\..*/\1/p')"
fi

case "$requested_variant" in
    modern) variant=modern ;;
    legacy) variant=legacy ;;
    auto)
        if [[ "$major" =~ ^[0-9]+$ ]] && (( major >= 45 )); then
            variant=modern
        else
            variant=legacy
        fi
        ;;
    *)
        printf 'Usage: %s [auto|modern|legacy]\n' "$0" >&2
        exit 2
        ;;
esac

archive="$SCRIPT_DIR/dist/desktop-computer-use-gnome-$variant.zip"
if [[ ! -f "$archive" ]]; then
    printf '%s\n' "Archive not found; run extensions/package.sh first: $archive" >&2
    exit 1
fi

if command -v gnome-extensions >/dev/null 2>&1; then
    gnome-extensions install --force "$archive"
else
    destination="${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/$UUID"
    stage="$(mktemp -d)"
    trap 'rm -rf -- "$stage"' EXIT
    if ! command -v unzip >/dev/null 2>&1; then
        printf '%s\n' 'install.sh requires gnome-extensions or unzip.' >&2
        exit 2
    fi
    unzip -q "$archive" -d "$stage"
    mkdir -p "$(dirname -- "$destination")"
    rm -rf -- "$destination"
    mkdir -p "$destination"
    cp -R -- "$stage"/. "$destination"/
fi

printf '%s\n' "Installed $variant GNOME extension locally as $UUID."
printf '%s\n' "Enable it with: gnome-extensions enable $UUID"
