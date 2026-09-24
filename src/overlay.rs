use gpui::{
    actions, div, hsla, layer_shell::KeyboardInteractivity, prelude::*, px, white, Context,
    FocusHandle, Global, Render, Window, WindowHandle,
};

use crate::config::Config;

actions!(upperadd, [Hide]);

/// App-global handle to the persistent overlay window (§16.9: exactly one,
/// never destroyed) so IPC tasks and key handlers can reach it.
pub struct OverlayGlobal(pub WindowHandle<Overlay>);

impl Global for OverlayGlobal {}

/// Overlay view state. Lives in the entity, so hide/show cycles never lose
/// search text, selection or scroll (spec: state survives toggling).
pub struct Overlay {
    pub visible: bool,
    focus: FocusHandle,
    cfg: Config,
}

impl Overlay {
    pub fn new(cfg: Config, cx: &mut Context<Self>) -> Self {
        Self {
            visible: false,
            focus: cx.focus_handle(),
            cfg,
        }
    }

    /// One place that maps `visible` onto the surface: unmap/remap via
    /// `set_visible` (owner decision: test it early) and take/release the
    /// exclusive keyboard via the fork's runtime patch.
    pub fn apply_visibility(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.set_visible(self.visible);
        window.set_keyboard_interactivity(if self.visible {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        if self.visible {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Esc. §16.8: surface mutations from inside a window update silently
    /// no-op — defer past the update, then mutate through the window handle.
    fn on_hide(&mut self, _: &Hide, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        self.visible = false;
        cx.defer(|app| {
            let handle = app.global::<OverlayGlobal>().0;
            let _ = handle.update(app, |ov, window, cx| ov.apply_visibility(window, cx));
        });
    }
}

impl Render for Overlay {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().size_full();
        }
        let cfg = &self.cfg.window;
        let out = _window.bounds().size;
        let panel_w = px(f32::from(out.width) * cfg.width_fraction);
        let panel_h = px(f32::from(out.height) * cfg.height_fraction);
        let margin = px(cfg.margin);

        // bottom-left anchor (config default): column justify_end = bottom,
        // items_start = left; the surface itself is a full-output
        // transparent layer, the panel is just an inner div.
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .items_start()
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_hide))
            .child(
                div()
                    .w(panel_w)
                    .h(panel_h)
                    .ml(margin)
                    .mb(margin)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .bg(hsla(220.0, 0.2, 0.12, cfg.panel_alpha))
                    .rounded(px(cfg.corner_radius))
                    .border_1()
                    .border_color(hsla(0.0, 0.0, 1.0, 0.15))
                    .text_color(white())
                    .child("upperadd — M1 shell (no data yet)")
                    .child("Esc hide · ua toggle show/hide"),
            )
    }
}
