use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    div, hsla, layer_shell::KeyboardInteractivity, prelude::*, px, relative, uniform_list, white,
    Context, FocusHandle, FontStyle, Global, HighlightStyle, InteractiveElement, KeyDownEvent,
    Pixels, Point, Render, ScrollStrategy, StrikethroughStyle, StyledText, UnderlineStyle,
    UniformListScrollHandle, Window, WindowHandle, FontWeight,
};

use crate::config::Config;
use crate::markdown::{self, Block, BlockKind, Inline};
use crate::search::{Mode, SearchModel};
use crate::worker::IndexCmd;

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
    search: SearchModel,
    list_scroll: UniformListScrollHandle,
    index_tx: UnboundedSender<IndexCmd>,
    /// Output origin (global layout coords) handed to pinned stickies.
    origin: Point<Pixels>,
    pins: usize,
    /// Rendered note content: ((path, line) → markdown source).
    preview: Option<((String, u32), String)>,
    /// Transient status-bar feedback: (message, shown-at).
    notice: Option<(&'static str, Instant)>,
}

impl Overlay {
    pub fn new(
        cfg: Config,
        index_tx: UnboundedSender<IndexCmd>,
        origin: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            visible: false,
            focus: cx.focus_handle(),
            cfg,
            search: SearchModel::new(),
            list_scroll: UniformListScrollHandle::new(),
            index_tx,
            origin,
            pins: 0,
            preview: None,
            notice: None,
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
            // Fresh results for the current query (also the very first show).
            self.request_results(cx);
        }
        cx.notify();
    }

    /// Hide the overlay (normal-mode Esc). §16.8: surface mutations from
    /// inside a window update silently no-op — defer past the update, then
    /// mutate through the window handle.
    ///
    /// This is a plain key-handler path, not an action: a bound
    /// `escape`→Hide action fired *alongside* `on_key_down` regardless of
    /// `stop_propagation`, so insert-Esc hid the overlay instead of
    /// stepping back to normal mode. With no escape binding anywhere, the
    /// key handler owns Esc completely (insert consumes, normal hides).
    fn hide(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        self.visible = false;
        cx.defer(|app| {
            let handle = app.global::<OverlayGlobal>().0;
            let _ = handle.update(app, |ov, window, cx| ov.apply_visibility(window, cx));
        });
    }

    /// Send the current query to the index worker; the reply is applied only
    /// if it answers the newest request (generation guard).
    fn request_results(&mut self, cx: &mut Context<Self>) {
        let generation = self.search.bump_generation();
        let query = self.search.query.clone();
        let (tx, rx) = futures::channel::oneshot::channel();
        if self
            .index_tx
            .unbounded_send(IndexCmd::Search { query, resp: tx })
            .is_err()
        {
            return;
        }
        cx.spawn(async move |this, cx| {
            if let Ok(results) = rx.await {
                this.update(cx, |ov, cx| {
                    if ov.search.apply_results(results, generation) {
                        ov.request_preview(cx);
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let mods = ks.modifiers;
        let plain = !mods.control && !mods.alt && !mods.platform;
        let printable = || -> Option<char> {
            if !plain {
                return None;
            }
            ks.key_char
                .as_ref()
                .and_then(|s| s.chars().next())
                .filter(|c| !c.is_control())
        };

        match self.search.mode {
            Mode::Insert => {
                let mut query_changed = false;
                match ks.key.as_str() {
                    // Two-stage Esc (spec 01): insert → normal. Consumed
                    // here so Esc never hides the overlay from insert mode.
                    "escape" => {
                        self.search.enter_normal();
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    // Editor handoff lands in M4; swallow for now so typing
                    // sessions don't accidentally trigger anything.
                    "enter" => {
                        cx.stop_propagation();
                        return;
                    }
                    // Pinning is a normal-mode verb.
                    "tab" => {
                        cx.stop_propagation();
                        return;
                    }
                    "up" => {
                        if self.move_selection(-1) {
                            self.request_preview(cx);
                            cx.notify();
                        }
                        cx.stop_propagation();
                        return;
                    }
                    "down" => {
                        if self.move_selection(1) {
                            self.request_preview(cx);
                            cx.notify();
                        }
                        cx.stop_propagation();
                        return;
                    }
                    "backspace" if plain => query_changed = self.search.backspace(),
                    "delete" if plain => query_changed = self.search.delete(),
                    "left" if plain => _ = self.search.left(),
                    "right" if plain => _ = self.search.right(),
                    "home" if plain => _ = self.search.home(),
                    "end" if plain => _ = self.search.end(),
                    _ => {
                        if let Some(ch) = printable() {
                            query_changed = self.search.insert(ch);
                        } else {
                            return;
                        }
                    }
                }
                if query_changed {
                    self.request_results(cx);
                }
                cx.stop_propagation();
                cx.notify();
            }
            Mode::Normal => match ks.key.as_str() {
                // Second Esc stage: hide. `stop_propagation` is belt and
                // suspenders — no other escape handler exists anymore.
                "escape" => {
                    self.hide(window, cx);
                    cx.stop_propagation();
                }
                "j" | "down" => {
                    if self.move_selection(1) {
                        self.request_preview(cx);
                        cx.notify();
                    }
                    cx.stop_propagation();
                }
                "k" | "up" => {
                    if self.move_selection(-1) {
                        self.request_preview(cx);
                        cx.notify();
                    }
                    cx.stop_propagation();
                }
                // P/Tab pin the selected note (spec 01).
                "p" | "tab" => {
                    self.pin_selected(cx);
                    cx.stop_propagation();
                }
                "i" | "enter" => {
                    self.search.enter_insert();
                    cx.stop_propagation();
                    cx.notify();
                }
                // Shift+R: full reindex from the vault (same as `ua reindex`).
                // Plain `r` falls through to the printable arm (types `r`).
                "r" | "R" if mods.shift || ks.key == "R" => {
                    let _ = self.index_tx.unbounded_send(IndexCmd::Reindex);
                    self.notice = Some(("reindexing vault…", Instant::now()));
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_secs(2))
                            .await;
                        this.update(cx, |ov, cx| {
                            if let Some((_, at)) = ov.notice {
                                if at.elapsed() >= Duration::from_secs(2) {
                                    ov.notice = None;
                                    cx.notify();
                                }
                            }
                        })
                        .ok();
                    })
                    .detach();
                    cx.stop_propagation();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = printable() {
                        // Any other printable drops you back into insert and
                        // types the char (fzf feel: just type to filter).
                        self.search.enter_insert();
                        if self.search.insert(ch) {
                            self.request_results(cx);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            },
        }
    }

    /// Fetch the selected section's content for the preview pane. Replies
    /// are applied only if the selection hasn't moved since the request.
    fn request_preview(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.search.results.get(self.search.selected) else {
            self.preview = None;
            return;
        };
        let key = (hit.path.clone(), hit.line);
        if self
            .preview
            .as_ref()
            .is_some_and(|(k, _)| *k == key)
        {
            return;
        }
        let (tx, rx) = futures::channel::oneshot::channel();
        if self
            .index_tx
            .unbounded_send(IndexCmd::Section {
                path: key.0.clone(),
                line: key.1,
                resp: tx,
            })
            .is_err()
        {
            return;
        }
        cx.spawn(async move |this, cx| {
            if let Ok(content) = rx.await {
                this.update(cx, |ov, cx| {
                    let still_selected = ov
                        .search
                        .results
                        .get(ov.search.selected)
                        .is_some_and(|h| (h.path.clone(), h.line) == key);
                    if still_selected {
                        ov.preview = Some((key, content.unwrap_or_default()));
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Move the selection and keep it in view.
    fn move_selection(&mut self, delta: i64) -> bool {
        if self.search.move_selection(delta) {
            self.list_scroll
                .scroll_to_item(self.search.selected, ScrollStrategy::Nearest);
            return true;
        }
        false
    }

    /// Extract the selected note into a sticky window (spec 01). The overlay
    /// stays open with its state intact; sticky content lands with M3.
    fn pin_selected(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.search.results.get(self.search.selected) else {
            return;
        };
        self.pins += 1;
        let title = hit.title.clone();
        if let Err(err) = crate::sticky::spawn(cx, &self.cfg, title, self.pins, self.origin) {
            log::warn!("spawning sticky: {err:#}");
        }
    }

    // ----- rendering -----------------------------------------------------

    fn render_query_line(&self) -> gpui::Div {
        let chars: Vec<char> = self.search.query.chars().collect();
        let at = self.search.cursor.min(chars.len());
        let before: String = chars[..at].iter().collect();
        let caret = chars.get(at).copied();
        let after: String = chars[(at + usize::from(caret.is_some())).min(chars.len())..]
            .iter()
            .collect();
        let insert = self.search.mode == Mode::Insert;

        div()
            .h(px(44.0))
            .w_full()
            .px(px(14.0))
            .flex()
            .items_center()
            .gap_1p5()
            .border_b_1()
            .border_color(hsla(0.0, 0.0, 1.0, 0.08))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(hsla(220.0, 0.5, 0.65, 0.9))
                    .child("❯"),
            )
            .child(div().text_size(px(13.5)).child(before))
            .child(
                // Block caret on the char under the cursor; dim when normal.
                div()
                    .text_size(px(13.5))
                    .text_color(hsla(220.0, 0.2, 0.10, 1.0))
                    .when(insert, |d| d.bg(hsla(0.0, 0.0, 0.92, 0.9)))
                    .when(!insert, |d| d.text_color(hsla(0.0, 0.0, 1.0, 0.35)))
                    .child(caret.map(String::from).unwrap_or_else(|| " ".into())),
            )
            .child(div().text_size(px(13.5)).child(after))
    }

    fn render_result_row(&self, ix: usize) -> gpui::AnyElement {
        let hit = &self.search.results[ix];
        let selected = ix == self.search.selected;
        let subtitle = format!("{}:{}", hit.path, hit.line + 1);
        div()
            .id(ix)
            .h(px(46.0))
            .w_full()
            .px(px(14.0))
            .flex()
            .flex_col()
            .justify_center()
            .when(selected, |d| d.bg(hsla(220.0, 0.30, 0.32, 0.55)))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(hsla(0.0, 0.0, 0.92, 0.95))
                    .truncate()
                    .child(hit.title.clone()),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(hsla(0.0, 0.0, 0.75, 0.55))
                    .truncate()
                    .child(subtitle),
            )
            .into_any_element()
    }

    fn render_results(&self, cx: &mut Context<Self>) -> gpui::Div {
        if self.search.len() == 0 {
            return div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(hsla(0.0, 0.0, 0.75, 0.4))
                        .child("no matches"),
                );
        }
        let len = self.search.len();
        div().flex_1().w_full().overflow_hidden().child(
            uniform_list("results", len, cx.processor(|this, range: std::ops::Range<usize>, _window, _cx| {
                range.map(|ix| this.render_result_row(ix)).collect()
            }))
            .track_scroll(&self.list_scroll)
            .h_full()
            .w_full(),
        )
    }

    fn render_status(&self) -> gpui::Div {
        let insert = self.search.mode == Mode::Insert;
        div()
            .h(px(28.0))
            .w_full()
            .px(px(14.0))
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(hsla(0.0, 0.0, 1.0, 0.08))
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(hsla(0.0, 0.0, 0.75, 0.5))
                    .child(match self.notice {
                        // Fresh notice replaces the hints for its lifetime.
                        Some((msg, at)) if at.elapsed() < Duration::from_secs(2) => {
                            msg.to_string()
                        }
                        _ => format!(
                            "{} result{} · Esc⇥normal · j/k move · P pin · ⇧R reindex",
                            self.search.len(),
                            if self.search.len() == 1 { "" } else { "s" }
                        ),
                    }),
            )
            .child(
                div()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .text_size(px(10.0))
                    .when(insert, |d| {
                        d.bg(hsla(220.0, 0.5, 0.45, 0.8))
                            .text_color(hsla(220.0, 0.2, 0.10, 1.0))
                    })
                    .when(!insert, |d| {
                        d.bg(hsla(0.0, 0.0, 1.0, 0.12))
                            .text_color(hsla(0.0, 0.0, 0.9, 0.8))
                    })
                    .child(if insert { "INSERT" } else { "NORMAL" }),
            )
    }

    fn render_left_panel(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .w(relative(0.25))
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(hsla(0.0, 0.0, 1.0, 0.08))
            .child(self.render_query_line())
            .child(self.render_results(cx))
            .child(self.render_status())
    }

    fn render_preview(&self) -> gpui::AnyElement {
        let Some((_, content)) = &self.preview else {
            return div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(hsla(0.0, 0.0, 0.75, 0.3))
                        .child("select a note"),
                )
                .into_any_element();
        };
        let blocks = markdown::parse(content);
        div()
            .id("preview")
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .gap_2p5()
            .px(px(20.0))
            .py(px(16.0))
            .overflow_y_scroll()
            .children(blocks.iter().map(|b| self.render_block(b)))
            .into_any_element()
    }

    fn render_block(&self, block: &Block) -> gpui::AnyElement {
        match block {
            Block::Rule => div()
                .h(px(1.0))
                .w_full()
                .bg(hsla(0.0, 0.0, 1.0, 0.12))
                .into_any_element(),
            Block::Code { code } => div()
                .font_family(".monospace")
                .text_size(px(11.5))
                .text_color(hsla(0.0, 0.0, 0.85, 0.9))
                .bg(hsla(0.0, 0.0, 1.0, 0.05))
                .rounded(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .child(code.clone())
                .into_any_element(),
            Block::Styled { kind, text, spans } => {
                let line = div().child(self.styled_line(text, spans));
                let block = match kind {
                    BlockKind::Heading(level) => {
                        let size = match level {
                            1 => 20.0,
                            2 => 17.0,
                            3 => 15.0,
                            _ => 13.5,
                        };
                        line.text_size(px(size)).font_weight(FontWeight::BOLD)
                    }
                    BlockKind::Paragraph => {
                        line.text_size(px(13.0)).line_height(relative(1.5))
                    }
                    BlockKind::Quote => line
                        .text_size(px(13.0))
                        .line_height(relative(1.5))
                        .border_l_2()
                        .border_color(hsla(220.0, 0.5, 0.55, 0.8))
                        .pl(px(10.0))
                        .text_color(hsla(0.0, 0.0, 0.85, 0.7)),
                    BlockKind::Item => line.text_size(px(13.0)),
                };
                if *kind == BlockKind::Item {
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(hsla(220.0, 0.5, 0.65, 0.9))
                                .child("•"),
                        )
                        .child(block)
                        .into_any_element()
                } else {
                    block.into_any_element()
                }
            }
        }
    }

    /// One block's text with inline highlight spans applied.
    fn styled_line(&self, text: &str, spans: &[(std::ops::Range<usize>, Inline)]) -> StyledText {
        if spans.is_empty() {
            return StyledText::new(text.to_string());
        }
        StyledText::new(text.to_string()).with_highlights(
            spans
                .iter()
                .map(|(range, inline)| (range.clone(), self.highlight(*inline))),
        )
    }

    fn highlight(&self, inline: Inline) -> HighlightStyle {
        let none = HighlightStyle {
            color: None,
            font_weight: None,
            font_style: None,
            background_color: None,
            underline: None,
            strikethrough: None,
            fade_out: None,
        };
        match inline {
            Inline::Bold => HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..none
            },
            Inline::Italic => HighlightStyle {
                font_style: Some(FontStyle::Italic),
                ..none
            },
            Inline::Code => HighlightStyle {
                background_color: Some(hsla(0.0, 0.0, 1.0, 0.10)),
                ..none
            },
            Inline::Strike => HighlightStyle {
                strikethrough: Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: None,
                }),
                ..none
            },
            Inline::Link => HighlightStyle {
                color: Some(hsla(215.0, 0.6, 0.7, 1.0)),
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: None,
                    wavy: false,
                }),
                ..none
            },
        }
    }
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return div().size_full();
        }
        let cfg = &self.cfg.window;
        let out = window.bounds().size;
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
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .w(panel_w)
                    .h(panel_h)
                    .ml(margin)
                    .mb(margin)
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .bg(hsla(220.0, 0.2, 0.12, cfg.panel_alpha))
                    .rounded(px(cfg.corner_radius))
                    .border_1()
                    .border_color(hsla(0.0, 0.0, 1.0, 0.15))
                    .text_color(white())
                    // fzf layout (spec 00): left search+results, right preview.
                    .child(self.render_left_panel(cx))
                    .child(self.render_preview()),
            )
    }
}
