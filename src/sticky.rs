use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Bounds, Context, CursorStyle, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, Size, Window, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions, div, hsla, layer_shell::*, point, prelude::*, px, size,
};
use log::debug;

use crate::config::Config;

const DEFAULT_W: f32 = 420.0;
const DEFAULT_H: f32 = 560.0;
/// Smallest allowed sticky size (edge-drag resize clamps here).
const MIN_W: f32 = 280.0;
const MIN_H: f32 = 180.0;
/// Width/height of the invisible edge hit strips.
const EDGE: f32 = 6.0;
/// First sticky's offset from the output's top-left; each next one cascades.
const BASE_POS: f32 = 100.0;
const CASCADE: f32 = 24.0;
/// Cursor poll rate while moving (window follows cursor, so every tick
/// redraws anyway).
const MOVE_POLL: Duration = Duration::from_millis(16);
/// Cursor poll rate while resizing. Each applied size change can show a
/// one-frame cut on the compositor (it applies the new surface size a frame
/// behind the buffer), so resize steps slower — imperceptible for a drag.
const RESIZE_POLL: Duration = Duration::from_millis(40);
/// A fast fling exits the surface at once; keep following the cursor while
/// it is outside, but end the gesture after this long without re-entry.
const GESTURE_LEAVE_GRACE: Duration = Duration::from_millis(250);
/// While resizing, the cursor rides exactly on the moving edge (the size is
/// derived from it) or pushes past it against the screen edge — count the
/// cursor as "inside" within this band so the leave-grace never fires mid-
/// resize. Move gestures don't need it (the window follows the cursor).
const RESIZE_EDGE_BAND: f32 = 24.0;

/// Which edge a resize drag started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    fn cursor(self) -> CursorStyle {
        match self {
            Edge::Left | Edge::Right => CursorStyle::ResizeLeftRight,
            Edge::Top | Edge::Bottom => CursorStyle::ResizeUpDown,
        }
    }
}

/// A ground-truth pointer gesture: the sticky follows the global cursor, so
/// surface-local origin shifts cannot feed back (no wiggle) and fast flings
/// that exit the surface keep working.
enum Gesture {
    Move {
        /// `pos = global cursor + offset`, constant for the whole drag.
        offset: Point<Pixels>,
        left_at: Option<Instant>,
    },
    Resize {
        edge: Edge,
        /// Position/size at press; edges that must stay put derive their
        /// target from the snapshot, never from accumulated deltas.
        start_pos: Point<Pixels>,
        start_size: Size<Pixels>,
        left_at: Option<Instant>,
    },
}

impl Gesture {
    fn left_at_mut(&mut self) -> &mut Option<Instant> {
        match self {
            Gesture::Move { left_at, .. } | Gesture::Resize { left_at, .. } => left_at,
        }
    }

    fn cursor(&self) -> CursorStyle {
        match self {
            Gesture::Move { .. } => CursorStyle::ClosedHand,
            Gesture::Resize { edge, .. } => edge.cursor(),
        }
    }
}

/// A pinned note: read-only, always on top (`Layer::Top`), never takes the
/// keyboard, draggable by its header, resizable by its edges, closable via ✕.
/// M1.5 skeleton — the body is a placeholder until the M3 markdown pipeline
/// lands (spec 01).
pub struct Sticky {
    title: String,
    /// Surface offset from the output's top-left == (top, left) margins.
    pos: Point<Pixels>,
    /// Surface size (requested via `Window::resize`).
    size: Size<Pixels>,
    /// Position/size actually sent to the compositor (advanced in `render`).
    sent_pos: Point<Pixels>,
    sent_size: Size<Pixels>,
    /// Layout origin of the output this surface lives on (cursorpos is
    /// global, margins are output-local).
    output_origin: Point<Pixels>,
    gesture: Option<Gesture>,
    cfg: Config,
}

/// Open a sticky window. Anchored top+left layer surface: margins position
/// it within the output and `Window::set_margin` (fork patch, tag
/// 0.0.260925.1) moves it at runtime. Explicit size wins here — §16.3's
/// "0×0 bounds" rule is the inverse (all-anchors) case.
pub fn spawn(
    cx: &mut App,
    cfg: &Config,
    title: String,
    index: usize,
    output_origin: Point<Pixels>,
) -> anyhow::Result<()> {
    let offset = (index % 8) as f32 * CASCADE;
    let pos = point(px(BASE_POS + offset), px(BASE_POS + offset));
    let win_size = size(px(DEFAULT_W), px(DEFAULT_H));
    let options = WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        app_id: Some("upperadd-sticky".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: win_size,
        })),
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: "upperadd-sticky".into(),
            layer: Layer::Top,
            anchor: Anchor::TOP | Anchor::LEFT,
            exclusive_zone: Some(px(-1.)),
            margin: Some((pos.y, px(0.), px(0.), pos.x)),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..Default::default()
        }),
        ..Default::default()
    };
    let cfg = cfg.clone();
    cx.open_window(options, |_, cx| {
        cx.new(|cx| Sticky::new(cfg, title, pos, win_size, output_origin, cx))
    })?;
    Ok(())
}

impl Sticky {
    fn new(
        cfg: Config,
        title: String,
        pos: Point<Pixels>,
        size: Size<Pixels>,
        output_origin: Point<Pixels>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title,
            pos,
            size,
            sent_pos: pos,
            sent_size: size,
            output_origin,
            gesture: None,
            cfg,
        }
    }

    /// Begin a pointer gesture and start the ground-truth poll loop.
    fn start_gesture(&mut self, gesture: Gesture, cx: &mut Context<Self>) {
        let poll = match gesture {
            Gesture::Move { .. } => MOVE_POLL,
            Gesture::Resize { .. } => RESIZE_POLL,
        };
        self.gesture = Some(gesture);
        cx.notify(); // pick up the gesture cursor this frame
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(poll).await;
            if !this.update(cx, Sticky::gesture_tick).unwrap_or(false) {
                break;
            }
        })
        .detach();
    }

    fn on_move_down(
        &mut self,
        ev: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        debug!("sticky move start at {:?}", ev.position);
        // At press the surface origin == self.pos, so cursor_global =
        // self.pos + ev.position; with pos = cursor_global + offset that
        // makes offset = −press_local, constant for the whole drag.
        let offset = point(-ev.position.x, -ev.position.y);
        self.start_gesture(Gesture::Move { offset, left_at: None }, cx);
    }

    fn on_edge_down(
        &mut self,
        edge: Edge,
        ev: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Edge strips overlap the header/footer bars; without this the
        // bar's move handler fires after ours and overwrites the gesture.
        cx.stop_propagation();
        debug!("sticky resize start {:?} at {:?}", edge, ev.position);
        self.start_gesture(
            Gesture::Resize {
                edge,
                start_pos: self.pos,
                start_size: self.size,
                left_at: None,
            },
            cx,
        );
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.gesture.is_some() && ev.pressed_button != Some(MouseButton::Left) {
            debug!("sticky gesture cancelled (button up missed)");
            self.gesture = None;
            cx.notify();
        }
    }

    fn on_up(&mut self, _ev: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.gesture.take().is_some() {
            cx.notify(); // release the window-wide gesture cursor
        }
    }

    /// One cursor poll while a gesture is active. Returns false when over.
    fn gesture_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(gesture) = &mut self.gesture else { return false };
        let Some(cursor) = crate::daemon::hypr_cursor_global() else {
            return true; // Hyprland IPC hiccup; keep the gesture alive
        };
        let local = point(
            cursor.x - self.output_origin.x,
            cursor.y - self.output_origin.y,
        );
        // Pointer-leave detection from the same ground truth as the motion
        // (surface events stop when the cursor exits): grace-period the end
        // of the gesture so fast flings that exit the surface still follow.
        let band = match gesture {
            Gesture::Resize { .. } => px(RESIZE_EDGE_BAND),
            Gesture::Move { .. } => px(0.0),
        };
        let inside = local.x >= self.pos.x - band
            && local.x < self.pos.x + self.size.width + band
            && local.y >= self.pos.y - band
            && local.y < self.pos.y + self.size.height + band;
        if inside {
            *gesture.left_at_mut() = None;
        } else {
            let left_at = gesture.left_at_mut().get_or_insert(Instant::now());
            if left_at.elapsed() > GESTURE_LEAVE_GRACE {
                debug!("sticky gesture ended (cursor left the surface)");
                self.gesture = None;
                cx.notify();
                return false;
            }
        }

        // Snap to whole pixels: the wayland layer path truncates to i32.
        let round = |v: Pixels| px(f32::from(v).round());
        match &gesture {
            Gesture::Move { offset, .. } => {
                let target = point(local.x + offset.x, local.y + offset.y);
                if target != self.pos {
                    debug!("sticky poll {:?} -> {:?}", self.pos, target);
                    self.pos = target;
                    cx.notify(); // render applies the margin (frame-gated)
                }
            }
            Gesture::Resize {
                edge,
                start_pos,
                start_size,
                ..
            } => {
                let (mut pos, mut size) = (*start_pos, *start_size);
                match edge {
                    // Edges whose opposite side stays put: the moving edge
                    // chases the cursor, the size comes from the snapshot.
                    Edge::Left => {
                        let right = start_pos.x + start_size.width;
                        pos.x = round(local.x);
                        size.width = round(right - pos.x);
                        if f32::from(size.width) < MIN_W {
                            size.width = px(MIN_W);
                            pos.x = round(right - px(MIN_W));
                        }
                    }
                    Edge::Top => {
                        let bottom = start_pos.y + start_size.height;
                        pos.y = round(local.y);
                        size.height = round(bottom - pos.y);
                        if f32::from(size.height) < MIN_H {
                            size.height = px(MIN_H);
                            pos.y = round(bottom - px(MIN_H));
                        }
                    }
                    // Right/bottom edges: the fixed left/top edge is
                    // self.pos (never changes during the gesture), so width/
                    // height is just cursor − edge. Deriving these from the
                    // snapshot instead mixes frames — `grab` is surface-
                    // local while `local` is output-local — which jumps the
                    // size by `pos` at press.
                    Edge::Right => {
                        size.width = round((local.x - self.pos.x).max(px(MIN_W)));
                    }
                    Edge::Bottom => {
                        size.height = round((local.y - self.pos.y).max(px(MIN_H)));
                    }
                }
                if size.width != self.size.width || size.height != self.size.height {
                    self.size = size;
                    self.pos = pos;
                    cx.notify(); // render applies resize (+ margin for L/T)
                } else if pos != self.pos {
                    self.pos = pos; // min-clamp shift without a size change
                    cx.notify();
                }
            }
        }
        true
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let alpha = self.cfg.window.panel_alpha;
        let radius = self.cfg.window.corner_radius;
        div()
            .h(px(36.0))
            .flex()
            .items_center()
            .px(px(12.0))
            .gap(px(4.0))
            .bg(hsla(220.0, 0.2, 0.16, alpha))
            // The parent's overflow_hidden does not clip children to the
            // rounded corners; round the bar's own corners to match.
            .rounded_t(px(radius))
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_move_down))
            .child(
                div()
                    .flex_1()
                    .text_size(px(14.0))
                    .text_color(hsla(0.0, 0.0, 1.0, 0.85))
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .id("sync")
                    .px(px(6.0))
                    .cursor_pointer()
                    .on_click(cx.listener(|_, _, _, _| {
                        debug!("sticky sync: no file bound yet (M3 wiring)");
                    }))
                    .child("↻"),
            )
            .child(
                div()
                    .id("close")
                    .px(px(6.0))
                    .cursor_pointer()
                    .on_click(cx.listener(|_, _, window, _| window.remove_window()))
                    .child("✕"),
            )
    }

    /// Bottom bar: overflow metadata (note path/section/mtime land here at
    /// M3); draggable like the header.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let alpha = self.cfg.window.panel_alpha;
        let radius = self.cfg.window.corner_radius;
        div()
            .h(px(28.0))
            .flex()
            .items_center()
            .px(px(12.0))
            .bg(hsla(220.0, 0.2, 0.14, alpha))
            .rounded_b(px(radius))
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_move_down))
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.0))
                    .text_color(hsla(0.0, 0.0, 1.0, 0.45))
                    .child("no note bound yet"),
            )
    }

    fn render_edge(
        &self,
        edge: Edge,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let listener = cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
            this.on_edge_down(edge, ev, window, cx);
        });
        let base = div()
            .absolute()
            .cursor(edge.cursor())
            .on_mouse_down(MouseButton::Left, listener);
        match edge {
            Edge::Left => base.top_0().bottom_0().left_0().w(px(EDGE)),
            Edge::Right => base.top_0().bottom_0().right_0().w(px(EDGE)),
            Edge::Top => base.top_0().left_0().right_0().h(px(EDGE)),
            Edge::Bottom => base.bottom_0().left_0().right_0().h(px(EDGE)),
        }
    }
}

impl Render for Sticky {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Stage only — no cx.notify() needed: this very frame presents right
        // after render, and its commit carries the staged margin and resize
        // together with the new buffer (one atomic configure).
        let pos_changed = self.pos != self.sent_pos;
        let size_changed = self.size != self.sent_size;
        if size_changed {
            let s = self.size;
            self.sent_size = s;
            debug!("sticky resize {:?}", s);
            window.resize(s);
        }
        if pos_changed {
            let p = self.pos;
            self.sent_pos = p;
            debug!("sticky set_margin {:?}", p);
            window.set_margin((p.y, px(0.), px(0.), p.x));
        }
        // A window-wide cursor request (hitbox_id: None) wins over hover
        // styles for the frame, so the gesture keeps its cursor even when
        // the pointer outruns the hit strips. Cleared next frame it stops.
        if let Some(gesture) = &self.gesture {
            window.set_window_cursor_style(gesture.cursor());
        }
        let alpha = self.cfg.window.panel_alpha;
        let radius = self.cfg.window.corner_radius;
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(hsla(220.0, 0.2, 0.12, alpha))
            .rounded(px(radius))
            .border_1()
            .border_color(hsla(0.0, 0.0, 1.0, 0.18))
            .overflow_hidden()
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .child(self.render_header(cx))
            .child(
                div()
                    .flex_1()
                    .p(px(12.0))
                    .text_size(px(13.0))
                    .text_color(hsla(0.0, 0.0, 1.0, 0.6))
                    .child("M1.5 skeleton — markdown body lands with the M3 preview pipeline."),
            )
            .child(self.render_footer(cx))
            .child(self.render_edge(Edge::Left, cx))
            .child(self.render_edge(Edge::Right, cx))
            .child(self.render_edge(Edge::Top, cx))
            .child(self.render_edge(Edge::Bottom, cx))
    }
}
