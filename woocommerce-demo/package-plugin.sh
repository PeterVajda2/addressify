#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "$0")" && pwd)"
plugin_dir="$root_dir/plugin/addresswise-woocommerce"
version="$(sed -n 's/^ \* Version: //p' "$plugin_dir/addresswise-woocommerce.php" | head -n 1)"

if [[ -z "$version" ]]; then
    echo "Could not determine plugin version." >&2
    exit 1
fi

mkdir -p "$root_dir/dist"
archive="$root_dir/dist/addresswise-woocommerce-$version.zip"
rm -f "$archive"
(
    cd "$root_dir/plugin"
    zip -qr "$archive" addresswise-woocommerce \
        -x '*/.DS_Store' -x '*/.git/*' -x '*/node_modules/*'
)
echo "$archive"
