#!/usr/bin/env bash
#
# Aggregate changelog.d/*.md fragments into CHANGELOG.md.
#
# Usage: scripts/build-changelog.sh [VERSION]
# VERSION defaults to the workspace version in Cargo.toml.
#
# Reads every fragment (excluding README.md and files starting with '_'),
# groups them by type under a new "## [VERSION] - DATE" heading, prepends the
# result to CHANGELOG.md, and removes the consumed fragments.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FRAG_DIR="$ROOT/changelog.d"
CHANGELOG="$ROOT/CHANGELOG.md"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(grep -m1 '^version = "' "$ROOT/Cargo.toml" | sed -E 's/version = "([^"]+)"/\1/')"
fi
DATE="$(date +%Y-%m-%d)"

declare -A HEAD=(
  [added]=Added [changed]=Changed [fixed]=Fixed [removed]=Removed
  [deprecated]=Deprecated [security]=Security [performance]=Performance [docs]=Docs
)
ORDER=(added changed fixed removed deprecated security performance docs)

mapfile -t FRAGS < <(find "$FRAG_DIR" -maxdepth 1 -name '*.md' ! -name 'README.md' ! -name '_*' | sort)
if [ ${#FRAGS[@]} -eq 0 ]; then
  echo "no changelog fragments found in $FRAG_DIR" >&2
  exit 1
fi

TMP="$(mktemp -d)"
ENTRY="$TMP/entry.md"
: > "$ENTRY"
echo "## [$VERSION] - $DATE" >> "$ENTRY"
echo >> "$ENTRY"

for t in "${ORDER[@]}"; do : > "$TMP/$t"; done

for f in "${FRAGS[@]}"; do
  base="$(basename "$f" .md)"
  type="${base##*.}"
  if [ -z "${HEAD[$type]:-}" ]; then
    echo "unknown changelog type '$type' in $f (expected: ${ORDER[*]})" >&2
    rm -rf "$TMP"
    exit 1
  fi
  # Strip a single leading/trailing blank line, then append.
  sed -e '1{/^$/d}' -e '${/^$/d}' "$f" >> "$TMP/$type"
done

for t in "${ORDER[@]}"; do
  if [ -s "$TMP/$t" ]; then
    echo "### ${HEAD[$t]}" >> "$ENTRY"
    echo >> "$ENTRY"
    while IFS= read -r line; do
      [ -z "$line" ] && continue
      case "$line" in
        -*|"*") echo "$line" >> "$ENTRY" ;;
        *) echo "- $line" >> "$ENTRY" ;;
      esac
    done < "$TMP/$t"
    echo >> "$ENTRY"
  fi
done

if [ ! -f "$CHANGELOG" ]; then
  { echo "# Changelog"; echo; cat "$ENTRY"; } > "$CHANGELOG"
else
  if grep -q '^## ' "$CHANGELOG"; then
    awk 'FNR==NR{e=e "\n" $0; next} /^## / && !ins{printf "%s\n", e; ins=1} {print}' "$ENTRY" "$CHANGELOG" > "$CHANGELOG.tmp"
  else
    { cat "$CHANGELOG"; echo; cat "$ENTRY"; } > "$CHANGELOG.tmp"
  fi
  mv "$CHANGELOG.tmp" "$CHANGELOG"
fi

for f in "${FRAGS[@]}"; do rm -f "$f"; done
echo "wrote [$VERSION] - $DATE to $CHANGELOG and removed ${#FRAGS[@]} fragment(s)"
rm -rf "$TMP"
