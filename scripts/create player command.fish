#!/usr/bin/env fish
# Delegate to Circolate so the generated function uses the real binary path.
set -l script_dir (dirname (status filename))
set -l bin (realpath $script_dir/../target/release/circolate 2>/dev/null)

if not test -x "$bin"
    echo "circolate: build it first with: cargo build --release" >&2
    exit 1
end

$bin install --fish-command
