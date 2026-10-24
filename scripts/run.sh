#!/bin/sh
# run.sh — unified launcher for trackscript-player.

self=$(readlink -f "$0")
share_dir=$(dirname "$self")
root=$(dirname "$share_dir")
bin="$root/target/release/circolate"
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
  test   [file]      run single-file diagnostics
  check  [path]      recursively probe .flp sources for missing
                     dependencies, and .trackscript/.tsz files for
                     referenced samples/instruments that are missing.
                     .mid/.midi/.it sources are skipped (the binary
                     cannot probe them). __MACOSX dirs and macOS
                     resource forks are ignored.
                     Pass --unresolved (or -u) to hide files that
                     have no missing samples, no unresolved fallbacks,
                     no silent channels, and no failed checks.
                     Pass --scaffold-fallbacks to seed share/fallbacks.json
                     from share/required_plugins.json + registry.json.
                     Pass --write-requirements FILE to write a
                     required_plugins.json from the scanned corpus.
  census [path]      tally plugin names across all .flp files in PATH
                     (default "."), writing share/required_plugins.json.
                     Use before check --scaffold-fallbacks.
  measure <plugin>   measure a plugin's default output (RMS + peak in dBFS)
                     by playing a sustained C4. Prints the result.
       --only NAME   measure a single installed plugin by name.
       --all         measure every plugin referenced by share/fallbacks.json
                     and share/registry.json, writing
                     share/plugins_inventory.json.
  plugins [--paths]  list discovered plugins
  fetch              download missing fallback plugins
  smoke  <plugin.vst3> [state.file]
                     VST3 smoke test
  sf2-smoke <file.sf2> [--bank N] [--patch N] [--note N] [--list] [--out FILE.wav]
                     SF2 engine smoke test
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
    exec "$bin"
fi

# ── verb extraction ────────────────────────────────────────────────────
verb=""
verb_explicit=0
first="$1"
case "$first" in
    play|export|dump|test|check|census|measure|plugins|fetch|smoke|sf2-smoke|list|rebuild|help|-h|--help)
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

    sf2-smoke)
        [ $# -lt 1 ] && {
            echo "usage: run.sh sf2-smoke <file.sf2> [--bank N] [--patch N] [--note N] [--list] [--out FILE.wav]" >&2
            exit 1
        }
        exec "$bin" sf2-smoke "$@"
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

# ── shq: single-quote-escape a string for safe re-parsing with eval ────
shq() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

# ── block_has_problems: true iff `$bin check` output needs attention ───
# Parses the trailing "N file(s): X clean, Y with problems, Z missing,
# W fallback(s) (V unresolved), U failed" summary. Block is interesting
# iff any of Y, Z, V, U is non-zero, OR the output mentions a silent
# channel ("channel will be silent"). The silent check exists because
# probe() reports "no fallback configured" as a note, not as a
# `missing` or `unresolved` count — without it, --unresolved would hide
# files whose mix is half dead air.
block_has_problems() {
    summary=$(printf '%s\n' "$1" | grep -E '^[0-9]+ file\(s\): ' | tail -1)
    [ -z "$summary" ] && return 0
    problems=$(printf '%s' "$summary" | sed -nE 's/.* ([0-9]+) with problems.*/\1/p')
    missing=$(printf '%s' "$summary" | sed -nE 's/.* ([0-9]+) missing.*/\1/p')
    unresolved=$(printf '%s' "$summary" | sed -nE 's/.*\(([0-9]+) unresolved\).*/\1/p')
    failed_=$(printf '%s' "$summary" | sed -nE 's/.* ([0-9]+) failed.*/\1/p')
    [ "${problems:-0}" -gt 0 ] && return 0
    [ "${missing:-0}" -gt 0 ] && return 0
    [ "${unresolved:-0}" -gt 0 ] && return 0
    [ "${failed_:-0}" -gt 0 ] && return 0
    silent=$(printf '%s\n' "$1" | grep -c 'channel will be silent')
    [ "${silent:-0}" -gt 0 ] && return 0
    return 1
}

# ── dependency scanners used by `check` ────────────────────────────────
check_trackscript() {
    ts="$1"
    ts_dir=$(dirname "$ts")
    missing=$(grep -oE "'(samples|instruments)/[^']*'" "$ts" 2>/dev/null \
              | tr -d "'" \
              | sort -u \
              | while IFS= read -r p; do
                    [ -e "$ts_dir/$p" ] || printf '  MISS %s\n' "$p"
                done)
    printf '%s\n' "$missing"
    [ -z "$missing" ] && return 0
    return 1
}

check_tsz() {
    tsz="$1"
    if ! command -v unzip >/dev/null 2>&1; then
        printf '  skip: unzip not installed\n'
        return 1
    fi
    tmpd=$(mktemp -d 2>/dev/null) || { printf '  skip: no temp dir\n'; return 1; }
    if ! unzip -q -o "$tsz" -d "$tmpd" 2>/dev/null; then
        printf '  cannot extract archive\n'
        rm -rf "$tmpd"
        return 1
    fi
    ts=$(find "$tmpd" -maxdepth 3 -type f \
             \( -name '*.trackscript' -o -name 'trackscript' \) 2>/dev/null \
         | head -1)
    rc=0
    if [ -z "$ts" ]; then
        printf '  no trackscript inside archive\n'
        rc=1
    else
        check_trackscript "$ts"
        rc=$?
    fi
    rm -rf "$tmpd"
    return "$rc"
}

# ── parse flags and file ───────────────────────────────────────────────
file=""
passthru=""
check_only_unresolved=0
while [ $# -gt 0 ]; do
    case "$1" in
        --unresolved|-u)
            if [ "$verb" = "check" ]; then
                check_only_unresolved=1
            else
                passthru="$passthru $1"
            fi
            shift
            ;;
        --render|--rate|--start|--samples-dir|--out|-o|--only|--sf2|--write-requirements)
            flag="$1"
            shift
            [ $# -eq 0 ] && { echo "flag needs a value" >&2; exit 1; }
            passthru="$passthru $flag $(shq "$1")"
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

    [ -n "$sf2" ] && passthru="$passthru --sf2 $(shq "$sf2")"
}

# ── dispatch ───────────────────────────────────────────────────────────
case "$verb" in
    play)
        if [ -z "$file" ] && [ "$verb_explicit" = 1 ]; then
            file=$(kdialog --getopenfilename "$root" "$play_filter" 2>/dev/null)
            [ -z "$file" ] && exit 0
        fi
        maybe_pick_sf2 "$file"
        eval "set -- $passthru"
        if [ -n "$file" ]; then
            exec "$bin" play "$@" "$file"
        fi
        exec "$bin" play "$@"
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
        eval "set -- $passthru"

        if [ "$user_out" = 1 ]; then
            exec "$bin" export "$@" "$file"
        fi

        stem=$(basename "$file")
        stem=${stem%.*}

        case " $passthru " in
            *' --archive '*|*' -a '*) out="$trackscripts/$stem.tsz" ;;
            *) out="$trackscripts/$stem" ;;
        esac

        mkdir -p "$trackscripts"
        exec "$bin" export "$@" "$file" -o "$out"
        ;;

    dump)
        if [ -z "$file" ]; then
            file=$(kdialog --getopenfilename "$root" "$play_filter" 2>/dev/null)
            [ -z "$file" ] && exit 0
        fi
        eval "set -- $passthru"
        if [ -t 1 ]; then
            "$bin" dump "$@" "$file" | less -R
        else
            exec "$bin" dump "$@" "$file"
        fi
        ;;

    test)
        if [ -z "$file" ]; then
            file=$(kdialog --getopenfilename "$root" "$test_filter" 2>/dev/null)
            [ -z "$file" ] && exit 0
        fi
        eval "set -- $passthru"
        exec "$bin" test "$@" "$file"
        ;;

    census)
        eval "set -- $passthru"
        exec "$bin" census "$@" "${file:-.}"
        ;;

    measure)
        eval "set -- $passthru"
        if [ -n "$file" ]; then
            exec "$bin" measure "$@" "$file"
        fi
        exec "$bin" measure "$@"
        ;;

    check)
        # --scaffold-fallbacks and --write-requirements short-circuit
        # inside the binary before any file walk. run.sh must not loop
        # per-file when they're set, or the output file gets overwritten
        # once per source.
        case " $passthru " in
            *' --scaffold-fallbacks '*|*' --write-requirements '*)
                eval "set -- $passthru"
                if [ -n "$file" ]; then
                    exec "$bin" check "$@" "$file"
                fi
                exec "$bin" check "$@"
                ;;
        esac

        eval "set -- $passthru"
        target="${file:-.}"

        if [ -f "$target" ]; then
            out=$("$bin" check "$@" "$target" 2>&1)
            rc=$?
            if [ "$check_only_unresolved" = 1 ] && ! block_has_problems "$out"; then
                printf '%s: clean (hidden by --unresolved)\n' "$target"
                exit 0
            fi
            printf '%s\n' "$out"
            exit "$rc"
        fi

        if [ ! -d "$target" ]; then
            echo "no such file or directory: $target" >&2
            exit 1
        fi

        # Directory mode. `find -L` follows symlinks so `music/flps` and
        # friends are traversed. `__MACOSX` is pruned (it holds resource
        # forks); `.Trashes` too. `._*` files are excluded explicitly
        # since they also appear outside `__MACOSX` in some trees.
        #
        # IMPORTANT: only `.flp` is probed. The binary's `check` doesn't
        # parse .mid / .midi / .it, and feeding them in produces one
        # "check only supports .flp files" FAIL per file — pure noise.
        n_src=0
        n_src_total=0
        n_ts=0
        n_ts_total=0
        n_tsz=0
        n_tsz_total=0
        n_skipped_clean=0
        n_skipped_nonflp=0
        failed=0
        n_links=0

        while IFS= read -r _; do
            n_links=$((n_links + 1))
        done <<EOF
$(find "$target" -type l 2>/dev/null)
EOF

        # Count non-FLP sources (skipped, not probed).
        while IFS= read -r _; do
            n_skipped_nonflp=$((n_skipped_nonflp + 1))
        done <<EOF
$(find -L "$target" \
    -type d \( -name target -o -name .git -o -name __pycache__ -o -name node_modules \
               -o -name __MACOSX -o -name .Trashes \) -prune \
    -o -type f \( -name '*.it' -o -name '*.mid' -o -name '*.midi' \) ! -name '._*' -print \
    2>/dev/null)
EOF

        # 1. FLP sources — binary probe.
        while IFS= read -r f; do
            [ -z "$f" ] && continue
            out=$("$bin" check "$@" "$f" 2>&1)
            rc=$?
            n_src_total=$((n_src_total + 1))
            if [ "$check_only_unresolved" = 1 ] && ! block_has_problems "$out"; then
                n_skipped_clean=$((n_skipped_clean + 1))
                continue
            fi
            printf '\n── SOURCE: %s ──\n' "$f"
            printf '%s\n' "$out"
            [ "$rc" -ne 0 ] && failed=$((failed + 1))
            n_src=$((n_src + 1))
        done <<EOF
$(find -L "$target" \
    -type d \( -name target -o -name .git -o -name __pycache__ -o -name node_modules \
               -o -name __MACOSX -o -name .Trashes \) -prune \
    -o -type f -name '*.flp' ! -name '._*' -print \
    2>/dev/null)
EOF

        # 2. Trackscripts — shell-level referenced-file check.
        while IFS= read -r ts; do
            [ -z "$ts" ] && continue
            n_ts_total=$((n_ts_total + 1))
            ts_out=$(check_trackscript "$ts")
            ts_rc=$?
            if [ "$ts_rc" -eq 0 ]; then
                if [ "$check_only_unresolved" = 1 ]; then
                    n_skipped_clean=$((n_skipped_clean + 1))
                    continue
                fi
                printf '\n── TRACKSCRIPT: %s ──\n' "$ts"
                echo "  all referenced samples/instruments present"
            else
                printf '\n── TRACKSCRIPT: %s ──\n' "$ts"
                printf '%s\n' "$ts_out"
            fi
            n_ts=$((n_ts + 1))
        done <<EOF
$(find -L "$target" \
    -type d \( -name target -o -name .git -o -name __pycache__ -o -name __MACOSX -o -name .Trashes \) -prune \
    -o -type f -name '*.trackscript' ! -name '._*' -print \
    2>/dev/null)
EOF

        # 3. TSZ archives — extract, then check the inner trackscript.
        while IFS= read -r tsz; do
            [ -z "$tsz" ] && continue
            n_tsz_total=$((n_tsz_total + 1))
            tsz_out=$(check_tsz "$tsz")
            tsz_rc=$?
            if [ "$tsz_rc" -eq 0 ]; then
                if [ "$check_only_unresolved" = 1 ]; then
                    n_skipped_clean=$((n_skipped_clean + 1))
                    continue
                fi
                printf '\n── TSZ: %s ──\n' "$tsz"
                echo "  all referenced samples/instruments present"
            else
                printf '\n── TSZ: %s ──\n' "$tsz"
                printf '%s\n' "$tsz_out"
            fi
            n_tsz=$((n_tsz + 1))
        done <<EOF
$(find -L "$target" \
    -type d \( -name target -o -name .git -o -name __MACOSX -o -name .Trashes \) -prune \
    -o -type f -name '*.tsz' ! -name '._*' -print \
    2>/dev/null)
EOF

        printf '\n────────────────────────────────────────────────────────────\n'
        if [ "$check_only_unresolved" = 1 ]; then
            printf 'followed %d symlink(s); showed %d of %d FLP(s), %d of %d trackscript(s), %d of %d tsz archive(s); %d hidden as clean; %d non-FLP source(s) skipped; %d binary check(s) failed\n' \
                "$n_links" \
                "$n_src" "$n_src_total" \
                "$n_ts" "$n_ts_total" \
                "$n_tsz" "$n_tsz_total" \
                "$n_skipped_clean" "$n_skipped_nonflp" "$failed"
        else
            printf 'followed %d symlink(s); checked %d FLP(s), %d trackscript(s), %d tsz archive(s); %d non-FLP source(s) skipped; %d binary check(s) failed\n' \
                "$n_links" "$n_src_total" "$n_ts_total" "$n_tsz_total" \
                "$n_skipped_nonflp" "$failed"
        fi
        exit "$failed"
        ;;

    plugins)
        eval "set -- $passthru"
        exec "$bin" plugins "$@"
        ;;

    fetch)
        eval "set -- $passthru"
        exec "$bin" fetch "$@"
        ;;
esac
