//! Index worker thread: owns the DB, the in-memory search corpus, and the
//! fs-watcher. UI and IPC talk to it over a channel so DB access never
//! blocks the UI thread (gpui §16.1 shape, but on a dedicated OS thread).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot;
use futures::{FutureExt, StreamExt};
use log::{debug, error, info, warn};

use crate::config::Separator;
use crate::db::{Db, SectionMeta};
use crate::index::{self, ParsedSection};

/// Coalesce editor save-storms before touching the DB.
const FILE_DEBOUNCE: Duration = Duration::from_millis(250);
const SEARCH_LIMIT: u32 = 100;
/// fjall lock contention (e.g. `mdrv-db verify` running) resolves in seconds.
const OPEN_RETRIES: u32 = 3;

pub enum IndexCmd {
    Reindex,
    FilesChanged(Vec<PathBuf>),
    Search {
        query: String,
        resp: oneshot::Sender<Vec<SearchHit>>,
    },
    Stats {
        resp: oneshot::Sender<String>,
    },
    /// One section's content for the preview pane.
    Section {
        path: String,
        line: u32,
        resp: oneshot::Sender<Option<String>>,
    },
    /// Toggle pin-at-top for a section (workspace state, DB-backed).
    TogglePin {
        path: String,
        line: u32,
    },
    /// Move a pinned section one slot up/down within the pinned group.
    MovePin {
        path: String,
        line: u32,
        up: bool,
    },
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    pub title: String,
    pub section_mtime: i64,
    pub score: i64,
    /// Pinned sections sort to the top in user-defined order.
    pub pinned: bool,
}

/// Spawn the worker + fs-watcher threads; returns the command sender.
pub fn spawn(notes_dir: PathBuf, separator: Separator) -> UnboundedSender<IndexCmd> {
    let (tx, rx) = futures::channel::mpsc::unbounded::<IndexCmd>();
    let watch_tx = tx.clone();
    let watch_dir = notes_dir.clone();
    thread::Builder::new()
        .name("upperadd-index".into())
        .spawn(move || run(notes_dir, separator, rx))
        .expect("spawning index worker");
    thread::Builder::new()
        .name("upperadd-watch".into())
        .spawn(move || watch(watch_dir, watch_tx))
        .expect("spawning fs-watcher");
    tx
}

fn watch(notes_dir: PathBuf, tx: UnboundedSender<IndexCmd>) {
    use notify::Watcher as _;
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(event) => {
                if matches!(event.kind, notify::event::EventKind::Access(_)) {
                    return;
                }
                let _ = ev_tx.send(event.paths);
            }
            Err(err) => warn!("fs-watch error: {err}"),
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                warn!("fs-watch unavailable: {err}");
                return;
            }
        };
    if let Err(err) = watcher.watch(&notes_dir, notify::RecursiveMode::Recursive) {
        warn!("watching {}: {err}", notes_dir.display());
        return;
    }
    info!("fs-watch: watching {}", notes_dir.display());
    while let Ok(paths) = ev_rx.recv() {
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .collect();
        if paths.is_empty() {
            continue;
        }
        if tx.unbounded_send(IndexCmd::FilesChanged(paths)).is_err() {
            break; // worker gone: daemon is shutting down
        }
    }
}

fn run(notes_dir: PathBuf, separator: Separator, mut rx: UnboundedReceiver<IndexCmd>) {
    let db = match open_with_retries() {
        Ok(db) => db,
        Err(err) => {
            error!("opening index DB: {err:#}");
            info!("continuing without an index (search unavailable)");
            return drain_degraded(rx);
        }
    };
    let mut worker = Worker::new(notes_dir, separator, db);
    if let Err(err) = worker.initial_import() {
        error!("initial index: {err:#}");
    }
    worker.load_pins();
    while let Some(cmd) = futures::executor::block_on(rx.next()) {
        worker.handle(cmd, &mut rx);
    }
}

/// No DB (e.g. lock held elsewhere at every retry): stay alive, answer
/// everything honestly, and keep retrying the open on each reindex request.
fn drain_degraded(mut rx: UnboundedReceiver<IndexCmd>) {
    while let Some(cmd) = futures::executor::block_on(rx.next()) {
        match cmd {
            IndexCmd::Stats { resp } => {
                let _ = resp.send("db unavailable (see journal)".into());
            }
            IndexCmd::Search { resp, .. } => {
                let _ = resp.send(Vec::new());
            }
            IndexCmd::Section { resp, .. } => {
                let _ = resp.send(None);
            }
            IndexCmd::Reindex | IndexCmd::FilesChanged(_) => {}
            IndexCmd::TogglePin { .. } | IndexCmd::MovePin { .. } => {}
        }
    }
}

fn open_with_retries() -> Result<Db> {
    let mut last = None;
    for attempt in 1..=OPEN_RETRIES {
        match Db::open() {
            Ok(db) => return Ok(db),
            Err(err) => {
                warn!("opening index DB (attempt {attempt}/{OPEN_RETRIES}): {err:#}");
                last = Some(err);
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
    Err(last.expect("at least one attempt")).context("index DB open failed")
}

struct Worker {
    notes_dir: PathBuf,
    separator: Separator,
    db: Db,
    corpus: Vec<SectionMeta>,
    /// Pinned sections of the default workspace, user-defined order.
    /// DB-backed (survives restarts); pruned against the corpus.
    pinned: Vec<(String, u32)>,
    /// True while the table has never been populated: fresh sections fall
    /// back to the file mtime instead of "now".
    cold: bool,
}

impl Worker {
    fn new(notes_dir: PathBuf, separator: Separator, db: Db) -> Self {
        let cold = db.section_count().map(|n| n == 0).unwrap_or(true);
        Self {
            notes_dir,
            separator,
            db,
            corpus: Vec::new(),
            pinned: Vec::new(),
            cold,
        }
    }

    fn handle(&mut self, cmd: IndexCmd, rx: &mut UnboundedReceiver<IndexCmd>) {
        match cmd {
            IndexCmd::Reindex => {
                if let Err(err) = self.reindex() {
                    error!("reindex: {err:#}");
                }
            }
            IndexCmd::FilesChanged(paths) => {
                thread::sleep(FILE_DEBOUNCE);
                // Coalesce whatever queued up behind us (editor save storms).
                let mut pending = paths;
                let mut deferred = Vec::new();
                while let Some(Some(cmd)) = rx.next().now_or_never() {
                    match cmd {
                        IndexCmd::FilesChanged(more) => pending.extend(more),
                        other => deferred.push(other),
                    }
                }
                for path in pending {
                    if let Err(err) = self.index_changed(&path) {
                        warn!("indexing {}: {err:#}", path.display());
                    }
                }
                for cmd in deferred {
                    self.handle(cmd, rx);
                }
            }
            IndexCmd::Search { query, resp } => {
                let _ = resp.send(self.search(&query));
            }
            IndexCmd::Section { path, line, resp } => {
                let content = self.db.section_content(&path, line).ok().flatten();
                let _ = resp.send(content);
            }
            IndexCmd::TogglePin { path, line } => self.toggle_pin(path, line),
            IndexCmd::MovePin { path, line, up } => self.move_pin(path, line, up),
            IndexCmd::Stats { resp } => {
                let _ = resp.send(self.stats());
            }
        }
    }

    /// Parse + index one absolute path (or remove its rows when deleted).
    fn index_changed(&mut self, abs: &Path) -> Result<()> {
        let rel = index::rel_path(&self.notes_dir, abs);
        if !abs.exists() {
            self.db.delete_path(&rel)?;
            self.corpus.retain(|m| m.path != rel);
            self.retain_valid_pins();
            info!("index: removed {rel}");
            return Ok(());
        }
        let rows = self.rows_for_file(abs, &rel)?;
        self.db.replace_file(&rows)?;
        self.refresh_corpus_for(&rel, &rows);
        self.cold = false;
        debug!("index: {} ({} sections)", rel, rows.len());
        Ok(())
    }

    fn rows_for_file(&self, abs: &Path, rel: &str) -> Result<Vec<crate::db::SectionRow>> {
        let text =
            std::fs::read_to_string(abs).with_context(|| format!("reading {}", abs.display()))?;
        let stem = abs
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel.to_string());
        let parsed: Vec<ParsedSection> = index::parse_sections(&text, &stem, self.separator);
        let previous = self.db.mtime_carry(rel)?;
        let mtime = index::file_mtime_ms(abs);
        Ok(index::section_rows(
            rel, parsed, mtime, self.cold, &previous,
        ))
    }

    fn initial_import(&mut self) -> Result<()> {
        let files = index::walk_notes(&self.notes_dir).context("walking notes dir")?;
        let mut sections = 0usize;
        for abs in &files {
            let rel = index::rel_path(&self.notes_dir, abs);
            match self.rows_for_file(abs, &rel) {
                Ok(rows) => {
                    sections += rows.len();
                    if !rows.is_empty() {
                        self.db.replace_file(&rows)?;
                    }
                }
                Err(err) => warn!("indexing {}: {err:#}", abs.display()),
            }
        }
        self.cold = false;
        self.reload_corpus()?;
        info!(
            "index: {} sections across {} files (cold={})",
            sections,
            files.len(),
            self.cold
        );
        Ok(())
    }

    fn reindex(&mut self) -> Result<()> {
        self.db.reset().context("resetting index")?;
        self.cold = true;
        let started = std::time::Instant::now();
        let files = index::walk_notes(&self.notes_dir)?;
        let mut sections = 0usize;
        for abs in &files {
            let rel = index::rel_path(&self.notes_dir, abs);
            match self.rows_for_file(abs, &rel) {
                Ok(rows) => {
                    sections += rows.len();
                    if !rows.is_empty() {
                        self.db.replace_file(&rows)?;
                    }
                }
                Err(err) => warn!("indexing {}: {err:#}", abs.display()),
            }
        }
        self.cold = false;
        self.reload_corpus()?;
        info!(
            "reindexed {} sections across {} files in {:?}",
            sections,
            files.len(),
            started.elapsed()
        );
        Ok(())
    }

    fn reload_corpus(&mut self) -> Result<()> {
        self.corpus = self.db.all_meta()?;
        self.retain_valid_pins();
        Ok(())
    }

    fn refresh_corpus_for(&mut self, rel: &str, rows: &[crate::db::SectionRow]) {
        self.corpus.retain(|m| m.path != rel);
        self.corpus.extend(rows.iter().map(|r| SectionMeta {
            path: r.path.clone(),
            line: r.line,
            title: r.title.clone(),
            section_mtime: r.section_mtime,
        }));
        self.retain_valid_pins();
    }

    /// Drop pins whose (path, line) left the corpus (file removed, or an
    /// edit shifted heading lines) and persist the prune.
    fn retain_valid_pins(&mut self) {
        let before = self.pinned.len();
        self.pinned
            .retain(|(p, l)| self.corpus.iter().any(|m| &m.path == p && &m.line == l));
        if self.pinned.len() != before {
            if let Err(err) = self
                .db
                .write_pins(crate::db::DEFAULT_WORKSPACE, &self.pinned)
            {
                warn!("pruning pins: {err:#}");
            }
        }
    }

    /// Load pins from the DB at boot (order preserved), pruning stale keys.
    fn load_pins(&mut self) {
        match self.db.pins(crate::db::DEFAULT_WORKSPACE) {
            Ok(pins) => {
                self.pinned = pins;
                self.retain_valid_pins();
            }
            Err(err) => warn!("loading pins: {err:#}"),
        }
    }

    fn toggle_pin(&mut self, path: String, line: u32) {
        if let Some(pos) = self
            .pinned
            .iter()
            .position(|(p, l)| *p == path && *l == line)
        {
            self.pinned.remove(pos);
        } else {
            self.pinned.push((path, line));
        }
        if let Err(err) = self
            .db
            .write_pins(crate::db::DEFAULT_WORKSPACE, &self.pinned)
        {
            warn!("writing pins: {err:#}");
        }
    }

    fn move_pin(&mut self, path: String, line: u32, up: bool) {
        let pos = match self
            .pinned
            .iter()
            .position(|(p, l)| *p == path && *l == line)
        {
            Some(pos) => pos,
            None => return,
        };
        let delta: isize = if up { -1 } else { 1 };
        let target = (pos as isize + delta).clamp(0, self.pinned.len() as isize - 1) as usize;
        if target != pos {
            self.pinned.swap(pos, target);
            if let Err(err) = self
                .db
                .write_pins(crate::db::DEFAULT_WORKSPACE, &self.pinned)
            {
                warn!("writing pins: {err:#}");
            }
        }
    }

    fn stats(&self) -> String {
        let sections = self
            .db
            .section_count()
            .map(|n| n.to_string())
            .unwrap_or_else(|_| "?".into());
        let files = self
            .corpus
            .iter()
            .map(|m| m.path.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        format!(
            "running; {sections} sections in {files} files; notes={}",
            self.notes_dir.display()
        )
    }

    fn search(&self, query: &str) -> Vec<SearchHit> {
        let q = query.trim();
        let mut hits: HashMap<(String, u32), SearchHit> = HashMap::new();
        let push = |hit: SearchHit, hits: &mut HashMap<(String, u32), SearchHit>| {
            let key = (hit.path.clone(), hit.line);
            match hits.get_mut(&key) {
                Some(existing) if existing.score >= hit.score => {}
                _ => {
                    hits.insert(key, hit);
                }
            }
        };

        if q.is_empty() {
            for m in &self.corpus {
                push(
                    SearchHit {
                        path: m.path.clone(),
                        line: m.line,
                        title: m.title.clone(),
                        section_mtime: m.section_mtime,
                        score: 0,
                        pinned: false,
                    },
                    &mut hits,
                );
            }
        } else {
            for m in &self.corpus {
                if let Some(score) = fuzzy_score(q, &m.title) {
                    push(
                        SearchHit {
                            path: m.path.clone(),
                            line: m.line,
                            title: m.title.clone(),
                            section_mtime: m.section_mtime,
                            score: score + 200,
                            pinned: false,
                        },
                        &mut hits,
                    );
                } else if let Some(score) = fuzzy_score(q, &m.path) {
                    push(
                        SearchHit {
                            path: m.path.clone(),
                            line: m.line,
                            title: m.title.clone(),
                            section_mtime: m.section_mtime,
                            score: score + 100,
                            pinned: false,
                        },
                        &mut hits,
                    );
                }
            }
            match self.db.content_search(q, SEARCH_LIMIT) {
                Ok(metas) => {
                    for m in metas {
                        push(
                            SearchHit {
                                path: m.path,
                                line: m.line,
                                title: m.title,
                                section_mtime: m.section_mtime,
                                score: 50,
                                pinned: false,
                            },
                            &mut hits,
                        );
                    }
                }
                Err(err) => warn!("content search: {err:#}"),
            }
        }

        // Pinned sections first (user order, always visible — even when a
        // query wouldn't surface them), then the scored hits.
        let rest: Vec<SearchHit> = hits
            .into_values()
            .filter(|h| !self.is_pinned(&h.path, h.line))
            .collect();
        assemble_hits(&self.pinned, &self.corpus, rest, SEARCH_LIMIT as usize)
    }

    fn is_pinned(&self, path: &str, line: u32) -> bool {
        self.pinned.iter().any(|(p, l)| p == path && *l == line)
    }
}

/// Pinned sections first (user order; stale keys skipped), then the scored
/// hits in score → mtime → path → line order. The cap applies to the tail
/// only, so pins always surface even in a full list.
fn assemble_hits(
    pinned: &[(String, u32)],
    corpus: &[SectionMeta],
    mut rest: Vec<SearchHit>,
    limit: usize,
) -> Vec<SearchHit> {
    let mut out: Vec<SearchHit> = Vec::new();
    for (path, line) in pinned {
        if let Some(m) = corpus.iter().find(|m| &m.path == path && &m.line == line) {
            out.push(SearchHit {
                path: m.path.clone(),
                line: m.line,
                title: m.title.clone(),
                section_mtime: m.section_mtime,
                score: 0,
                pinned: true,
            });
        }
    }
    rest.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.section_mtime.cmp(&a.section_mtime))
            .then(a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    rest.truncate(limit.saturating_sub(out.len()));
    out.extend(rest);
    out
}

/// Greedy subsequence match with bonuses for consecutive and word-start
/// hits, normalized by text length. `None` = not a subsequence.
fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    if q.is_empty() || q.len() > t.len() {
        return None;
    }
    let mut score = 0i64;
    let mut ti = 0usize;
    let mut prev: Option<usize> = None;
    for &qc in &q {
        let mut hit = None;
        while ti < t.len() {
            if t[ti] == qc {
                hit = Some(ti);
                ti += 1;
                break;
            }
            ti += 1;
        }
        let hi = hit?;
        score += 1;
        if prev == Some(hi.wrapping_sub(1)) {
            score += 2; // consecutive run
        }
        if hi == 0 || !t[hi - 1].is_alphanumeric() {
            score += 4; // start of a word
        }
        prev = Some(hi);
    }
    // Tighter (shorter) texts score higher for the same match shape.
    Some(score * 100 / (t.len() as i64 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(path: &str, line: u32) -> SectionMeta {
        SectionMeta {
            path: path.into(),
            line,
            title: path.into(),
            section_mtime: 0,
        }
    }

    fn hit(path: &str, line: u32, score: i64) -> SearchHit {
        SearchHit {
            path: path.into(),
            line,
            title: path.into(),
            section_mtime: 0,
            score,
            pinned: false,
        }
    }

    #[test]
    fn pins_lead_in_user_order_and_cap_spares_them() {
        let corpus = vec![meta("a.md", 1), meta("b.md", 5), meta("c.md", 9)];
        // "gone.md" left the corpus: silently skipped.
        let pinned = vec![
            ("c.md".to_string(), 9),
            ("a.md".to_string(), 1),
            ("gone.md".to_string(), 3),
        ];
        let rest = vec![hit("b.md", 5, 10), hit("a.md", 3, 99)];

        let out = assemble_hits(&pinned, &corpus, rest.clone(), 100);
        assert_eq!(out.len(), 4);
        assert_eq!((out[0].path.as_str(), out[0].line), ("c.md", 9));
        assert!(out[0].pinned);
        // User order, not score order.
        assert_eq!((out[1].path.as_str(), out[1].line), ("a.md", 1));
        assert!(out[1].pinned);
        // Tail sorted by score.
        assert_eq!((out[2].path.as_str(), out[2].line), ("a.md", 3));
        assert!(!out[2].pinned);
        assert_eq!(out[3].path, "b.md");

        // Cap applies to the tail only — pins always surface.
        let out = assemble_hits(&pinned, &corpus, rest, 3);
        assert_eq!(out.len(), 3);
        assert!(out[0].pinned && out[1].pinned);
        assert_eq!(out[2].path, "a.md");
    }

    #[test]
    fn fuzzy_matches_subsequences_and_rewards_words() {
        assert!(fuzzy_score("upadd", "upperadd").is_some());
        assert!(fuzzy_score("ua", "wayland-notes/ua.md").is_some());
        assert!(fuzzy_score("xyz", "upperadd").is_none());
        // Word-start hits beat scattered ones even in a longer text.
        let word_start = fuzzy_score("no", "the note").unwrap();
        let scattered = fuzzy_score("no", "nnnnnoooooooo").unwrap();
        assert!(word_start > scattered);
        assert!(fuzzy_score("", "anything").is_none());
    }
}
