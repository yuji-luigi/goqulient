#!/bin/bash
# One VSIX per engine binary (docs/RELEASING.md): every goku-engine-server[.exe] under <engines dir>, in a folder named
# after its target (`linux-x64/`, or the `engine-linux-x64/` of older workflow artifacts), becomes
# <out dir>/goqulient-<target>.vsix. Run it in vscode/ after `npm ci` and a production build; extra arguments go to
# `vsce package` (e.g. --pre-release). extension-release.yml and `scripts/dev.sh vscode package` call it.
#
# Not in a folder Docker shares from a Mac: that disk is case-insensitive, and once something has looked up
# `readme.md`, the Linux side lists README.md under both spellings, which vsce rejects as "the same case insensitive
# path". `scripts/dev.sh vscode package` copies the extension to the container's own disk first.
set -euo pipefail
engines=$1 out=$2
shift 2
# vsce ignores repository.directory: point README links and images at vscode/ explicitly.
base=(--baseContentUrl https://github.com/yuji-luigi/goqulient/blob/HEAD/vscode
  --baseImagesUrl https://github.com/yuji-luigi/goqulient/raw/HEAD/vscode)
mapfile -t exes < <(find "$engines" -type f -name 'goku-engine-server*' | sort)
((${#exes[@]})) || { echo "no goku-engine-server under $engines" >&2; exit 1; }
mkdir -p "$out" # vsce doesn't create it
for exe in "${exes[@]}"; do
  target=$(basename "$(dirname "$exe")")
  target=${target#engine-}
  rm -rf bin && mkdir -p "bin/$target" && cp "$exe" "bin/$target/" && chmod +x "bin/$target/"*
  npx --yes @vscode/vsce@4 package --target "$target" "${base[@]}" "$@" -o "$out/goqulient-$target.vsix"
done
rm -rf bin
