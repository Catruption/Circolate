#!/bin/sh
# Shared helpers for acquire scripts.  Sourced by per-plugin entry
# scripts (acquire/<name>.sh) AND runnable directly:
#
#   ./acquire.sh                 # install everything it knows how to
#   . acquire.sh                 # load helpers only
#
# Every script installs into $PLUGINS_DIR, which defaults to <repo>/plugins.
# Soundfonts go into $SOUNDFONTS_DIR, default <repo>/soundfonts.

set -e

# ── repo root detection ───────────────────────────────────────────────
#
# `$(dirname "$0")/../..` only works when sourced from acquire/<name>.sh.
# Run directly (./acquire.sh) from the repo root and it walks out of the
# repo.  So: honour $CIRCOLATE_ROOT if set, otherwise look for acquire.sh
# in the script's own directory, then one level up.

if [ -n "$CIRCOLATE_ROOT" ]; then
    _repo_root=$(cd "$CIRCOLATE_ROOT" && pwd)
else
    _script_dir=$(cd "$(dirname "$0")" && pwd)
    if [ -f "$_script_dir/acquire.sh" ]; then
        _repo_root="$_script_dir"
    elif [ -f "$_script_dir/../acquire.sh" ]; then
        _repo_root=$(cd "$_script_dir/.." && pwd)
    else
        _repo_root="$_script_dir"
    fi
fi

PLUGINS_DIR="${PLUGINS_DIR:-$_repo_root/plugins}"
SOUNDFONTS_DIR="${SOUNDFONTS_DIR:-$_repo_root/soundfonts}"
mkdir -p "$PLUGINS_DIR"

# Per-run scratch space.  Using $PLUGINS_DIR/.tmp.$$ instead of /tmp so
# paths are visible in the tree and not subject to tmpfs quirks.
_scratch=""
_scratch_init() {
    [ -n "$_scratch" ] && return 0
    _scratch="$PLUGINS_DIR/.tmp.$$"
    mkdir -p "$_scratch"
}
_scratch_cleanup() {
    [ -n "$_scratch" ] && [ -d "$_scratch" ] && rm -rf "$_scratch"
    _scratch=""
}
trap _scratch_cleanup EXIT INT TERM HUP

if [ "$(basename "$0")" = "acquire.sh" ]; then
    printf '  repo root:  %s\n' "$_repo_root"
    printf '  plugins:    %s\n' "$PLUGINS_DIR"
    printf '  soundfonts: %s\n' "$SOUNDFONTS_DIR"
fi

# ── download helper ───────────────────────────────────────────────────

_download() {
    _dl_url="$1"; _dl_dest="$2"
    printf '    GET %s\n' "$_dl_url"
    curl -fL --progress-bar --retry 3 "$_dl_url" -o "$_dl_dest"
}

# ── extraction helpers ────────────────────────────────────────────────
#
# Signature for all of them: _extract_X <archive> <dest>
# Variables are prefixed _ex_ so they can't collide with the caller's
# $archive / $dest (POSIX sh has no `local`).

_extract_tar() {
    _ex_archive="$1"; _ex_dest="$2"
    if [ -z "$_ex_archive" ] || [ -z "$_ex_dest" ]; then
        echo "    _extract_tar: missing arg (archive='$_ex_archive' dest='$_ex_dest')" >&2
        return 1
    fi
    mkdir -p "$_ex_dest"
    tar -xf "$_ex_archive" -C "$_ex_dest"
}

_extract_zip() {
    _ex_archive="$1"; _ex_dest="$2"
    if [ -z "$_ex_archive" ] || [ -z "$_ex_dest" ]; then
        echo "    _extract_zip: missing arg (archive='$_ex_archive' dest='$_ex_dest')" >&2
        return 1
    fi
    mkdir -p "$_ex_dest"
    unzip -q "$_ex_archive" -d "$_ex_dest"
}

_extract_7z() {
    _ex_archive="$1"; _ex_dest="$2"
    if [ -z "$_ex_archive" ] || [ -z "$_ex_dest" ]; then
        echo "    _extract_7z: missing arg (archive='$_ex_archive' dest='$_ex_dest')" >&2
        return 1
    fi
    mkdir -p "$_ex_dest"
    if ! command -v 7z >/dev/null 2>&1; then
        echo "    7z not found — install p7zip" >&2
        return 1
    fi
    7z x -y -o"$_ex_dest" "$_ex_archive" > /dev/null
}

_extract_deb() {
    _ex_archive="$1"; _ex_dest="$2"
    if [ -z "$_ex_archive" ] || [ -z "$_ex_dest" ]; then
        echo "    _extract_deb: missing arg (archive='$_ex_archive' dest='$_ex_dest')" >&2
        return 1
    fi
    mkdir -p "$_ex_dest"
    if ! command -v ar >/dev/null 2>&1; then
        echo "    ar not found — install binutils" >&2
        return 1
    fi
    _ex_tmp=$(mktemp -d)
    ( cd "$_ex_tmp" && ar x "$_ex_archive" )
    _ex_data=$(ls "$_ex_tmp"/data.tar.* 2>/dev/null | head -n1)
    if [ -z "$_ex_data" ]; then
        echo "    no data.tar.* inside $_ex_archive" >&2
        rm -rf "$_ex_tmp"
        return 1
    fi
    case "$_ex_data" in
        *.xz)  tar -xJf "$_ex_data" -C "$_ex_dest" ;;
        *.zst) tar --zstd -xf "$_ex_data" -C "$_ex_dest" ;;
        *.gz)  tar -xzf "$_ex_data" -C "$_ex_dest" ;;
        *)     tar -xf "$_ex_data" -C "$_ex_dest" ;;
    esac
    rm -rf "$_ex_tmp"
}

# ── VST3 bundle discovery ─────────────────────────────────────────────
#
# A VST3 on Linux is a *directory* bundle: Foo.vst3/Contents/x86_64-linux/*.so.
# Different packagers put it under different prefixes inside a .deb payload
# (usr/lib/vst3, usr/lib/lxvst3, usr/lib/x86_64-linux-gnu/vst3, ...).
# Rather than guess, search the extracted tree for any *.vst3 directory
# and move it to the front of $dest.

_find_and_flatten_vst3() {
    _fv_dest="$1"
    _fv_name="$2"

    [ -d "$_fv_dest/$_fv_name" ] && return 0

    _fv_found=$(find "$_fv_dest" -maxdepth 6 -type d -name '*.vst3' -print 2>/dev/null | head -n1)

    if [ -n "$_fv_found" ] && [ "$_fv_found" != "$_fv_dest/$_fv_name" ]; then
        mv "$_fv_found" "$_fv_dest/$_fv_name" 2>/dev/null || return 1
    fi

    rm -rf "$_fv_dest/usr" 2>/dev/null || true

    [ -d "$_fv_dest/$_fv_name" ]
}

# ── plugin installers ─────────────────────────────────────────────────
#
# Each _install_* is idempotent: it checks for the "provides" path first
# and returns early if it's already there.  Every call to _extract_* is
# written as `_extract_X "$archive" "$dest"` — archive first, dest second.

_install_surge_xt() {
    dest="$PLUGINS_DIR/surge-xt-linux-1.3.4-pluginsonly"
    provides="$dest/Surge XT.vst3"
    [ -e "$provides" ] && { echo "  ok      Surge XT"; return 0; }

    _scratch_init
    archive="$_scratch/surge-xt.deb"

    url="https://ftp4.se.freebsd.org/pub/opensuse/repositories/home:/surge-synth-team/Debian_13/amd64/surge-xt-release-vst3_1.3.4-1_amd64.deb"
    _download "$url" "$archive" || return 1
    _extract_deb "$archive" "$dest" || return 1
    _find_and_flatten_vst3 "$dest" "Surge XT.vst3"

    [ -e "$provides" ] || { echo "    WARN: $provides still missing" >&2; return 1; }
    echo "  done    Surge XT"
}

_install_tal_noisemaker() {
    dest="$PLUGINS_DIR/TAL-NoiseMaker"
    provides="$dest/TAL-NoiseMaker.vst3"
    [ -e "$provides" ] && { echo "  ok      TAL-NoiseMaker"; return 0; }

    _scratch_init
    archive="$_scratch/tal-noisemaker.zip"

    url="https://tal-software.com/downloads/TAL-NoiseMaker-5.0.6.zip"
    _download "$url" "$archive" || return 1
    _extract_zip "$archive" "$dest" || return 1
    _find_and_flatten_vst3 "$dest" "TAL-NoiseMaker.vst3"

    [ -e "$provides" ] || { echo "    WARN: $provides still missing" >&2; return 1; }
    echo "  done    TAL-NoiseMaker"
}

_install_vital() {
    dest="$PLUGINS_DIR/VitalInstaller"
    provides="$dest/Vital.vst3"
    [ -e "$provides" ] && { echo "  ok      Vital"; return 0; }

    _scratch_init
    archive="$_scratch/vital.zip"

    url="https://builds.vital.audio/VitalAudio/vital/1_5_5/VitalInstaller.zip"
    _download "$url" "$archive" || return 1
    _extract_zip "$archive" "$dest" || return 1

    if ! _find_and_flatten_vst3 "$dest" "Vital.vst3"; then
        inner_deb=$(find "$dest" -maxdepth 2 -name '*.deb' -print -quit 2>/dev/null || true)
        inner_appimage=$(find "$dest" -maxdepth 2 -name '*.AppImage' -print -quit 2>/dev/null || true)

        if [ -n "$inner_deb" ]; then
            _extract_deb "$inner_deb" "$dest" || true
            _find_and_flatten_vst3 "$dest" "Vital.vst3" || true
        elif [ -n "$inner_appimage" ]; then
            chmod +x "$inner_appimage" 2>/dev/null || true
            ( cd "$dest" && "$inner_appimage" --appimage-extract >/dev/null 2>&1 || true )
            if [ -d "$dest/squashfs-root" ]; then
                _fv_inner=$(find "$dest/squashfs-root" -maxdepth 5 -type d -name 'Vital.vst3' -print -quit 2>/dev/null || true)
                [ -n "$_fv_inner" ] && mv "$_fv_inner" "$provides" 2>/dev/null || true
            fi
        fi
    fi

    [ -e "$provides" ] || {
        echo "    WARN: Vital.vst3 still missing — inspect $dest for the layout" >&2
        return 1
    }
    echo "  done    Vital"
}

_install_mt_powerdrumkit() {
    dest="$PLUGINS_DIR/MT-PowerDrumKit"
    provides="$dest/MT-PowerDrumKit.vst3"
    [ -e "$provides" ] && { echo "  ok      MT-PowerDrumKit"; return 0; }

    _scratch_init
    archive="$_scratch/mt-powerdrumkit.zip"

    url="https://cdn1.resources.manda-audio.com/DOWNLOADS/products/mtpdk2_free/2.1.5/MTPDK-2.1.5.1-VST3-64bit-Linux-FULL.zip"
    _download "$url" "$archive" || return 1
    _extract_zip "$archive" "$dest" || return 1
    _find_and_flatten_vst3 "$dest" "MT-PowerDrumKit.vst3"

    [ -e "$provides" ] || { echo "    WARN: $provides still missing" >&2; return 1; }
    echo "  done    MT-PowerDrumKit"
}

_install_magical8bitplug2() {
    dest="$PLUGINS_DIR/Magical8bitPlug2"
    provides="$dest/Magical8bitPlug2.vst3"
    [ -e "$provides" ] && { echo "  ok      Magical 8bit Plug 2"; return 0; }

    mkdir -p "$dest"

    if ! command -v git >/dev/null 2>&1; then
        echo "    WARN: git not installed; cannot fetch Magical 8bit Plug 2" >&2
        echo "          download manually from: https://codeberg.org/p1r0j/Magical8bitPlug2ForLinux" >&2
        return 1
    fi

    _scratch_init
    clone="$_scratch/m8bp2"

    for branch in linux-fix main master; do
        printf '    CLONE %s (%s)\n' "codeberg.org/p1r0j/Magical8bitPlug2ForLinux" "$branch"
        if git clone --depth 1 --branch "$branch" \
            https://codeberg.org/p1r0j/Magical8bitPlug2ForLinux.git "$clone" 2>/dev/null; then

            tarball=""
            for cand in \
                "$clone/linux_binaries/magical8bitplug2.vst3.tar.gz" \
                "$clone/linux_binaries/magical8bitplug2.tar.gz"
            do
                [ -f "$cand" ] && { tarball="$cand"; break; }
            done

            if [ -n "$tarball" ]; then
                _extract_tar "$tarball" "$dest" || true

                if _find_and_flatten_vst3 "$dest" "Magical8bitPlug2.vst3"; then
                    rm -rf "$clone"
                    echo "  done    Magical 8bit Plug 2"
                    return 0
                fi
            fi

            rm -rf "$clone"
        fi
    done

    echo "    WARN: could not fetch Magical8bitPlug2.vst3 automatically" >&2
    echo "          download manually from: https://codeberg.org/p1r0j/Magical8bitPlug2ForLinux" >&2
    echo "          and place the .vst3 bundle in $dest" >&2
    return 1
}

_install_dexed() {
    dest="$PLUGINS_DIR/Dexed-1.0.1-lnx"
    provides="$dest/Dexed.vst3"
    [ -e "$provides" ] && { echo "  ok      Dexed"; return 0; }

    _scratch_init
    archive="$_scratch/dexed.zip"

    url="https://github.com/asb2m10/dexed/releases/download/v1.0.1/Dexed-1.0.1-lnx.zip"
    _download "$url" "$archive" || return 1
    _extract_zip "$archive" "$dest" || return 1
    _find_and_flatten_vst3 "$dest" "Dexed.vst3"

    [ -e "$provides" ] || { echo "    WARN: $provides still missing" >&2; return 1; }
    echo "  done    Dexed"
}

_install_obxd() {
    dest="$PLUGINS_DIR/OB-Xd-2.20"
    provides="$dest/OB-Xd.vst3"
    [ -e "$provides" ] && { echo "  ok      OB-Xd"; return 0; }

    _scratch_init
    archive="$_scratch/obxd.deb"

    url="https://github.com/reales/OB-Xd/releases/download/v2.20/Obxd220.deb"
    _download "$url" "$archive" || return 1
    _extract_deb "$archive" "$dest" || return 1

    if ! _find_and_flatten_vst3 "$dest" "OB-Xd.vst3"; then
        echo "    WARN: no .vst3 bundle found under $dest" >&2
        echo "          tree contains:" >&2
        find "$dest" -maxdepth 5 -print 2>/dev/null | sed 's/^/            /' >&2
        return 1
    fi

    [ -e "$provides" ] || { echo "    WARN: $provides still missing" >&2; return 1; }
    echo "  done    OB-Xd"
}

# ── soundfont acquisition ─────────────────────────────────────────────
#
# Fetches .sf2 files from Musical Artifacts by search term.
# API docs: https://github.com/lfzawacki/musical-artifacts/wiki
#
# Requires: curl, jq
#
# Downloads go to $SOUNDFONTS_DIR (default: <repo>/soundfonts).
#
# Musical Artifacts sits behind Cloudflare, which fingerprints the TLS
# handshake.  Plain curl gets a 403 regardless of headers.  This section
# tries, in order:
#
#   1. curl-impersonate-ff / curl-impersonate-chrome — patched curl with
#      Firefox's and Chrome's exact TLS fingerprints.  This is the real
#      fix.  Install with:
#          sudo dnf copr enable bonesmoses/curl-impersonate
#          sudo dnf install curl-impersonate
#   2. Plain curl with a browser User-Agent + Referer + Sec-Fetch-*
#      headers.  Occasionally works if Cloudflare is in a lenient mood.
#   3. wget, which has a different (but still not browser) fingerprint.
#
# If all three fail, the artifact has to be downloaded by hand.  The
# warning printed on failure says so.

_SF_UA="Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0"

# Pick the best available curl variant for anti-Cloudflare work.
_sf_curl() {
    for c in curl-impersonate-ff curl-impersonate-chrome curl; do
        if command -v "$c" >/dev/null 2>&1; then
            echo "$c"
            return
        fi
    done
    echo curl
}

# Search Musical Artifacts and return the download URL of the first
# .sf2 result.  Empty string if nothing found.
_sf2_search_url() {
    _sf_query="$1"

    if ! command -v jq >/dev/null 2>&1; then
        echo "    jq not found — install jq for soundfont search" >&2
        return 1
    fi

    _sf_encoded=$(printf '%s' "$_sf_query" | jq -sRr @uri)
    "$(_sf_curl)" -fsSL --retry 3 -A "$_SF_UA" \
        "https://musical-artifacts.com/artifacts.json?q=${_sf_encoded}&formats=sf2" \
        2>/dev/null \
        | jq -r '.[0].file // empty' 2>/dev/null
}

# Download one file with the best available method.
# $1 = artifact page URL (Referer), $2 = file URL, $3 = local path.
_sf_download_file() {
    _sdf_page="$1"; _sdf_url="$2"; _sdf_dest="$3"

    _sdf_curl=$(_sf_curl)
    _sdf_jar=$(mktemp)

    # Prime the session: visit the artifact page like a browser.
    "$_sdf_curl" -fsSL --http2 -A "$_SF_UA" \
        -H "Accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8" \
        -H "Accept-Language: en-US,en;q=0.5" \
        -H "Sec-Fetch-Dest: document" \
        -H "Sec-Fetch-Mode: navigate" \
        -H "Sec-Fetch-Site: same-origin" \
        -c "$_sdf_jar" -b "$_sdf_jar" \
        "$_sdf_page" > /dev/null 2>&1 || true

    printf '    GET %s\n' "$_sdf_url"

    # Attempt 1: best curl with browser headers.
    if "$_sdf_curl" -fL --progress-bar --retry 3 --http2 \
        -A "$_SF_UA" \
        -H "Referer: $_sdf_page" \
        -H "Accept: */*" \
        -H "Accept-Language: en-US,en;q=0.5" \
        -H "Sec-Fetch-Dest: empty" \
        -H "Sec-Fetch-Mode: no-cors" \
        -H "Sec-Fetch-Site: same-origin" \
        -c "$_sdf_jar" -b "$_sdf_jar" \
        "$_sdf_url" -o "$_sdf_dest"; then
        rm -f "$_sdf_jar"
        return 0
    fi

    # Attempt 2: wget with the same cookie jar and Referer.
    if command -v wget >/dev/null 2>&1; then
        printf '    RETRY with wget\n'
        if wget -q --show-progress \
            --user-agent="$_SF_UA" \
            --referer="$_sdf_page" \
            --load-cookies="$_sdf_jar" \
            -O "$_sdf_dest" "$_sdf_url"; then
            rm -f "$_sdf_jar"
            return 0
        fi
    fi

    rm -f "$_sdf_jar"
    return 1
}

_fetch_soundfont() {
    _sf_term="$1"
    _sf_name="${2:-}"

    mkdir -p "$SOUNDFONTS_DIR"

    printf '  sf2     searching "%s"\n' "$_sf_term"

    _sf_url=$(_sf2_search_url "$_sf_term")
    if [ -z "$_sf_url" ] || [ "$_sf_url" = "null" ]; then
        printf '    no .sf2 match for "%s"\n' "$_sf_term" >&2
        return 1
    fi

    if [ -n "$_sf_name" ]; then
        _sf_dest="$SOUNDFONTS_DIR/$_sf_name"
    else
        _sf_base=$(basename "$_sf_url")
        _sf_dest="$SOUNDFONTS_DIR/$_sf_base"
    fi

    if [ -e "$_sf_dest" ]; then
        printf '  ok      %s\n' "$(basename "$_sf_dest")"
        return 0
    fi

    _sf_id=$(printf '%s' "$_sf_url" | sed -n 's|.*/artifacts/\([0-9][0-9]*\)/.*|\1|p')
    if [ -z "$_sf_id" ]; then
        printf '    could not parse artifact id from %s\n' "$_sf_url" >&2
        return 1
    fi
    _sf_page="https://musical-artifacts.com/artifacts/${_sf_id}"

    if _sf_download_file "$_sf_page" "$_sf_url" "$_sf_dest"; then
        case "$_sf_dest" in
            *.zip)
                _extract_zip "$_sf_dest" "$SOUNDFONTS_DIR" && rm -f "$_sf_dest"
                ;;
        esac
        printf '  done    %s\n' "$(basename "$_sf_dest")"
        return 0
    else
        printf '    WARN: download failed for %s\n' "$_sf_term" >&2
        printf '          Cloudflare rejected the request.  Install curl-impersonate:\n' >&2
        printf '              sudo dnf copr enable bonesmoses/curl-impersonate\n' >&2
        printf '              sudo dnf install curl-impersonate\n' >&2
        printf '          or download by hand from the artifact page.\n' >&2
        rm -f "$_sf_dest"
        return 1
    fi
}

# Deterministic fetch by artifact ID.  Use when you know which artifact
# you want and don't trust search ranking.
_fetch_soundfont_by_id() {
    _sf_id="$1"
    _sf_name="$2"

    if ! command -v jq >/dev/null 2>&1; then
        echo "    jq not found — install jq" >&2
        return 1
    fi

    mkdir -p "$SOUNDFONTS_DIR"

    _sf_url=$("$(_sf_curl)" -fsSL --retry 3 -A "$_SF_UA" \
        "https://musical-artifacts.com/artifacts/${_sf_id}.json" \
        | jq -r '.file // empty' 2>/dev/null)

    if [ -z "$_sf_url" ] || [ "$_sf_url" = "null" ]; then
        echo "    no such artifact: $_sf_id" >&2
        return 1
    fi

    _sf_dest="$SOUNDFONTS_DIR/$_sf_name"
    [ -e "$_sf_dest" ] && { printf '  ok      %s\n' "$_sf_name"; return 0; }

    _sf_page="https://musical-artifacts.com/artifacts/${_sf_id}"
    if _sf_download_file "$_sf_page" "$_sf_url" "$_sf_dest"; then
        printf '  done    %s\n' "$_sf_name"
    else
        printf '    WARN: download failed for artifact %s\n' "$_sf_id" >&2
        rm -f "$_sf_dest"
        return 1
    fi
}

# Clone a curated GitHub soundfont collection.  ~50 MB; idempotent.
_fetch_github_soundfonts() {
    _gh_dest="$SOUNDFONTS_DIR/github-collection"
    [ -d "$_gh_dest/.git" ] && { echo "  ok      github-collection"; return 0; }

    if ! command -v git >/dev/null 2>&1; then
        echo "    WARN: git not installed; skipping GitHub soundfont collection" >&2
        return 1
    fi

    printf '    CLONE bratpeki/soundfonts\n'
    if git clone --depth 1 \
        https://github.com/bratpeki/soundfonts.git "$_gh_dest" 2>/dev/null; then
        echo "  done    github-collection"
        return 0
    fi
    echo "    WARN: clone failed" >&2
    return 1
}

# ── manual-only notices ───────────────────────────────────────────────
# These can't be fetched without an account or interactive installer.

_manual_labs() {
    echo "  manual  LABS — https://legacylabs.spitfireaudio.com/download"
}
_manual_addictive_keys() {
    echo "  manual  Addictive Keys — https://www.xlnaudio.com/addictivekeys"
}
_manual_ample_bass() {
    echo "  manual  Ample Bass P Lite II — https://www.amplesound.net/en/pro-pd.asp?id=19"
}
_manual_sforzando() {
    echo "  manual  sforzando — https://www.plogue.com/downloads.html"
}

# ── main ──────────────────────────────────────────────────────────────
# Run the full install only when invoked directly.  When sourced, the
# caller decides which installer to call.  Each call is wrapped so one
# failure doesn't abort the rest (set -e would otherwise kill us).

if [ "$(basename "$0")" = "acquire.sh" ]; then
    echo "Acquiring plugins into $PLUGINS_DIR"
    echo

    failed=0

    _install_surge_xt         || failed=$((failed+1))
    _install_tal_noisemaker   || failed=$((failed+1))
    _install_vital            || failed=$((failed+1))
    _install_mt_powerdrumkit  || failed=$((failed+1))
    _install_magical8bitplug2 || failed=$((failed+1))
    _install_dexed            || failed=$((failed+1))
    _install_obxd             || failed=$((failed+1))

    echo
    echo "Soundfonts:"
    _fetch_github_soundfonts  || failed=$((failed+1))

    # Targeted soundfonts.  Search is fuzzy; the first matching .sf2 wins.
    # If a term returns nothing, try a shorter word or use
    # _fetch_soundfont_by_id with the numeric id from the site URL.
    _fetch_soundfont "kirby"              "kirby.sf2"             || true
    _fetch_soundfont "Super Mario World"  "smw.sf2"               || true
    _fetch_soundfont "Green Hill Zone"    "sonic-green-hill.sf2"  || true
    _fetch_soundfont "earthbound"         "earthbound.sf2"        || true
    _fetch_soundfont "chrono trigger"     "chronotrigger.sf2"     || true

    echo
    echo "Manual downloads:"
    _manual_labs
    _manual_addictive_keys
    _manual_ample_bass
    _manual_sforzando

    echo
    if [ "$failed" -gt 0 ]; then
        echo "Done with $failed failure(s).  Run 'sh -x ./acquire.sh' to trace."
        exit 1
    fi
    echo "Done.  Run 'trackscript-player check' to verify."
fi