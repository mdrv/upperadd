# upperadd

A Wayland layer-shell overlay for jumping into your markdown notes:
fzf-style search over `/m`, live markdown preview, one-keystroke handoff to
your editor — plus draggable, resizable **sticky notes** pinned above your
windows.

- **Instant**: summoned by a global hotkey, exclusive keyboard while shown,
  hidden without losing your query or selection
- **Section-level indexing**: every `# ` heading is a search result; the
  index lives in an embedded [mdrv-db](https://github.com/mdrv/mdrv-db)
  engine and rebuilds itself from the files (they are the source of truth)
- **Live preview**: GFM markdown rendered in the right pane, images resolved
  relative to the note, fs-watch keeps it fresh after every editor save
- **Editor handoff**: `Enter` opens the selected section in your editor
  (default `neovide +<line> <file>`); saves show up without a manual reindex
- **Stickies**: pin a section into an always-on-top, draggable/resizable
  read-only window with live content
- **Pins**: pin results to the top of the list, reorder them, persist
  across restarts as workspace state

Linux/Wayland only (v0.1). Built with the
[mdrv gpui-ce fork](https://github.com/mdrv/gpui-ce).

## Install

Arch Linux — binary packages via the [mdrv pacman
repo](https://github.com/mdrv/alarm):

```sh
pacman -S upperadd
```

Or grab a tarball from
[Releases](https://github.com/mdrv/upperadd/releases) (x86_64 + aarch64) and
install `upperadd` + `upperadd.service` by hand.

### From source

```sh
git clone https://github.com/mdrv/upperadd
cd upperadd && cargo build --release
```

## Usage

```sh
systemctl --user enable --now upperadd   # the daemon owns the overlay

ua toggle    # show/hide (default verb when the daemon is running)
ua status    # daemon + index health
ua reindex   # full rebuild from the notes dir
ua stick-test
ua stop
```

### Overlay keys

| Insert mode (default)        | Normal mode (`Esc`)          |
| ---------------------------- | ---------------------------- |
| type to fuzzy-filter         | `j`/`k` or arrows move       |
| `↑`/`↓` move selection       | `P`/`Tab` pin to top of list |
| `Alt+↑`/`Alt+↓` reorder pins | `Alt+J`/`Alt+K` reorder pins |
| `Enter` open in editor       | `S` extract sticky           |
| `Esc` → normal mode          | `i` insert · `Enter` editor  |
|                              | `⇧R` reindex · `Esc` hide    |

Sticky notes: drag by header/footer, resize from all edges **and corners**,
`↻` re-reads the section, `✕` closes.

### Hyprland

```ini
bind = $mainMod, N, exec, ua toggle
layerrule = blur, upperadd
layerrule = blur, upperadd-sticky
```

## Configuration

`~/.config/upperadd/config.toml` (all keys optional, missing file = defaults):

```toml
[notes]
dir = "/m"

[window] # panel geometry & look
panel_alpha = 0.92
preview_alpha = 0.80
corner_radius = 16.0
width_fraction = 0.7
height_fraction = 0.7
anchor = "bottom-left" # nine-way kebab
margin = 16.0
output = "cursor" # "cursor" | "primary" | monitor index

[sections]
separator = "h1" # "h1" | "hr"

[editor]
command = "neovide"
args = ["+{line}", "{file}"]

[fonts]
body = "IBM Plex Sans"
heading = "Fira Sans"
monospace = "JetBrains Mono"
```

The database (metadata + search index, derived from the files) lives in
`/x/db/upperadd` — never edit it by hand; `ua reindex` rebuilds it.

## License

[MIT](LICENSE)
