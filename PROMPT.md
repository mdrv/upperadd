# upperadd v0.1.0 — kickoff prompt

You are implementing **upperadd v0.1.0** in this repo, from scratch.

Read, in order, before writing any code:

1. `AGENTS.md` — dependency block (exact, copy it), fork-specific rules,
   mdrv-db rules, verification gates, house style, out-of-scope list.
2. `specs/00-v0.1.0-spec.md` — the decided design. It is the contract;
   deviate only after asking.
3. `/x/m/v270/gpui-ce/10-practical-api.md` — practical gpui-ce fork docs (§6
   layer-shell, §19 z-order, §28 animation freeze).
4. `/x/m/v270/mdrv-db/01-architecture.md` + `02-engine-api.md` — the DB
   you embed (Rust engine crate, TS-flavored API docs but same ops model).

Build in milestone order (spec §Milestones): M1 shell → M2 index+search →
M3 preview → M4 editor+polish. Each milestone ends with the AGENTS.md
verification green and a short commit. Do not start M(n+1) with M(n) red.

Hard rules:

- The gpui-ce fork is consumed from `/g/gpui-ce` working tree (path
  deps, package renames as given in AGENTS.md). NEVER modify `/g/gpui-ce`
  or `/g/mdrv-db` from this repo — if you hit a fork bug, stop and
  report it instead of patching.
- Never touch `/x/db/upperadd/live/` by hand; all writes go through the
  engine. `ua reindex` must be able to rebuild everything from `/m`.
- No SurrealDB, no editing in-app, no Windows — v0.1.0 scope only
  (spec §Out of scope).
- OPEN items in the spec are questions for the owner, not decisions for
  you to bake in silently: pick the simplest default that unblocks the
  milestone, note it, and surface it.

First task: `git status` (should be spec files only), scaffold the
crate exactly per AGENTS.md's dep block, get `cargo check` green with a
`main.rs` that prints its verbs, then start M1.
