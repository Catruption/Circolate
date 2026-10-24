#!/bin/sh
# Check which soundfonts your FLPs need are already on disk.
#
#   ./find_soundfonts.sh sf2_needed.txt [dir ...]
#
# With no dirs it searches ./soundfonts, ~/soundfonts, ~/Music, ~/Downloads
# and the Circolate music/ folder, three levels deep or more (find is
# unbounded, so point it at specific folders for speed).
#
# Names are compared the way the converter does it: lowercase letters and
# digits only, extension dropped, so "FNF Boyfriend.sf2" matches
# "fnf_boyfriend.SF2".  Prints one line per needed file:
#
#   FOUND      name  ->  /path/to/file.sf2
#   CANDIDATE  name  ~   /path/that/looks/similar.sf2
#   MISSING    name      https://musical-artifacts.com/... (search link)
#
# and writes the missing names to sf2_missing.txt.

list="${1:?usage: $0 sf2_needed.txt [dir ...]}"
shift
here=$(cd "$(dirname "$0")" && pwd)

if [ $# -gt 0 ]; then
    dirs="$*"
else
    dirs="$here/soundfonts $here/share/soundfonts $here/music $HOME/soundfonts $HOME/Music $HOME/Downloads"
fi

norm() {
    # file name -> lowercase alphanumerics of the stem
    printf '%s' "$1" | sed 's/\.[^.]*$//' | tr 'A-Z' 'a-z' | tr -cd 'a-z0-9'
}

index=$(mktemp)
trap 'rm -f "$index"' EXIT

# index: normalized-stem <TAB> path
for d in $dirs; do
    [ -d "$d" ] || continue
    find "$d" -type f \( -iname '*.sf2' \) 2>/dev/null |
    while IFS= read -r p; do
        printf '%s\t%s\n' "$(norm "$(basename "$p")")" "$p"
    done
done > "$index"

echo "indexed $(wc -l < "$index") soundfont file(s) under: $dirs"
echo

: > sf2_missing.txt
found=0; cand=0; missing=0

while IFS= read -r name; do
    [ -n "$name" ] || continue
    key=$(norm "$name")
    [ "${#key}" -ge 4 ] || continue

    hit=$(awk -F'\t' -v k="$key" '$1 == k { print $2; exit }' "$index")
    if [ -n "$hit" ]; then
        printf 'FOUND      %s  ->  %s\n' "$name" "$hit"
        found=$((found + 1))
        continue
    fi

    # Looser: one name contains the other.
    near=$(awk -F'\t' -v k="$key" \
        'length($1) >= 4 && (index($1, k) || index(k, $1)) { print $2; exit }' "$index")
    if [ -n "$near" ]; then
        printf 'CANDIDATE  %s  ~   %s\n' "$name" "$near"
        cand=$((cand + 1))
        continue
    fi

    q=$(printf '%s' "$name" | sed 's/\.[^.]*$//; s/[_ ]\+/ /g; s/ /%20/g')
    printf 'MISSING    %s      https://musical-artifacts.com/artifacts?formats=sf2&q=%s\n' "$name" "$q"
    printf '%s\n' "$name" >> sf2_missing.txt
    missing=$((missing + 1))
done < "$list"

echo
echo "found $found, candidates $cand, missing $missing  (missing names in sf2_missing.txt)"
