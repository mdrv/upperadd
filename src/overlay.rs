use std::time::{Duration, Instant};

use std::path::Path;

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    div, hsla, layer_shell::KeyboardInteractivity, prelude::*, px, relative, uniform_list, white,
    ClipboardItem, Context, FocusHandle, Global, InteractiveElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollStrategy,
    UniformListScrollHandle, Window, WindowHandle,
};

use crate::config::Config;
use crate::markdown;
use crate::search::{Mode, SearchModel};
use crate::selection::PaneSel;
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
    /// Block selection state for the preview pane (see selection.rs).
    pane_sel: PaneSel,
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
            pane_sel: PaneSel::new(),
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

    /// Enter: hand the selected note to the editor (spec 00 — default
    /// `neovide +{line} {file}` via the arg template in config). The
    /// overlay hides first and the editor is spawned inside the deferred
    /// tick, *after* `apply_visibility` released the exclusive keyboard —
    /// spawn before that and the editor comes up unfocused. `process_group`
    /// detaches it from the daemon's process group so `ua stop` (or a
    /// daemon crash) can't take the editor down.
    fn open_editor(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self.search.results.get(self.search.selected) else {
            return;
        };
        let abs = self.cfg.notes.dir.join(&hit.path);
        let line = (hit.line + 1).to_string();
        let file = abs.display().to_string();
        let args: Vec<String> = self
            .cfg
            .editor
            .args
            .iter()
            .map(|t| t.replace("{line}", &line).replace("{file}", &file))
            .collect();
        let cmd = self.cfg.editor.command.clone();
        self.visible = false;
        cx.defer(move |app| {
            let handle = app.global::<OverlayGlobal>().0;
            let _ = handle.update(app, |ov, window, cx| ov.apply_visibility(window, cx));
            use std::os::unix::process::CommandExt as _;
            match std::process::Command::new(&cmd)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0)
                .spawn()
            {
                Ok(_) => log::info!("editor launched: {cmd} {args:?}"),
                Err(err) => log::warn!("launching editor {cmd}: {err:#}"),
            }
        });
        cx.notify();
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

        // Ctrl+C works in both modes: copy the preview selection.
        if mods.control && ks.key == "c" {
            self.copy_preview_selection(cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }

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
                    // Enter: editor handoff (spec 00) from either mode.
                    "enter" => {
                        self.open_editor(window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }
                    // Pinning is a normal-mode verb.
                    "tab" => {
                        cx.stop_propagation();
                        return;
                    }
                    // Alt+Up/Down: reorder pinned sections (insert-mode alias).
                    "up" | "down" if ks.modifiers.alt => {
                        self.move_pin_selected(ks.key == "up", cx);
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
                // Alt+J/K: reorder pinned sections (before the plain j/k arms).
                // Vim semantics: j moves the pin down, k moves it up.
                "j" | "k" if ks.modifiers.alt => {
                    self.move_pin_selected(ks.key == "k", cx);
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
                // P/Tab: toggle pin-at-top for the selected section
                // (workspace state, survives restarts — spec 01 §2).
                "p" | "tab" => {
                    self.toggle_pin_selected(cx);
                    cx.stop_propagation();
                }
                // S: extract the selected section into a sticky window.
                "s" => {
                    self.stick_selected(cx);
                    cx.stop_propagation();
                }
                "i" => {
                    self.search.enter_insert();
                    cx.stop_propagation();
                    cx.notify();
                }
                // Enter: editor handoff (spec 00).
                "enter" => {
                    self.open_editor(window, cx);
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
        // New section → stale block indices; drop the selection.
        self.pane_sel.clear();
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
            if let Ok(data) = rx.await {
                this.update(cx, |ov, cx| {
                    let still_selected = ov
                        .search
                        .results
                        .get(ov.search.selected)
                        .is_some_and(|h| (h.path.clone(), h.line) == key);
                    if still_selected {
                        ov.preview = Some((key, data.map(|d| d.content).unwrap_or_default()));
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Ctrl+C: copy the currently selected preview blocks as plain text.
    fn copy_preview_selection(&mut self, cx: &mut Context<Self>) {
        let Some((_, content)) = &self.preview else {
            return;
        };
        let Some(range) = self.pane_sel.sel_range() else {
            return;
        };
        let text = markdown::copy_range(&markdown::parse(content), range);
        let item = ClipboardItem::new_string(text);
        cx.write_to_clipboard(item.clone());
        cx.write_to_primary(item);
        self.notice = Some(("copied", Instant::now()));
        // Timed clear — never a fade (§28 animation-freeze rule).
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::selection::COPIED_CHIP)
                .await;
            this.update(cx, |ov, cx| {
                if ov.notice.is_some_and(|(t, _)| t == "copied") {
                    ov.notice = None;
                }
                cx.notify();
            })
            .ok();
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

    /// Extract the selected note into a sticky window (spec 01). The
    /// overlay stays open with its state intact; the section content is
    /// fetched fresh so the sticky shows the note even if the preview pane
    /// hasn't loaded it yet.
    /// Toggle pin-at-top for the selected section, then re-query so the
    /// new order comes back from the worker (single source of truth).
    fn toggle_pin_selected(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.search.results.get(self.search.selected) else {
            return;
        };
        let _ = self.index_tx.unbounded_send(IndexCmd::TogglePin {
            path: hit.path.clone(),
            line: hit.line,
        });
        self.request_results(cx);
    }

    /// Move the selected pin one slot up/down within the pinned group and
    /// keep the selection on the moved note.
    fn move_pin_selected(&mut self, up: bool, cx: &mut Context<Self>) {
        let Some(hit) = self.search.results.get(self.search.selected) else {
            return;
        };
        let path = hit.path.clone();
        let line = hit.line;
        let pinned = hit.pinned;
        let _ = self
            .index_tx
            .unbounded_send(IndexCmd::MovePin { path, line, up });
        // The selection follows the note: pins are a prefix of the results
        // in pin order, so the moved note lands on the adjacent row — but
        // only when that neighbor is itself pinned (boundary = no-op, same
        // clamp the worker applies).
        if pinned {
            let len = self.search.results.len();
            let neighbor = if up {
                self.search.selected.checked_sub(1)
            } else {
                Some(self.search.selected + 1)
            };
            if let Some(ix) = neighbor {
                if ix < len && self.search.results[ix].pinned {
                    self.search.selected = ix;
                }
            }
        }
        self.request_results(cx);
    }

    fn stick_selected(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self.search.results.get(self.search.selected) else {
            return;
        };
        self.pins += 1;
        let index = self.pins;
        let key = (hit.path.clone(), hit.line);
        let cfg = self.cfg.clone();
        let origin = self.origin;
        let index_tx = self.index_tx.clone();
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
        cx.spawn(async move |_, cx| {
            let Ok(Some(data)) = rx.await else {
                log::warn!("pin: section vanished before fetch ({})", key.0);
                return;
            };
            if let Err(err) = cx.update(|app| {
                crate::sticky::spawn(
                    app, &cfg, data.title, index, origin, key, data.content, index_tx,
                )
            }) {
                log::warn!("spawning sticky: {err:#}");
            }
        })
        .detach();
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
                // Pinned rows get an accent dot before the title.
                div()
                    .flex()
                    .gap_1()
                    .when(hit.pinned, |d| {
                        d.child(
                            div()
                                .text_size(px(13.0))
                                .text_color(hsla(120.0, 0.6, 0.6, 0.95))
                                .child("•"),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(hsla(0.0, 0.0, 0.92, 0.95))
                            .truncate()
                            .child(hit.title.clone()),
                    ),
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
                            "{} result{} · Esc⇥normal · P pin · S sticky · Alt+J/K reorder · ⇧R reindex · ⏎ edit",
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
                        // Opaque accent bg: translucent colors composite over
                        // unknown wallpaper pixels, so contrast would be a
                        // gamble (the black-on-brown chip). Text color comes
                        // from the WCAG ratio on the real luminance.
                        let bg = hsla(220.0, 0.5, 0.45, 1.0);
                        d.bg(bg).text_color(markdown::contrast_text(bg))
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

    /// The note's directory — relative image URLs resolve against it
    /// (AGENTS.md fork rule 5).
    fn note_dir(&self, path: &str) -> std::path::PathBuf {
        self.cfg
            .notes
            .dir
            .join(path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cfg.notes.dir.clone())
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(((path, _), content)) = &self.preview else {
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
        let base = self.note_dir(path);
        let blocks = markdown::parse(content);
        let fonts = &self.cfg.fonts;
        let pane_sel = &self.pane_sel;
        let items = blocks.iter().enumerate().map(|(ix, b)| {
            markdown::render_block_div(b, &base, fonts, pane_sel.is_selected(ix))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Self, _: &MouseDownEvent, _, cx| {
                        this.pane_sel.block_down(ix);
                        // Keep the container's clear-handler out of it.
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_move(cx.listener(
                    move |this: &mut Self, _: &MouseMoveEvent, _, cx| {
                        if this.pane_sel.block_drag(ix) {
                            cx.notify();
                        }
                    },
                ))
        });
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_preview_clear))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_preview_up))
            .children(items)
            .into_any_element()
    }

    /// Click on empty preview space: drop the selection.
    fn on_preview_clear(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.pane_sel.clear() {
            cx.notify();
        }
    }

    fn on_preview_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.pane_sel.block_up() {
            cx.notify();
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
                    .child(self.render_preview(cx)),
            )
    }
}
