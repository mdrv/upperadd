# 01 — Sticky notes (pinned previews)

Decided 2026-09-25, right after M1 (grilling round 3). Landing in two
steps: **M1.5 skeleton** now (`ua pin-test` — verifies the fork patch,
drag, and close before real content exists), full wiring once selection
(M2) and markdown rendering (M3) exist.

## Feature

With a note selected (overlay in Vim-like normal mode), `P` or `Tab`
extracts it into a separate always-on-top window: read-only rendered
markdown, draggable by its header strip, closed via ✕. Many stickies may
be pinned at once. A sticky never takes the keyboard
(`KeyboardInteractivity::None` forever); its body scrolls by wheel only.

## Decisions

- **Drag = fork patch `Window::set_margin`** (runtime setter + immediate
  commit; tag `mdrv-gpui-0.0.260925.1`; documented in `/g/gpui-ce/MDRV.md`).
  An unanchored `Layer::Top` surface is positioned entirely by its margins
  (CSS order top/right/bottom/left), so dragging = pointer-delta → margin
  updates. Compositors do not move layer surfaces for you; `start_window_move`
  is xdg-only and a normal toplevel cannot stay on top on Wayland.
  Known limitation: pointer events stop when the cursor leaves the surface
  mid-drag (layer surfaces have no pointer grab); the drag resumes when the
  cursor re-enters. Accepted for v1; revisit only if it annoys in practice.
- **Window shape**: `Layer::Top`, `Anchor::empty()`, `exclusive_zone: -1`,
  keyboard None, fixed 420×560 (resize = hand-rolled edge drag, later),
  `app_id`/namespace `upperadd-sticky` (README documents
  `layerrule blur, upperadd-sticky` at M4). Explicit size in
  `window_bounds` is correct here — spec 00 §16.3's "0×0" rule is the
  inverse (all-anchors) case.
- **Header**: note title + ↻ (force re-read from the bound path) + ✕
  (close, `Window::remove_window`).
- **Content**: same GFM pipeline as the M3 preview; **live** — fs-watch
  re-renders on save. If the file is deleted/renamed, the sticky
  **persists** showing the last content plus an out-of-sync badge; ↻
  retries the read; if the path reappears, fs-watch re-syncs and clears
  the badge. No auto-close (owner override of the original proposal).
- **Keys**: `P` (mnemonic) and `Tab` (alias) pin in normal mode. Esc is
  **two-stage**: insert → normal → hide overlay. This amends spec 00's
  "Esc hides" (now "Esc hides _from normal mode_"); lands with M2's
  search input, not before.
- **Lifecycle**: stickies die with the daemon; **no** persistence across
  restarts in v0.1.x. New stickies spawn at (100, 100) + 24 px cascade
  per sticky (index mod 8), on the same output as the overlay window.
- **M1.5 skeleton**: `ua pin-test` spawns a placeholder sticky so the
  fork patch, dragging, and ✕ are verified early — same reasoning as
  testing `set_visible` in M1.
