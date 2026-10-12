#!/bin/sh
# Shared launcher for the .desktop files.  $1 is the verb (play/export/dump/test),
# then optional flags (e.g. --raw), then an optional file to operate on.
#
# Resolves its own location so the repo can live anywhere, and rewrites the
# sibling .desktop files' Exec=/Icon= lines to match.

self=$(readlink -f "$0")
root=$(dirname "$self")

# ── self-heal desktop file paths ───────────────────────────────────────
# Idempotent: only rewrites files whose stored path doesn't match this
# checkout.  Runs on every invocation so a moved repo fixes itself the
# next time any launcher is used.
for d in "$root"/*.desktop; do
    [ -f "$d" ] || continue
    grep -q "^Exec=$root/run\.sh" "$d" && continue
    sed -i \
        -e "s|^Exec=.*/run\.sh|Exec=$root/run.sh|" \
        -e "s|^Icon=.*/icons/|Icon=$root/icons/|" \
        "$d"
done

# If stdin isn't a tty, we were launched by KDE.  Spawn Konsole that holds
# open, and re-run this same script inside it.
if ! [ -t 0 ]; then
    exec konsole --hold -e "$self" "$@"
fi

if [ $# -eq 0 ]; then
    echo "usage: $0 <play|export|dump|test> [flags] [file]" >&2
    exit 1
fi

verb="$1"
shift

case "$verb" in
    play|export|dump|test) ;;
    *)
        echo "unknown verb: $verb" >&2
        exit 1
        ;;
esac

# Collect flags and the file path.  Flags may appear before or after the
# file.  Value-taking flags consume two arguments, so the flag's value
# never gets mistaken for the file:
#
#   run.sh play --render out.wav song.flp
#   run.sh play song.flp --render out.wav
#   run.sh play song.flp --start 12 --no-cache
#
# Both work, and any of the value-taking flags can appear in any order.
flags=""
file=""
while [ $# -gt 0 ]; do
    case "$1" in
        --render|--rate|--start|--samples-dir)
            # Value-taking flag: consume the flag and its value.
            flags="$flags $1"
            shift
            if [ $# -eq 0 ]; then
                echo "flag $1 needs a value" >&2
                exit 1
            fi
            flags="$flags $1"
            shift
            ;;
        -*)
            # Boolean flag: consume just the flag.
            flags="$flags $1"
            shift
            ;;
        *)
            if [ -z "$file" ]; then
                file="$1"
            else
                echo "unexpected positional argument: $1" >&2
                exit 1
            fi
            shift
            ;;
    esac
done

filter="*.trackscript *.flp *.it|Trackscript sources"

if [ -z "$file" ]; then
    file=$(kdialog --getopenfilename . "$filter")
    [ -z "$file" ] && exit 0
fi

bin="$root/target/release/trackscript-player"
if [ ! -x "$bin" ]; then
    echo "binary not found: $bin" >&2
    echo "run: cargo build --release" >&2
    exit 1
fi

case "$verb" in
    play)
        exec "$bin" play $flags "$file"
        ;;
    export)
        # Ask for an output directory next to the source file.
        out=$(kdialog --getexistingdirectory "$(dirname "$file")" \
                --title "Where to write?")
        [ -z "$out" ] && exit 0
        exec "$bin" export $flags "$file" -o "$out"
        ;;
    dump)
        # Page it only when stdout is a terminal, so `> file` still works.
        if [ -t 1 ]; then
            "$bin" dump $flags "$file" | less
        else
            "$bin" dump $flags "$file"
        fi
        ;;
    test)
        "$bin" test $flags "$file"
        ;;
esac