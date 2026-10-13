#!/bin/sh
# Shared launcher for the .desktop files.
#
#   run.sh                   → play with no song (queue widget focused)
#   run.sh play [file]       → play, picking a file if none given
#   run.sh export [file]     → convert (picks a file if none given)
#   run.sh dump   [file]     → print parsed structure
#   run.sh test   [file]     → run diagnostics
#   run.sh check  [path]     → recursive dependency probe
#
# Resolves its own location so the repo can live anywhere, and rewrites
# the sibling .desktop files' Exec=/Icon= lines to match.

self=$(readlink -f "$0")
share_dir=$(dirname "$self")
root=$(dirname "$share_dir")

# ── self-heal desktop file paths ───────────────────────────────────────
# Idempotent: only rewrites files whose stored path doesn't match this
# checkout.  Runs on every invocation so a moved repo fixes itself the
# next time any launcher is used.
for d in "$share_dir"/*.desktop; do
    [ -f "$d" ] || continue
    grep -q "^Exec=$share_dir/run\.sh" "$d" && continue
    sed -i \
        -e "s|^Exec=.*/run\.sh|Exec=$share_dir/run.sh|" \
        -e "s|^Icon=.*/icons/|Icon=$share_dir/icons/|" \
        "$d"
done

# If stdin isn't a tty, we were launched by KDE.  Spawn Konsole that holds
# open, and re-run this same script inside it.
if ! [ -t 0 ]; then
    exec konsole --hold -e "$self" "$@"
fi

# ── verb ───────────────────────────────────────────────────────────────
# No arguments at all means "open the player with no song" — the queue
# widget is the entry point, no file dialog.  Any argument at all means
# the user typed a verb, and the verb decides whether a dialog appears.
verb_explicit=0
if [ $# -eq 0 ]; then
    verb="play"
else
    verb="$1"
    shift
    verb_explicit=1
fi

case "$verb" in
    play|export|dump|test|check) ;;
    *)
        echo "unknown verb: $verb" >&2
        exit 1
        ;;
esac

# ── collect flags and the file path ────────────────────────────────────
# Flags may appear before or after the file.  Value-taking flags consume
# two arguments so the flag's value never gets mistaken for the file:
#
#   run.sh play --render out.wav song.flp
#   run.sh play song.flp --render out.wav
#   run.sh play song.flp --start 12 --no-cache
flags=""
file=""
while [ $# -gt 0 ]; do
    case "$1" in
        --render|--rate|--start|--samples-dir|--out|-o)
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

filter="*.trackscript *.tsz *.flp *.it|Trackscript sources"

# ── file dialog ────────────────────────────────────────────────────────
# check never prompts (it recurses).  Bare `run.sh` never prompts (it
# opens the empty player).  Explicit `play`/`export`/etc. with no file
# prompts as before.
case "$verb" in
    check)
        ;;
    play)
        if [ "$verb_explicit" = "1" ] && [ -z "$file" ]; then
            file=$(kdialog --getopenfilename "$root" "$filter")
            [ -z "$file" ] && exit 0
        fi
        ;;
    *)
        if [ -z "$file" ]; then
            file=$(kdialog --getopenfilename "$root" "$filter")
            [ -z "$file" ] && exit 0
        fi
        ;;
esac

bin="$root/target/release/trackscript-player"
if [ ! -x "$bin" ]; then
    echo "binary not found: $bin" >&2
    echo "run: cargo build --release" >&2
    exit 1
fi

case "$verb" in
    play)
        # No file = start empty (queue widget focused).  File given =
        # play it immediately.
        if [ -z "$file" ]; then
            exec "$bin" play $flags
        else
            exec "$bin" play $flags "$file"
        fi
        ;;
    export)
        # --archive determines its own output path; skip the picker.
        case " $flags " in
            *' --archive '*|*' -a '*)
                exec "$bin" export $flags "$file"
                ;;
        esac
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
    check)
        # No file dialog: recursive, defaults to the current directory.
        exec "$bin" check $flags "${file:-.}"
        ;;
esac