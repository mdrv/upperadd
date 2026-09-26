# 02 — Backlog & deferred ideas

Features brainstormed/deferred during M3. Nothing here is committed scope
for v0.1.0 — items graduate into numbered specs only by owner decision.

## Preview rendering

- **Code-block syntax highlighting (syntect)** — map the fenced language to
  a syntect syntax set, emit `StyledText` highlight spans per line. Dep is
  heavy (~2s clean builds, big binary); measure before adopting. Fallback
  styling (monospace panel) already shipped.
- **Tables** — enable pulldown-cmark `ENABLE_TABLES`, render as a grid of
  `div()` rows (no table element in gpui). Column alignment via fixed
  flex-basis from max content width, or accept uneven columns for v1.
- **Remote images** — current `render_image` handles local files only
  (relative → note dir, per AGENTS.md rule 5). Remote http(s) URLs need an
  async fetch + byte cache → `RenderImage`; consider a per-note LRU.
- **SVG images** — gpui `img()` renders raster; SVG needs usvg rasterize
  step before handing bytes to `RenderImage`.
- **Image interactions** — click → open file in editor, wheel-zoom inside
  the pane, captions already render from alt text.

## Selection & clipboard

~~Shared selectable-text primitive~~ **Shipped in v0.1.1** — block-level
selection (`selection.rs` PaneSel) for sticky body + overlay preview;
whole blocks are the unit (no glyph hit-testing yet). Mouse-up auto-copy
to primary AND clipboard, "copied" chips (timed show/hide only — no
opacity fade, §28). Overlay preview additionally copies on Ctrl+C.
Remaining here: glyph-level (sub-block) selection via
`index_for_position` hit-testing + highlight runs across line breaks.

## Stickies

- ~~**Auto-refresh on fs change**~~ **Shipped in v0.1.1** — stickies
  subscribe to index-worker broadcasts (`IndexMsg::{Changed,Removed,
  Reindexed}`); refresh + out-of-sync badge automatic, ↻ still forces.
- **Persistence** — stickies die with the daemon by design (spec 01);
  revisit only if the owner wants session restore.
- **Per-sticky zoom** — ctrl+wheel to scale body text; store per key.
- **Sticky from search context** — pin should probably preserve the query
  highlight (scroll-to-match) inside the sticky body.

## Search & results

- **Match snippets** — worker returns the best matching line per section
  (LIKE hit or fuzzy title region); render under the title, preview scrolls
  to first match.
- **Mouse in results** — click row = select, double-click = open editor,
  right-click = pin. Currently keyboard-only.
- **fzf-style OR terms** — space-separated terms AND-ed, `|` OR-ed; scorer
  already normalized per-term, just needs multi-term plumbing.

## Editor handoff (shipped minimal)

- Enter opens `cfg.editor` at `line+1` via args template with
  `process_group(0)` + `KillMode=process` (editor survives `ua stop`).
- Open items: returning focus to the overlay after save (currently manual
  `ua toggle`), neovide can't scroll to a _section_ (line is approximate
  after edits), and a `--wait` variant could ping the daemon on save for
  instant preview refresh (fs-watch already covers it, ~250 ms later).

## Infrastructure

- **mdrv-gpui-ce 0.0.260925.x tags**: upperadd pins `.4` (set_margin
  stage-only + synchronous wayland resize + deferred callback). Bump
  procedure: edit both `tag =` lines in Cargo.toml, rebuild.
- **Status line truncation** — hints line grows; wrap or drop items on
  narrow panels.
