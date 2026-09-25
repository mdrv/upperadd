# Wayland overlay patterns — verified on the mdrv-gpui tree

Findings from upperadd M1 + M1.5 (2026-09-25), fork at
`mdrv-gpui-0.0.260925.1`. These supersede the "doc-era" caveats in
`/x/m/v270/gpui-ce/gpui-ce.md` §16.

## `Window::set_visible` works on layer surfaces (verified)

Hide/show via `set_visible(false/true)` + `set_keyboard_interactivity`
(None ↔ Exclusive) is solid on Hyprland 0.5x with one persistent
layer-shell window created with `show: false`:

- unmap/remap round-trips cleanly; no flicker or stale-frame artifacts
  observed across dozens of toggles;
- keyboard re-take on remap works — Esc is delivered again after re-show
  (with `window.focus(&focus_handle, cx)` re-applied on show);
- entity state (search text, selection, scroll) survives hide/show for
  free since the window is never destroyed.

The §16.2 fallback (empty render + `set_input_region(Some(&[]))`) is
**not needed**; kept here as the escape hatch if a compositor misbehaves.

## Drag via runtime `set_margin` (fork patch, tag 0.0.260925.1)

`Anchor::TOP | Anchor::LEFT` + `margin = position` + `Window::set_margin`
(commits immediately, like the other runtime setters). Final form after
three failed event-driven attempts: **poll-based ground-truth drag**.
Event-driven drag cannot be made smooth:

- Event positions are **surface-local**, and the surface moves under the
  cursor as each margin lands (origin(k) == pos(k)) — start+delta lags the
  cursor, and per-event accumulation feeds unapplied deltas back into the
  origin estimate (events ~1000 Hz, margin commits ~frame rate) → wiggle.
- Fast flings exit the surface, pointer events stop (no pointer grab on
  layer surfaces) → the drag stalls.

The working pattern (implemented in `src/sticky.rs`):

1. At press: `offset = −press_local` (surface origin == pos at press, so
   `cursor_global = pos + press_local`; keep `pos = cursor_global + offset`
   for the whole drag).
2. Poll the global cursor every ~16 ms while dragging (`hyprctl cursorpos`;
   global coords → subtract the output origin from
   `PlatformDisplay::bounds().origin` to get margin space), compute the
   target, `cx.notify()` only on change.
3. Frame-gate `set_margin` in `render()` — at most one call per frame, and
   only when the polled position actually moved.
4. Pointer-leave from the same ground truth: cursor inside the surface rect
   = still dragging; outside = 250 ms grace, then end. Surface events are
   safety nets only (cancel when a move arrives with no button pressed).

Result: 1:1 tracking, no wiggle, fling-proof. Verified via synthetic input
(ydotool) + `hyprctl layers -j` position readback.

Also true: `MouseMoveEvent::pressed_button` is `Option<MouseButton>` in
this fork — guard with `!= Some(MouseButton::Left)`. Anchor `TOP | LEFT`,
never unanchored (compositors center unanchored layer surfaces and their
margins don't behave). Explicit `window_bounds` size is correct here
(§16.3's 0×0 rule is the all-four-anchors case).

## Cursor output resolution (no gpui cursor API)

`hyprctl cursorpos` → global (x, y); `hyprctl monitors -j` → find the
monitor whose `x/y/width/height` box contains it; match that origin
against `PlatformDisplay::bounds().origin` (±1.0 px) → `DisplayId` for
`WindowOptions.display_id`. Works; keep a primary-display fallback for
non-Hyprland sessions.

## Resizing layer surfaces + cursor rules (tag 0.0.260925.4)

- `Window::resize` stages `layer_surface.set_size`; with `set_margin` staging
  too (tag .2), the present commit carries margin + size + buffer atomically.
  Never rely on a committing setter mid-gesture: a commit that pairs a new
  size with the old buffer gets scaled by the compositor (border smear).
- Tag .4 applies the drawable resize synchronously and fires the gpui-core
  callback from a spawn — the callback re-enters the App via
  `AsyncApp::update`, which DEADLOCKS if called mid-update. Sync state,
  deferred callbacks.
- Cursor styles resolve per frame; a `set_window_cursor_style` request
  (hitbox-less) overrides all hover styles for its frame — push it from
  `render()` while a gesture is active for a stable drag/resize cursor.
- Remaining cosmetic: the compositor applies a resize one frame behind the
  buffer → transient cut at the leading edge during fast resizes. End state
  exact. Full fix = ack_configure dance in the fork (deferred; not worth it
  for v1).
