# upperadd v0.1.0 — agent guide

GUI markdown note vault: a toggleable gpui-ce layer-shell overlay that
searches `/m` and previews notes, backed by mdrv-db for index/metadata.
Spec: [specs/00-v0.1.0-spec.md](specs/00-v0.1.0-spec.md). Kickoff prompt:
[PROMPT.md](PROMPT.md).

## Status

Spec-only. No code yet. Target: single crate `upperadd` (lib + bin),
edition 2024, MSRV 1.85, Linux/Wayland only. v0.1.0 shares NOTHING with
the v0.0.2 CLI (`/g/upperadd-w20260715`, SurrealDB) — no code or schema
carryover.

## Toolchain & dependencies (copy-paste, load-bearing)

```toml
[dependencies]
gpui = { path = "/g/gpui-ce/crates/gpui", package = "mdrv-gpui-ce" }
gpui_platform = { path = "/g/gpui-ce/crates/gpui_platform", package = "mdrv-gpui-platform", features = [
	"wayland",
] }
mdrv-db = "0.5" # engine crate; path override below for local dev

[patch.crates-io]
arrayref = { path = "/g/gpui-ce/vendor/arrayref" } # crates.io arrayref is yanked/broken (via tiny-skia)
```

- **Portable (no `/g/` pin) alternative — git tag** (fork policy, see
  `/g/gpui-ce/MDRV.md` § Registry): swap the two `path =` dep lines for
  `git = "https://github.com/mdrv/gpui-ce", tag = "mdrv-gpui-0.0.260925"`
  (package keys stay) and the arrayref patch for
  `arrayref = { git = "https://github.com/mdrv/gpui-ce" }`. Use path form
  for local dev; tag form when reproducibility away from this machine
  matters. New fork release ⇒ new `mdrv-gpui-0.0.<version>` tag.

- The fork is consumed from the **working tree** (`/g/gpui-ce` on `main`,
  must always build). Same convention as mdrv-ds-{clock,legend,shell,overlay,launcher}.
- Package renames are NOT optional: crates.io names are `gpui-ce` /
  `gpui_ce_platform`; lib names stay `gpui` / `gpui_platform`.
- Read `/x/m/v270/gpui-ce/gpui-ce.md` (practical fork API docs) BEFORE
  writing any gpui code. §6 = layer-shell, §19 = z-order, §20 =
  gamepad-driven panel pattern, §28 = animation-freeze known issue.
- mdrv-db docs: `/x/m/v270/mdrv-db/` (01-architecture, 02-engine-api —
  TS-flavored but ops shapes are the serde model the Rust engine uses).

## Fork-specific rules (verified the hard way elsewhere)

1. `set_keyboard_interactivity` at runtime is a **fork-only patch** —
   upstream only sets it at surface creation. It is load-bearing here:
   exclusive while shown, OnDemand (or drop) before spawning the editor.
2. Layer = **Top**, never `Overlay` (§19: Overlay z-order breaks under
   fullscreen windows).
3. Transparency: `WindowBackgroundAppearance::Transparent`; paint your
   own rounded/translucent panels (alpha in Hsla) — the surface itself
   is fully transparent.
4. Avoid `with_animation` opacity fades (§28 freeze). Animate via
   geometry (position/size) only until the style-transitions API is
   merged and verified.
5. Images in preview: `img()` with absolute paths resolved from the
   note's directory at render time.

## mdrv-db rules

- One slug: `upperadd`, data dir `/x/db/upperadd`, engine name
  `upperadd`. Register in `~/.config/mdrv-db/config.toml` fleet table
  (daemon/CLI read it; **upperadd never reads that file**).
- upperadd's daemon is the **single writer** (fjall file lock). Markdown
  files under `/m` are the content source of truth; the DB holds only
  metadata + the search index, and `ua reindex` must rebuild it from
  scratch (drop keyspaces/tables, re-walk).
- After schema-affecting changes: `mdrv-db verify` + `ua reindex` must
  both pass. Never hand-edit `/x/db/upperadd/live/`.

## Service & CLI conventions

- systemd user unit `upperadd.service` (mirror
  `~/.config/systemd/user/mdrv-ds-overlay.service`):
  `ExecStart=<dir>/target/release/upperadd daemon start --foreground`,
  `Restart=on-failure`, `WantedBy=default.target`, enabled at login.
- CLI verbs over `$XDG_RUNTIME_DIR/upperadd.sock`:
  `toggle` | `show` | `stop` | `status` | `reindex` (bare `ua` prints help).
- Hyprland: `bind = <key>, exec, ua toggle` + `layerrule blur, upperadd`
  documented in README (blur is compositor-side, not app-side).

## Verification (green before every commit)

    cargo check && cargo test
    cargo build --release
    # manual round-trip (interactive):
    systemctl --user restart upperadd && ua toggle   # overlay appears
    ua stop                                           # clean exit, lock released

Editor handoff test: select note → Enter → `neovide +<line> <file>`
opens focused; edit; save; `ua toggle` shows updated preview without
manual reindex (fs-watch did it).

## House style

- Conventional commits (`feat:`, `fix:`, `docs:`, `chore:`), small and
  frequent; commit only with checks green.
- `specs/` = numbered design docs (bake into binary via `include_str!`
  only if actually used at runtime). `docs/` = findings, gotchas,
  post-mortems — write one the first time a tool lies to you.
- Minimal diffs; no speculative abstraction; no new deps without a
  sentence of justification in the commit body.

## Out of scope for v0.1.0 (do not build)

In-app editing (opt-in later), Windows, wikilinks/footnotes, themes,
tags UI, sync, old-CLI features (memos, nushell completions, SurrealDB
anything).
