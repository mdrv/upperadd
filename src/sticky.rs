use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    App, AppContext, Bounds, ClickEvent, Context, CursorStyle, InteractiveElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Size, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, hsla,
    layer_shell::*, point, prelude::*, px, size,
};
use log::debug;

use crate::config::Config;
use crate::markdown;
use crate::worker::IndexCmd;

const DEFAULT_W: f32 = 420.0;
const DEFAULT_H: f32 = 560.0;
/// Smallest allowed sticky size (edge-drag resize clamps here).
const MIN_W: f32 = 280.0;
const MIN_H: f32 = 180.0;
/// Width/height of the invisible edge hit strips.
const EDGE: f32 = 6.0;
/// Corner grab squares (larger than edge strips: corners are the hardest
/// target to hit, and they resize both axes at once).
const CORNER: f32 = 12.0;
/// First sticky's offset from the output's top-left; each next one cascades.
const BASE_POS: f32 = 100.0;
const CASCADE: f32 = 24.0;
/// Cursor poll rate while moving (window follows cursor, so every tick
/// redraws anyway).
const MOVE_POLL: Duration = Duration::from_millis(16);
/// Cursor poll rate while resizing. Equal to the move rate: the sticky is
/// not eased — every poll jumps straight to the cursor — so this is the
/// upper bound on how far a moving edge trails the pointer, and small fast
/// steps keep each compositor size-apply visually negligible.
const RESIZE_POLL: Duration = MOVE_POLL;
/// A fast fling exits the surface at once; keep following the cursor while
/// it is outside, but end the gesture after this long without re-entry.
const GESTURE_LEAVE_GRACE: Duration = Duration::from_millis(250);
/// While resizing, the cursor rides exactly on the moving edge (the size is
/// derived from it) or pushes past it against the screen edge — count the
/// cursor as "inside" within this band so the leave-grace never fires mid-
/// resize. Move gestures don't need it (the window follows the cursor).
const RESIZE_EDGE_BAND: f32 = 24.0;

/// Which cardinal edge a resize drag started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    fn cursor(self) -> CursorStyle {
        self.dir().cursor()
    }

    fn dir(self) -> ResizeDir {
        match self {
            Edge::Left => ResizeDir::Left,
            Edge::Right => ResizeDir::Right,
            Edge::Top => ResizeDir::Top,
            Edge::Bottom => ResizeDir::Bottom,
        }
    }
}

/// Which corner a resize drag started from (both axes at once).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn dir(self) -> ResizeDir {
        match self {
            Corner::TopLeft => ResizeDir::TopLeft,
            Corner::TopRight => ResizeDir::TopRight,
            Corner::BottomLeft => ResizeDir::BottomLeft,
            Corner::BottomRight => ResizeDir::BottomRight,
        }
    }
}

/// The axis-resolved resize direction: which edges move and which stay put.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeDir {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeDir {
    fn cursor(self) -> CursorStyle {
        match self {
            ResizeDir::Left | ResizeDir::Right => CursorStyle::ResizeLeftRight,
            ResizeDir::Top | ResizeDir::Bottom => CursorStyle::ResizeUpDown,
            ResizeDir::TopLeft | ResizeDir::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            ResizeDir::TopRight | ResizeDir::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        }
    }

    /// Whether the left edge (x origin) follows the cursor.
    fn moves_x(self) -> bool {
        matches!(self, ResizeDir::Left | ResizeDir::TopLeft | ResizeDir::BottomLeft)
    }

    /// Whether the top edge (y origin) follows the cursor.
    fn moves_y(self) -> bool {
        matches!(self, ResizeDir::Top | ResizeDir::TopLeft | ResizeDir::TopRight)
    }

    /// Whether this direction drives the width at all — a pure vertical
    /// resize leaves the snapshot width alone.
    fn active_x(self) -> bool {
        !matches!(self, ResizeDir::Top | ResizeDir::Bottom)
    }

    /// Whether this direction drives the height at all — a pure horizontal
    /// resize leaves the snapshot height alone.
    fn active_y(self) -> bool {
        !matches!(self, ResizeDir::Left | ResizeDir::Right)
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
        dir: ResizeDir,
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
            Gesture::Resize { dir, .. } => dir.cursor(),
        }
    }
}

/// A pinned note: read-only, always on top (`Layer::Top`), never takes the
/// keyboard, draggable by its header, resizable by its edges and corners,
/// closable via ✕. The body renders the bound section's markdown (same
/// pipeline as the preview pane); ↻ re-reads it via the index worker.
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
    /// Bound section (vault-relative path, section line) — what ↻ re-reads
    /// and what relative image URLs resolve against.
    key: (String, u32),
    /// Markdown source of the bound section.
    body: String,
    index_tx: UnboundedSender<IndexCmd>,
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
    key: (String, u32),
    body: String,
    index_tx: UnboundedSender<IndexCmd>,
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
        cx.new(|cx| Sticky::new(cfg, title, pos, win_size, output_origin, key, body, index_tx, cx))
    })?;
    Ok(())
}

impl Sticky {
    #[allow(clippy::too_many_arguments)]
    fn new(
        cfg: Config,
        title: String,
        pos: Point<Pixels>,
        size: Size<Pixels>,
        output_origin: Point<Pixels>,
        key: (String, u32),
        body: String,
        index_tx: UnboundedSender<IndexCmd>,
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
            key,
            body,
            index_tx,
            cfg,
        }
    }

    /// ↻: re-read the bound section via the index worker (fs-watch already
    /// keeps the DB fresh; this pulls the latest into this sticky).
    fn on_sync(
        &mut self,
        _: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (tx, rx) = futures::channel::oneshot::channel();
        if self
            .index_tx
            .unbounded_send(IndexCmd::Section {
                path: self.key.0.clone(),
                line: self.key.1,
                resp: tx,
            })
            .is_err()
        {
            return;
        }
        cx.spawn(async move |this, cx| {
            if let Ok(Some(content)) = rx.await {
                this.update(cx, |s, cx| {
                    s.body = content;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// The note's directory — relative image URLs in the body resolve
    /// against it (same rule as the preview pane).
    fn note_dir(&self) -> PathBuf {
        self.cfg
            .notes
            .dir
            .join(&self.key.0)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cfg.notes.dir.clone())
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
        dir: ResizeDir,
        ev: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Edge/corner strips overlap the header/footer bars; without this
        // the bar's move handler fires after ours and overwrites the
        // gesture.
        cx.stop_propagation();
        debug!("sticky resize start {:?} at {:?}", dir, ev.position);
        self.start_gesture(
            Gesture::Resize {
                dir,
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
                dir,
                start_pos,
                start_size,
                ..
            } => {
                let (mut pos, mut size) = (*start_pos, *start_size);
                // Snapshot edges never move; moving edges chase the cursor
                // with a min-size clamp. Right/bottom sizes come from the
                // live self.pos (fixed for the whole gesture) — deriving
                // them from the snapshot mixes frames (surface-local press
                // vs output-local cursor) and jumps the size at press.
                // Axes the direction doesn't touch keep the snapshot size.
                let right = round(start_pos.x + start_size.width);
                let bottom = round(start_pos.y + start_size.height);
                if dir.moves_x() {
                    pos.x = round(local.x).min(right - px(MIN_W));
                    size.width = round(right - pos.x);
                } else if dir.active_x() {
                    size.width = round((local.x - self.pos.x).max(px(MIN_W)));
                }
                if dir.moves_y() {
                    pos.y = round(local.y).min(bottom - px(MIN_H));
                    size.height = round(bottom - pos.y);
                } else if dir.active_y() {
                    size.height = round((local.y - self.pos.y).max(px(MIN_H)));
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
                    .on_click(cx.listener(Self::on_sync))
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
                    .child(format!("{}:{}", self.key.0, self.key.1 + 1)),
            )
    }

    fn render_edge(
        &self,
        edge: Edge,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let listener = cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
            this.on_edge_down(edge.dir(), ev, window, cx);
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

    /// Corner grab squares, painted above the edge strips; resize both axes
    /// at once like a true window.
    fn render_corner(
        &self,
        corner: Corner,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let dir = corner.dir();
        let listener = cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
            this.on_edge_down(dir, ev, window, cx);
        });
        let base = div()
            .absolute()
            .size(px(CORNER))
            .cursor(dir.cursor())
            .on_mouse_down(MouseButton::Left, listener);
        match corner {
            Corner::TopLeft => base.top_0().left_0(),
            Corner::TopRight => base.top_0().right_0(),
            Corner::BottomLeft => base.bottom_0().left_0(),
            Corner::BottomRight => base.bottom_0().right_0(),
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
                // Markdown body (same pipeline as the preview pane), wheel-
                // scrollable; overflow_y_scroll needs a stateful element.
                div()
                    .id("sticky-body")
                    .flex_1()
                    .w_full()
                    .overflow_y_scroll()
                    .px(px(14.0))
                    .py(px(12.0))
                    .child(markdown::render_blocks(
                        &markdown::parse(&self.body),
                        &self.note_dir(),
                        &self.cfg.fonts,
                    )),
            )
            .child(self.render_footer(cx))
            .child(self.render_edge(Edge::Left, cx))
            .child(self.render_edge(Edge::Right, cx))
            .child(self.render_edge(Edge::Top, cx))
            .child(self.render_edge(Edge::Bottom, cx))
            .child(self.render_corner(Corner::TopLeft, cx))
            .child(self.render_corner(Corner::TopRight, cx))
            .child(self.render_corner(Corner::BottomLeft, cx))
            .child(self.render_corner(Corner::BottomRight, cx))
    }
}
