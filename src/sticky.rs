use gpui::{
    div, hsla, layer_shell::*, point, prelude::*, px, size, App, AppContext, Bounds, Context,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use log::debug;

use crate::config::Config;

const WIDTH: f32 = 420.0;
const HEIGHT: f32 = 560.0;
/// First sticky's offset from the output's top-left; each next one cascades.
const BASE_POS: f32 = 100.0;
const CASCADE: f32 = 24.0;

/// A pinned note: read-only, always on top (`Layer::Top`), never takes the
/// keyboard, draggable by its header, closable via ✕. M1.5 skeleton — the
/// body is a placeholder until the M3 markdown pipeline lands (spec 01).
pub struct Sticky {
    title: String,
    /// Surface offset from the output's top-left == (top, left) margins.
    pos: gpui::Point<gpui::Pixels>,
    drag: Option<Drag>,
    cfg: Config,
}

struct Drag {
    press: gpui::Point<gpui::Pixels>,
    start: gpui::Point<gpui::Pixels>,
}

/// Open a sticky window. Unanchored layer surface: the margin IS the
/// position (fork patch `Window::set_margin`, tag 0.0.260925.1). Explicit
/// size wins here — §16.3's "0×0 bounds" rule is the inverse (all-anchors)
/// case.
pub fn spawn(cx: &mut App, cfg: &Config, title: String, index: usize) -> anyhow::Result<()> {
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
            anchor: Anchor::empty(),
            exclusive_zone: Some(px(-1.)),
            margin: Some((pos.y, px(0.), px(0.), pos.x)),
            keyboard_interactivity: KeyboardInteractivity::None,
            ..Default::default()
        }),
        ..Default::default()
    };
    let cfg = cfg.clone();
    cx.open_window(options, |_, cx| {
        cx.new(|cx| Sticky::new(cfg, title, pos, cx))
    })?;
    Ok(())
}

impl Sticky {
    fn new(
        cfg: Config,
        title: String,
        pos: gpui::Point<gpui::Pixels>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title,
            pos,
            drag: None,
            cfg,
        }
    }

    fn on_header_down(
        &mut self,
        ev: &MouseDownEvent,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.drag = Some(Drag {
            press: ev.position,
            start: self.pos,
        });
        window.refresh();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, _cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        self.pos = point(
            drag.start.x + (ev.position.x - drag.press.x),
            drag.start.y + (ev.position.y - drag.press.y),
        );
        window.set_margin((self.pos.y, px(0.), px(0.), self.pos.x));
    }

    fn on_up(&mut self, _ev: &MouseUpEvent, window: &mut Window, _cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            window.refresh();
        }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
