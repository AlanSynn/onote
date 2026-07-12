// onote Manual — Catppuccin Latte print theme (HEADER rules).
// Two-step pipeline (see scripts/build-pdf.sh): pandoc converts the Markdown to
// Typst body markup, then this file is concatenated in FRONT of that body and
// compiled. Keeping pandoc and Typst decoupled (no pandoc `$body$` placeholders)
// avoids template-substitution pitfalls and lets these show rules drive all
// styling. Fonts use Typst's bundled "New Computer Modern" (always present in
// the typst CLI image) so CI never hits a missing-font failure.

#let accent  = rgb("#7287fd") // lavender (app accent)
#let fg      = rgb("#4c4f69") // latte text
#let muted   = rgb("#6c6f85") // latte subtext0
#let surface = rgb("#e6e9ef") // latte mantle
#let border  = rgb("#bcc0cc") // latte surface1
#let linkb   = rgb("#1e66f5") // latte blue

// pandoc's typst writer emits `#horizontalrule` for thematic breaks (---); its
// default template defines this, so we must too when supplying our own rules.
#let horizontalrule = align(center)[#line(length: 40%, stroke: 0.6pt + border)]

#set document(title: "onote Manual")
#set page(
  paper: "a4",
  margin: (top: 2cm, bottom: 2cm, inside: 1.8cm, outside: 1.8cm),
  numbering: "1",
)
#set text(font: "New Computer Modern", size: 10.5pt, fill: fg, lang: "en")
#set par(leading: 0.75em, spacing: 0.9em)
#set heading(numbering: "1.1.")

#show heading.where(level: 1): it => block(spacing: 1.4em)[
  #text(weight: "bold", size: 19pt, fill: accent)[#it.body]
  #v(-0.5em)
  #line(length: 100%, stroke: 0.6pt + border)
]
#show heading.where(level: 2): it => block(spacing: 1em)[
  #text(weight: "bold", size: 14pt, fill: fg)[#it.body]
]
#show heading.where(level: 3): it => text(weight: "bold", size: 11.5pt, fill: muted)[#it.body]

// Code blocks echo the site's TerminalPane: lavender left-accent bar on mantle.
#show raw.where(block: true): it => block(
  width: 100%,
  fill: surface,
  stroke: (left: 2.5pt + accent),
  inset: 9pt,
  radius: 4pt,
)[#it]
#show raw.where(block: false): it => box(
  fill: surface,
  inset: (x: 3pt, y: 1pt),
  radius: 2pt,
)[#it]

#show link: it => text(fill: linkb)[#it]

// Cover + table of contents (the pandoc-converted body is appended after this).
#align(center)[
  #text(weight: "bold", size: 26pt, fill: accent)[onote Manual]
  #v(0.4em)
  #text(size: 11pt, fill: muted)[
    A single-Rust-binary, terminal-native Markdown vault client.
  ]
]
#v(1em)
#outline(title: "Contents", indent: 1em, depth: 2)
#pagebreak()

// ── pandoc-converted body is appended here by build-pdf.sh ──
