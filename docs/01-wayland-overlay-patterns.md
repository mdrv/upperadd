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

Unanchored layer surface + `margin = position` + pointer-delta updates:
works, commits land immediately (same pattern as the other runtime
setters — apply then `surface.commit()`).

- Set margins from the **drag start** position + accumulated delta, not
  from incremental deltas (rounding drift compounds).
- **Pointer-leave limitation is real**: Wayland sends `pointer_leave`
  when the cursor exits the surface, so a fast drag stalls until
  re-entry. Acceptable for sticky-note-sized drags; do not build
  fling-drag UIs on this.
- Unanchored + explicit `window_bounds` size is the correct combo
  (§16.3's "pass 0×0" rule applies only when all four anchors are set).
- `MouseMoveEvent::pressed_button` is `Option<MouseButton>` in this fork,
  not a bitflags set — guard with `!= Some(MouseButton::Left)`.

## Cursor output resolution (no gpui cursor API)

`hyprctl cursorpos` → global (x, y); `hyprctl monitors -j` → find the
monitor whose `x/y/width/height` box contains it; match that origin
against `PlatformDisplay::bounds().origin` (±1.0 px) → `DisplayId` for
`WindowOptions.display_id`. Works; keep a primary-display fallback for
non-Hyprland sessions.
