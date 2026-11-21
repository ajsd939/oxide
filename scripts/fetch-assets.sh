#!/usr/bin/env bash
# Downloads the vendored frontend assets (Monaco editor + xterm.js) into ui/.
# Required once after cloning — these folders are gitignored to keep the repo light.
set -euo pipefail

UI="$(cd "$(dirname "$0")/.." && pwd)/ui"
mkdir -p "$UI/monaco" "$UI/xterm" "$TMPDIR/rustide-assets" 2>/dev/null || mkdir -p "$UI/monaco" "$UI/xterm" /tmp/rustide-assets
TMP="${TMPDIR:-/tmp}/rustide-assets"

fetch() {
    local url="$1" tar="$2" src="$3" dest="$4"
    echo "Fetching $url"
    curl -sL "$url" -o "$TMP/$tar"
    mkdir -p "$TMP/${tar%.tgz}" "$dest"
    tar -xzf "$TMP/$tar" -C "$TMP/${tar%.tgz}"
    cp -r "$TMP/${tar%.tgz}/$src/." "$dest/"
}

fetch "https://registry.npmjs.org/monaco-editor/-/monaco-editor-0.52.2.tgz" monaco.tgz package/min "$UI/monaco"
fetch "https://registry.npmjs.org/@xterm/xterm/-/xterm-5.5.0.tgz" xterm.tgz package/lib "$UI/xterm"
fetch "https://registry.npmjs.org/@xterm/addon-fit/-/addon-fit-0.10.0.tgz" fit.tgz package/lib "$UI/xterm"

echo "Assets ready:"
ls -la "$UI/monaco/vs/loader.js" "$UI/xterm/xterm.js"
