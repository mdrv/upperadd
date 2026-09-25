//! Notes-dir walking and markdown section parsing (spec 00: split on H1
//! `^#\s`, frontmatter never splits, pre-heading region is titled by the
//! file stem; opt-in `hr` separator = a lone `---`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::config::Separator;
use crate::db::SectionRow;

pub struct ParsedSection {
    /// 1-based line of the heading (1 for the pre-heading region).
    pub line: u32,
    pub title: String,
    /// Body without the heading line, trimmed.
    pub content: String,
}

/// Split a note into sections per the configured separator.
pub fn parse_sections(text: &str, stem: &str, separator: Separator) -> Vec<ParsedSection> {
    let lines: Vec<&str> = text.lines().collect();

    // Frontmatter: only when line 1 is exactly `---`/`+++` (and it closes).
    // It is metadata — never a separator (spec 00).
    let mut start = 0usize;
    if let Some(first) = lines.first() {
        let fence = first.trim_end();
        if fence == "---" || fence == "+++" {
            if let Some(idx) = lines.iter().skip(1).position(|l| l.trim_end() == fence) {
                start = idx + 2; // first line after the closing fence
            }
        }
    }

    let mut sections = Vec::new();
    // (line, title, body, is_heading) — an empty heading section still
    // exists (title-only search); an empty pre-heading region does not.
    let mut cur: Option<(u32, String, Vec<&str>, bool)> = None;
    let mut last_line = String::new(); // previous file line, for hr detection

    let mut flush = |cur: &mut Option<(u32, String, Vec<&str>, bool)>| {
        if let Some((line, title, body, heading)) = cur.take() {
            let content = body.join("\n").trim().to_string();
            if heading || !content.is_empty() {
                sections.push(ParsedSection {
                    line,
                    title,
                    content,
                });
            }
        }
    };

    for (i, raw) in lines.iter().enumerate().skip(start) {
        let lineno = (i + 1) as u32;
        let trimmed_end = raw.trim_end();

        // Split triggers.
        let on_h1 = matches!(separator, Separator::H1) && raw.starts_with("# ");
        // A lone `---` is an hr only when it follows a blank line — directly
        // after text it is a setext H2 underline, not a separator.
        let on_hr = matches!(separator, Separator::Hr)
            && trimmed_end == "---"
            && (i == start || last_line.trim().is_empty());
        if on_h1 || on_hr {
            flush(&mut cur);
            let title = if on_h1 { raw[2..].trim() } else { stem };
            cur = Some((lineno, title.to_string(), Vec::new(), true));
            last_line = trimmed_end.to_string();
            continue;
        }

        match &mut cur {
            Some((_, _, body, _)) => body.push(raw),
            None => {
                // Pre-heading region starts at the first content line.
                cur = Some((lineno, stem.to_string(), vec![raw], false));
            }
        }
        last_line = trimmed_end.to_string();
    }
    flush(&mut cur);
    sections
}

/// All `*.md` files under `dir` (recursive, hidden entries skipped), sorted.
pub fn walk_notes(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if name.ends_with(".md") {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

pub fn file_mtime_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Notes-dir-relative, `/`-separated path.
pub fn rel_path(notes_dir: &Path, abs: &Path) -> String {
    abs.strip_prefix(notes_dir)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Turn parsed sections into DB rows, deriving per-section mtimes:
/// unchanged hashes carry the old `section_mtime`, edits get `now_ms`,
/// and a cold rebuild (empty table) falls back to the file mtime.
pub fn section_rows(
    path: &str,
    parsed: Vec<ParsedSection>,
    file_mtime_ms: i64,
    cold: bool,
    previous: &std::collections::HashMap<String, i64>,
) -> Vec<SectionRow> {
    parsed
        .into_iter()
        .map(|s| {
            let hash = blake3::hash(s.content.as_bytes()).to_hex().to_string();
            let section_mtime =
                previous
                    .get(&hash)
                    .copied()
                    .unwrap_or(if cold { file_mtime_ms } else { now_ms() });
            SectionRow {
                path: path.to_string(),
                line: s.line,
                title: s.title,
                section_mtime,
                hash,
                content: s.content,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const STEM: &str = "my-note";

    #[test]
    fn splits_on_h1_and_titles_preheading_by_stem() {
        let text = "intro stuff\n\n# One\n\none body\n\n## Sub\n\nstill one\n\n# Two\n\ntwo body\n";
        let sections = parse_sections(text, STEM, Separator::H1);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].title, "my-note");
        assert_eq!(sections[0].line, 1);
        assert_eq!(sections[0].content, "intro stuff");
        assert_eq!(sections[1].title, "One");
        assert_eq!(sections[1].line, 3);
        assert_eq!(sections[1].content, "one body\n\n## Sub\n\nstill one");
        assert_eq!(sections[2].title, "Two");
        assert!(sections[2].content.contains("two body"));
    }

    #[test]
    fn frontmatter_never_splits_and_is_dropped() {
        let text = "---\ntitle: x\n---\n\n# Real\n\nbody\n";
        let sections = parse_sections(text, STEM, Separator::H1);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].title, "Real");
        assert_eq!(sections[0].line, 5);
    }

    #[test]
    fn unterminated_frontmatter_is_plain_text() {
        let text = "---\nno closing fence\n\n# Real\n\nbody\n";
        let sections = parse_sections(text, STEM, Separator::H1);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, STEM);
        assert_eq!(sections[1].title, "Real");
    }

    #[test]
    fn hr_separator_splits_but_setext_underline_does_not() {
        let text = "alpha\n\n---\n\nbeta\ntext\n---\n\ngamma\n";
        let sections = parse_sections(text, STEM, Separator::Hr);
        // `text\n---` is a setext H2 — not a separator.
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].content, "alpha");
        assert!(sections[1].content.contains("beta"));
        assert!(sections[1].content.contains("gamma"));
    }

    #[test]
    fn h1_mode_ignores_lone_dashes() {
        let text = "alpha\n\n---\n\nbeta\n";
        let sections = parse_sections(text, STEM, Separator::H1);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].content.contains("beta"));
    }

    #[test]
    fn rows_carry_mtime_for_unchanged_hashes() {
        let parsed = vec![
            ParsedSection {
                line: 1,
                title: "a".into(),
                content: "one".into(),
            },
            ParsedSection {
                line: 3,
                title: "b".into(),
                content: "two".into(),
            },
        ];
        // Cold rebuild (empty table): sections fall back to the file mtime.
        let first = section_rows("p.md", parsed, 42, true, &HashMap::new());
        assert!(first.iter().all(|r| r.section_mtime == 42));

        // Second pass: unchanged hash carries its mtime, edits get now.
        let previous: HashMap<String, i64> = first.iter().map(|r| (r.hash.clone(), 111)).collect();
        let reparsed = vec![
            ParsedSection {
                line: 1,
                title: "a".into(),
                content: "one".into(),
            },
            ParsedSection {
                line: 3,
                title: "b".into(),
                content: "CHANGED".into(),
            },
        ];
        let second = section_rows("p.md", reparsed, 42, false, &previous);
        assert_eq!(second[0].section_mtime, 111); // carried
        assert_ne!(second[1].section_mtime, 111); // fresh edit
    }
}
