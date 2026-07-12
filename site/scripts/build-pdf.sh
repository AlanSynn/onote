#!/usr/bin/env bash
# build-pdf.sh — render the whole-manual PDF from the same Markdown the web pages
# use (single source of truth). build:downloads already produced the
# sidebar-ordered, frontmatter-stripped concatenation at
# dist/download/onote-manual.md.
#
# TWO-STEP pipeline (decouples pandoc from Typst templating):
#   1. pandoc concat.md → Typst body markup (--to typst)
#   2. concatenate typst/manual.typ (Catppuccin header rules) + body.typ → full
#   3. typst compile → PDF
# This avoids pandoc `$body$`/`$if(toc)$` template substitution pitfalls.
#
# Robustness: missing pandoc/typst → skip. Custom compile fails → retry with
# pandoc's default typst template. A PDF problem never blocks a Pages deploy.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)" # site/
OUT="$DIR/dist/download"
CONCAT="$OUT/onote-manual.md"
PDF="$OUT/onote-manual.pdf"
TEMPLATE="$DIR/typst/manual.typ"
BODY="$OUT/body.typ"
FULL="$OUT/onote-manual.typ"
mkdir -p "$OUT"

# Don't ship the intermediate Typst sources — only the final PDF belongs in dist.
cleanup() { rm -f "$BODY" "$FULL" "$DIR/.pdf.err"; }
trap cleanup EXIT

if [ ! -f "$CONCAT" ]; then
  echo "build-pdf: $CONCAT missing — run 'npm run build:downloads' first" >&2
  exit 1
fi

if ! command -v pandoc >/dev/null 2>&1; then
  echo "build-pdf: pandoc not found — skipping PDF" >&2
  exit 0
fi
if ! command -v typst >/dev/null 2>&1; then
  echo "build-pdf: typst not found — skipping PDF" >&2
  exit 0
fi

# 1. Markdown → Typst body markup.
pandoc "$CONCAT" --from gfm --to typst --output "$BODY"

# 2. Assemble header rules + body.
{
  cat "$TEMPLATE"
  echo
  cat "$BODY"
} > "$FULL"

# 3. Compile with the custom Catppuccin theme.
if typst compile "$FULL" "$PDF" 2>"$DIR/.pdf.err"; then
  rm -f "$DIR/.pdf.err"
  echo "build-pdf: wrote $PDF (Catppuccin template)"
  exit 0
fi

echo "build-pdf: custom template compile failed — retrying with pandoc's default typst template" >&2
cat "$DIR/.pdf.err" >&2 || true
rm -f "$DIR/.pdf.err"

# Fallback: pandoc's field-tested default typst template.
if pandoc "$CONCAT" \
  --from gfm \
  --pdf-engine=typst \
  --toc --toc-depth=2 \
  --metadata title="onote Manual" \
  --metadata author="Alan Synn" \
  --variable mainfont="New Computer Modern" \
  --variable linkcolor="7287fd" \
  -o "$PDF"; then
  echo "build-pdf: wrote $PDF (default template fallback)"
  exit 0
fi

echo "build-pdf: both attempts failed — skipping PDF (not blocking)" >&2
exit 0
