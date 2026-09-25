use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Bounds, Context, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, div, hsla, layer_shell::*, point, prelude::*, px, size,
};
use log::debug;

use crate::config::Config;

const WIDTH: f32 = 420.0;
const HEIGHT: f32 = 560.0;
/// First sticky's offset from the output's top-left; each next one cascades.
const BASE_POS: f32 = 100.0;
const CASCADE: f32 = 24.0;
/// Cursor poll rate while dragging.
const DRAG_POLL: Duration = Duration::from_millis(16);
/// A fast fling exits the 36 px header at once; keep following the cursor
/// while it is outside, but end the drag after this long without re-entry.
const DRAG_LEAVE_GRACE: Duration = Duration::from_millis(250);

/// A pinned note: read-only, always on top (`Layer::Top`), never takes the
/// keyboard, draggable by its header, closable via ✕. M1.5 skeleton — the
/// body is a placeholder until the M3 markdown pipeline lands (spec 01).
pub struct Sticky {
    title: String,
    /// Surface offset from the output's top-left == (top, left) margins.
    pos: Point<Pixels>,
    /// Position actually sent via `set_margin` (advanced in `render`).
    sent: Point<Pixels>,
    /// Layout origin of the output this surface lives on (cursorpos is
    /// global, margins are output-local).
    output_origin: Point<Pixels>,
    drag: Option<Drag>,
    cfg: Config,
}

struct Drag {
    /// `pos = global cursor + offset`, constant while dragging.
    offset: Point<Pixels>,
    /// When the cursor last left the surface, if it is currently outside.
    left_at: Option<Instant>,
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
    let options = WindowOptions {
        titlebar: None,
        focus: false,
        show: true,
        app_id: Some("upperadd-sticky".into()),
        window_background: WindowBackgroundAppearance::Transparent,
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(WIDTH), px(HEIGHT)),
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
        cx.new(|cx| Sticky::new(cfg, title, pos, output_origin, cx))
    })?;
    Ok(())
}

impl Sticky {
    fn new(
        cfg: Config,
        title: String,
        pos: Point<Pixels>,
        output_origin: Point<Pixels>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title,
            pos,
            sent: pos,
            output_origin,
            drag: None,
            cfg,
        }
    }

    fn on_header_down(
        &mut self,
        ev: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        log::debug!("sticky drag start at {:?}", ev.position);
        // At press the surface origin == self.pos, so cursor_global =
        // self.pos + ev.position; with pos = cursor_global + offset that
        // makes offset = −press_local, constant for the whole drag.
        let offset = point(-ev.position.x, -ev.position.y);
        self.drag = Some(Drag {
            offset,
            left_at: None,
        });
        // Ground-truth drag loop: poll the global cursor (origin-independent,
        // so no feedback wiggle) and keep following even when fast movement
        // carries the cursor outside this surface.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(DRAG_POLL).await;
            if !this.update(cx, Sticky::drag_tick).unwrap_or(false) {
                break;
            }
        })
        .detach();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        if self.drag.is_some() && ev.pressed_button != Some(MouseButton::Left) {
            debug!("sticky drag cancelled (button up missed)");
            self.drag = None;
        }
    }

    fn on_up(&mut self, _ev: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.drag = None;
    }

    /// One cursor poll while dragging. Returns false when the drag is over.
    fn drag_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(drag) = &mut self.drag else { return false };
        let Some(cursor) = crate::daemon::hypr_cursor_global() else {
            return true; // Hyprland IPC hiccup; keep the drag alive
        };
        let local = point(
            cursor.x - self.output_origin.x,
            cursor.y - self.output_origin.y,
        );
        // Pointer-leave detection from the same ground truth as the motion
        // (surface events stop when the cursor exits): grace-period the end
        // of the drag so fast flings that exit the header still follow.
        let inside = local.x >= self.pos.x
            && local.x < self.pos.x + px(WIDTH)
            && local.y >= self.pos.y
            && local.y < self.pos.y + px(HEIGHT);
        if inside {
            drag.left_at = None;
        } else {
            let left_at = *drag.left_at.get_or_insert(Instant::now());
            if left_at.elapsed() > DRAG_LEAVE_GRACE {
                debug!("sticky drag ended (cursor left the surface)");
                self.drag = None;
                return false;
            }
        }
        let target = point(local.x + drag.offset.x, local.y + drag.offset.y);
        if target != self.pos {
            debug!("sticky poll {:?} -> {:?}", self.pos, target);
            self.pos = target;
            cx.notify(); // render applies the margin (frame-gated)
        }
        true
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let alpha = self.cfg.window.panel_alpha;
        div()
            .h(px(36.0))
            .flex()
            .items_center()
            .px(px(12.0))
            .gap(px(4.0))
            .bg(hsla(220.0, 0.2, 0.16, alpha))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_header_down))
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
                    .on_click(cx.listener(|_, _, _, _| {
                        debug!("sticky sync: no file bound yet (M3 wiring)");
                    }))
                    .child("↻"),
            )
            .child(
                div()
                    .id("close")
                    .px(px(6.0))
                    .on_click(cx.listener(|_, _, window, _| window.remove_window()))
                    .child("✕"),
            )
    }
}

impl Render for Sticky {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Frame-gated margin application: at most one set_margin per drawn
        // frame, and only when the polled position actually moved.
        if self.pos != self.sent {
            let p = self.pos;
            self.sent = p;
            debug!("sticky set_margin {:?}", p);
            window.set_margin((p.y, px(0.), px(0.), p.x));
        }
        let alpha = self.cfg.window.panel_alpha;
        let radius = self.cfg.window.corner_radius;
        div()
            .size_full()
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
    }
}
