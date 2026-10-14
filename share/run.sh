#!/bin/sh
# run.sh — unified launcher for trackscript-player.

self=$(readlink -f "$0")
share_dir=$(dirname "$self")
root=$(dirname "$share_dir")
bin="$root/target/release/trackscript-player"
trackscripts="$root/trackscripts"

# ── self-heal desktop file paths ───────────────────────────────────────
for d in "$share_dir"/*.desktop; do
    [ -f "$d" ] || continue
    grep -q "^Exec=$share_dir/run\.sh" "$d" && continue
    sed -i \
        -e "s|^Exec=.*/run\.sh|Exec=$share_dir/run.sh|" \
        -e "s|^Icon=.*/icons/|Icon=$share_dir/icons/|" \
        "$d"
done

# ── konsole respawn ────────────────────────────────────────────────────
if ! [ -t 0 ]; then
    exec konsole --hold -e "$self" "$@"
fi

# ── usage ──────────────────────────────────────────────────────────────
usage() {
    cat <<'EOF'
usage: run.sh <verb> [args] [flags]

  (no args)          open the player with no song
  <file>             play <file>  (implicit "play")
  play [file]        play a song.  Accepts a .trackscript, .tsz, source
                     file, or a directory containing a trackscript.
  export <file>      export to trackscripts/<stem>/
  export -a <file>   export to trackscripts/<stem>.tsz
  dump   [file]      print parsed structure as JSON
  test   [file]      run diagnostics
  check  [path]      recursively probe for missing dependencies
  plugins [--paths]  list discovered plugins
  fetch              download missing fallback plugins
  smoke  <plugin.vst3> [state.file]
                     VST3 smoke test
  list               list trackscripts/
  rebuild            cargo build --release
  help               this message

Flags (passed through to the binary):
  --render F --rate N --start N --samples-dir D -o D --archive --sf2 D
  --skip-native --no-cache --no-master-fx --no-watch

When a .mid is selected, or a trackscript whose Config says
"SoundFont: ask;", a second dialog asks which SoundFont to use.
Cancel it to fall back to the SF2 baked into the trackscript (or, for
a raw MIDI, whatever the converter finds on disk).
EOF
}

# ── no args: open empty player ─────────────────────────────────────────
if [ $# -eq 0 ]; then
    if [ ! -x "$bin" ]; then
        echo "binary not found: $bin" >&2
        echo "run: run.sh rebuild" >&2
        exit 1
    fi
    exec "$bin" play
fi

# ── verb extraction ────────────────────────────────────────────────────
verb=""
verb_explicit=0
first="$1"
case "$first" in
    play|export|dump|test|check|plugins|fetch|smoke|list|rebuild|help|-h|--help)
        verb="$first"
        verb_explicit=1
        shift
        ;;
    *)
        verb="play"
        ;;
esac

# ── verbs that don't fit the (file, flags) shape ──────────────────────
case "$verb" in
    help|-h|--help)
        usage
        exit 0
        ;;

    smoke)
        [ $# -lt 1 ] && { echo "usage: run.sh smoke <plugin.vst3> [state.file]" >&2; exit 1; }
        exec "$bin" vst-smoke "$@"
        ;;

    rebuild)
        cd "$root" && exec cargo build --release
        ;;

    list)
        [ ! -d "$trackscripts" ] && { echo "no trackscripts directory"; exit 0; }
        for entry in "$trackscripts"/* "$trackscripts"/.[!.]*; do
            [ -e "$entry" ] || continue
            name=$(basename "$entry")
            if [ -d "$entry" ]; then
                printf '  %-40s  [dir]\n' "$name"
            else
                size=$(du -h "$entry" 2>/dev/null | cut -f1)
                printf '  %-40s  %s\n' "$name" "$size"
            fi
        done
        exit 0
        ;;
esac

# ── binary must exist ─────────────────────────────────────────────────
if [ ! -x "$bin" ]; then
    echo "binary not found: $bin" >&2
    echo "run: run.sh rebuild" >&2
    exit 1
fi

# ── parse flags and file ───────────────────────────────────────────────
file=""
passthru=""
while [ $# -gt 0 ]; do
    case "$1" in
        --render|--rate|--start|--samples-dir|--out|-o|--only|--sf2)
            passthru="$passthru $1"
            shift
            [ $# -eq 0 ] && { echo "flag needs a value" >&2; exit 1; }
            passthru="$passthru $1"
            shift
            ;;
        -*)
            passthru="$passthru $1"
            shift
            ;;
        *)
            [ -z "$file" ] && file="$1" || { echo "unexpected extra argument: $1" >&2; exit 1; }
            shift
            ;;
    esac
done

# ── directory inputs ───────────────────────────────────────────────────
# A directory means "the extracted form of a trackscript".  Look for
# the trackscript inside.
#
# Two possible names:
#   * `trackscript` — the bare ENTRY_KEY name used inside a .tsz.  When
#     extracted, that's what lands on disk.  The binary recognises it
#     via SourceKind::from_path.
#   * `<stem>.trackscript` — the name directory exports use.
if [ -d "$file" ]; then
    case "$verb" in
        play|dump|test)
            ts_dir="$file"
            ts_file=""

            if [ -f "$ts_dir/trackscript" ]; then
                ts_file="$ts_dir/trackscript"
            else
                for cand in "$ts_dir"/*.trackscript; do
                    [ -f "$cand" ] && ts_file="$cand" && break
                done
            fi

            if [ -z "$ts_file" ]; then
                echo "no trackscript found in $ts_dir" >&2
                exit 1
            fi
            file="$ts_file"
            ;;
    esac
fi

play_filter='*.trackscript *.tsz *.flp *.it *.mid *.midi|Trackscript sources'
test_filter='*.trackscript *.tsz *.flp *.it *.mid *.midi|Trackscript sources'
src_filter='*.flp *.it *.mid *.midi|FLP / IT / MIDI sources'

# ── SoundFont picker ───────────────────────────────────────────────────
# Prompts for an SF2 when:
#   * the source file is a .mid / .midi, or
#   * the file is a .trackscript whose Config block says "SoundFont: ask;"
# Skipped if --sf2 already appears in $passthru.
#
# Sets $passthru to include --sf2 <path> if the user picks one.
# Cancel → no change; the binary falls back to whatever it would
# normally use.
maybe_pick_sf2() {
    [ -z "$1" ] && return
    case " $passthru " in
        *' --sf2 '*) return ;;
    esac

    needs_sf2=0
    case "$1" in
        *.[mM][iI][dD]|*.[mM][iI][dD][iI])
            needs_sf2=1
            ;;
        *trackscript)
            grep -q '^[[:space:]]*SoundFont:[[:space:]]*ask' "$1" 2>/dev/null && needs_sf2=1
            ;;
        *.tsz)
            if command -v unzip >/dev/null 2>&1; then
                unzip -p "$1" 2>/dev/null \
                    | grep -q '^[[:space:]]*SoundFont:[[:space:]]*ask' && needs_sf2=1
            fi
            ;;
    esac

    [ "$needs_sf2" = 0 ] && return

    start_dir="$root/soundfonts"
    [ -d "$start_dir" ] || start_dir="$root"

    sf2=$(kdialog --title 'Select SoundFont' \
                  --getopenfilename "$start_dir" \
                  '*.sf2 *.sf3 *.SF2 *.SF3|SoundFont files' 2>/dev/null)

    [ -n "$sf2" ] && passthru="$passthru --sf2 $sf2"
}

# ── dispatch ───────────────────────────────────────────────────────────
case "$verb" in
    play)
        if [ -z "$file" ] && [ "$verb_explicit" = 1 ]; then
            file=$(kdialog --getopenfilename "$root" "$play_filter" 2>/dev/null)
            [ -z "$file" ] && exit 0
        fi
        maybe_pick_sf2 "$file"
        [ -n "$file" ] && exec "$bin" play $passthru "$file"
        exec "$bin" play $passthru
        ;;

    export)
        user_out=0
        case " $passthru " in
            *' -o '*|*' --out '*) user_out=1 ;;
        esac

        if [ -z "$file" ]; then
            file=$(kdialog --getopenfilename "$root" "$src_filter" 2>/dev/null)
            [ -z "$file" ] && exit 0
        fi

        maybe_pick_sf2 "$file"

        [ "$user_out" = 1 ] && exec "$bin" export $passthru "$file"

        stem=$(basename "$file")
        stem=${stem%.*}

        case " $passthru " in
            *' --archive '*|*' -a '*) out="$trackscripts/$stem.tsz" ;;
            *) out="$trackscripts/$stem" ;;
        esac

        mkdir -p "$trackscripts"
        exec "$bin" export $passthru "$file" -o "$out"
        ;;

    dump)
        if [ -z "$file" ]; then
            file=$(kdialog --getopenfilename "$root" "$play_filter" 2>/dev/null)
            [ -z "$file" ] && exit 0
        fi
        if [ -t 1 ]; then
            "$bin" dump $passthru "$file" | less -R
        else
            exec "$bin" dump $passthru "$file"
        fi
        ;;

    test)
        if [ -z "$file" ]; then
            file=$(kdialog --getopenfilename "$root" "$test_filter" 2>/dev/null)
            [ -z "$file" ] && exit 0
        fi
        exec "$bin" test $passthru "$file"
        ;;

    check)
        exec "$bin" check $passthru "${file:-.}"
        ;;

    plugins)
        exec "$bin" plugins $passthru
        ;;

    fetch)
        exec "$bin" fetch $passthru
        ;;
esac