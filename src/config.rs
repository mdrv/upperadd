use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Parsed `~/.config/upperadd/config.toml`.
///
/// Missing file = all defaults; invalid file = hard error (the daemon
/// refuses to boot with a broken config, visible in journald); unknown
/// keys are ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub notes: Notes,
    pub window: Window,
    pub sections: Sections,
    pub editor: Editor,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notes: Notes::default(),
            window: Window::default(),
            sections: Sections::default(),
            editor: Editor::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Notes {
    pub dir: PathBuf,
}

impl Default for Notes {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("/m"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Window {
    /// left panel background opacity (0–1)
    pub panel_alpha: f32,
    /// right preview panel background opacity (0–1)
    pub preview_alpha: f32,
    pub corner_radius: f32,
    /// of the resolved output
    pub width_fraction: f32,
    pub height_fraction: f32,
    /// where the panel docks inside the output
    pub anchor: Anchor,
    /// px gap between panel and output edges
    pub margin: f32,
    pub output: OutputSpec,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            panel_alpha: 0.92,
            preview_alpha: 0.80,
            corner_radius: 16.0,
            width_fraction: 0.7,
            height_fraction: 0.7,
            anchor: Anchor::BottomLeft,
            margin: 16.0,
            output: OutputSpec::default(),
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    #[default]
    BottomLeft,
    Bottom,
    BottomRight,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(untagged)]
pub enum OutputSpec {
    Named(NamedOutput),
    Index(usize),
}

impl Default for OutputSpec {
    fn default() -> Self {
        Self::Named(NamedOutput::Cursor)
    }
}

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NamedOutput {
    #[default]
    Cursor,
    Primary,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Sections {
    /// "h1" (default) or "hr" (also split on lone blank-line-delimited ---)
    pub separator: Separator,
}

impl Default for Sections {
    fn default() -> Self {
        Self {
            separator: Separator::H1,
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Separator {
    #[default]
    H1,
    Hr,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Editor {
    pub command: String,
    /// `{file}` / `{line}` placeholders
    pub args: Vec<String>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            command: "neovide".into(),
            args: vec!["+{line}".into(), "{file}".into()],
        }
    }
}

pub fn default_path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("upperadd").join("config.toml")
}

pub fn load() -> anyhow::Result<Config> {
    load_from(&default_path())
}

pub fn load_from(path: &Path) -> anyhow::Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(anyhow::anyhow!("reading {}: {e}", path.display())),
    };
    toml::from_str(&text).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = load_from(Path::new("/nonexistent/upperadd/config.toml")).unwrap();
        assert_eq!(cfg.notes.dir, PathBuf::from("/m"));
        assert_eq!(cfg.window.anchor, Anchor::BottomLeft);
        assert_eq!(cfg.window.output, OutputSpec::Named(NamedOutput::Cursor));
        assert_eq!(cfg.sections.separator, Separator::H1);
        assert_eq!(cfg.editor.command, "neovide");
        assert_eq!(cfg.editor.args, vec!["+{line}", "{file}"]);
    }

    #[test]
    fn overrides_apply_and_unknown_keys_are_ignored() {
        let dir = std::env::temp_dir().join(format!("upperadd-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[notes]\ndir = \"/m/notes\"\nbogus_key = true\n[window]\nanchor = \"top-right\"\nmargin = 8.0\n",
        )
        .unwrap();
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg.notes.dir, PathBuf::from("/m/notes"));
        assert_eq!(cfg.window.anchor, Anchor::TopRight);
        assert_eq!(cfg.window.margin, 8.0);
        assert_eq!(cfg.window.panel_alpha, 0.92); // default preserved
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_toml_is_a_hard_error() {
        let dir = std::env::temp_dir().join(format!("upperadd-config-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[window\nanchor = 3\n").unwrap();
        assert!(load_from(&path).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
