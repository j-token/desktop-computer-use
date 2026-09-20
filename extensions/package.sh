#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
DIST_DIR="$SCRIPT_DIR/dist"
UUID='desktop-computer-use@local'

if ! command -v zip >/dev/null 2>&1; then
    printf '%s\n' 'package.sh requires the zip utility (apt install zip).' >&2
    exit 2
fi

mkdir -p "$DIST_DIR"

package_variant() {
    local variant="$1"
    local source_dir="$SCRIPT_DIR/gnome-$variant"
    local stage_dir="$DIST_DIR/$UUID"
    local archive="$DIST_DIR/desktop-computer-use-gnome-$variant.zip"

    if [[ ! -d "$source_dir" ]]; then
        printf 'Missing extension source: %s\n' "$source_dir" >&2
        exit 1
    fi

    rm -rf -- "$stage_dir"
    rm -f -- "$archive"
    mkdir -p "$stage_dir"
    cp -R -- "$source_dir"/. "$stage_dir"/

    if command -v glib-compile-schemas >/dev/null 2>&1; then
        glib-compile-schemas "$stage_dir/schemas"
    fi

    (
        cd "$stage_dir"
        zip -q -r "$archive" .
    )
    rm -rf -- "$stage_dir"
    printf '%s\n' "$archive"
}

package_variant modern
package_variant legacy
