#!/bin/sh
set -e

# Cloudflare Pages build script
# Assembles the static landing page + Zola docs into a single output dir.

ZOLA_VERSION="${ZOLA_VERSION:-0.19.2}"

if ! command -v zola >/dev/null 2>&1; then
  echo "Installing zola ${ZOLA_VERSION}..."
  mkdir -p .bin
  curl -sL "https://github.com/getzola/zola/releases/download/v${ZOLA_VERSION}/zola-v${ZOLA_VERSION}-x86_64-unknown-linux-gnu.tar.gz" | tar xz -C .bin
  export PATH="$PWD/.bin:$PATH"
fi

echo "Using $(zola --version)"

OUT="_build"
rm -rf "$OUT"
mkdir -p "$OUT"

# 1. Copy static landing page files
for asset in \
  index.html \
  asciinema-player.css \
  asciinema-player.min.js \
  doom.mp4 \
  doom-av1.mp4 \
  ../install.sh \
  ../install.ps1 \
  favicon.ico \
  favicon-16x16.png \
  favicon-32x32.png \
  apple-touch-icon.png \
  android-chrome-192x192.png \
  android-chrome-512x512.png \
  site.webmanifest; do
  if [ ! -f "$asset" ]; then
    echo "error: site asset '$asset' is missing or unreadable" >&2
    exit 1
  fi
  cp "$asset" "$OUT/"
done

# 2. Build Zola docs
cd docs
zola build -o "../_build/docs"
cd ..

# 3. Markdown mirrors + llms.txt / llms-full.txt for LLM consumption
BASE_URL="https://github.com/w0wl0lxd/n00n"

body() {
  awk '/^\+\+\+$/{c++; next} c>=2' "$1"
}

first_paragraph() {
  body "$1" | awk '
    /^```/ { fence = !fence; next }
    fence || /^#/ { next }
    /^$/ { if (p) exit; next }
    { printf "%s%s", (p ? " " : ""), $0; p = 1 }
    END { print "" }
  '
}

pages=$(for f in docs/content/*/_index.md; do
  w=$(sed -n 's/^weight = \([0-9]*\)$/\1/p' "$f")
  echo "${w:-999} $f"
done | sort -n | cut -d' ' -f2-)

body docs/content/_index.md >"$OUT/docs/index.md"

summary=$(first_paragraph docs/content/_index.md)

{
  echo "# n00n"
  echo
  echo "> $summary"
  echo
  echo "Full documentation in one file: $BASE_URL/llms-full.txt"
  echo
  echo "## Docs"
  echo
  echo "- [n00n Docs]($BASE_URL/docs/index.md): overview and map of the documentation"
  for f in $pages; do
    slug=$(basename "$(dirname "$f")")
    title=$(sed -n 's/^title = "\(.*\)"$/\1/p' "$f")
    desc=$(first_paragraph "$f")
    echo "- [$title]($BASE_URL/docs/$slug/index.md): $desc"
  done
} >"$OUT/llms.txt"

{
  body docs/content/_index.md
  for f in $pages; do
    slug=$(basename "$(dirname "$f")")
    body "$f" >"$OUT/docs/$slug/index.md"
    echo
    echo "---"
    echo
    body "$f"
  done
} >"$OUT/llms-full.txt"

cp "$OUT/llms.txt" "$OUT/llms-full.txt" "$OUT/docs/"
