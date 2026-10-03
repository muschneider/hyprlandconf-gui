<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Architecture

hyprconf is a Cargo workspace with a hard split between a UI-free core library
and a thin Iced front-end. This document explains that split, the central model,
and where the schema comes from.

```
┌─────────────────────────────────────────────────────────────┐
│ hyprconf-gui  (bin, iced 0.14 — The Elm Architecture)         │
│                                                               │
│  App state ── update(Message) ──▶ Task        view(&App) ──▶ UI│
│     │                                   ▲                      │
│     │ edit.rs / save.rs / load.rs / color_picker.rs / …        │
│     ▼                                                          │
├─────────────────────────────────────────────────────────────┤
│ hyprconf-core  (lib, no iced)                                 │
│                                                               │
│  conf ⇄  ┌────────────┐  ⇄ lua                                │
│  parser  │  Config     │   parser     schema (data-driven)    │
│  serial. │ (the model) │   serial.    validate · fs · hyprctl │
│          └────────────┘                                       │
└─────────────────────────────────────────────────────────────┘
```

## Two crates, one rule

| Crate           | Kind | Responsibility                                                            |
| --------------- | ---- | ------------------------------------------------------------------------- |
| `hyprconf-core` | lib  | Format-agnostic model, schema, both parsers/serializers, validation, atomic FS, `hyprctl`. **No GUI dependency.** |
| `hyprconf-gui`  | bin  | Iced 0.14 desktop front-end. Owns no config knowledge beyond the schema.  |

The rule: **`hyprconf-core` never depends on `iced`.** Everything interesting —
parsing, the model, conversions, validation, save planning primitives — is
therefore unit-testable without a window or GPU. The GUI is a presentation layer
that drives the core.

## The model (`core::model`)

`Config` is the format-agnostic heart:

- **Scalar options** in an insertion-ordered `IndexMap<String, Tracked<Value>>`,
  keyed by `conf`-style path (`decoration:blur:size`).
- **Ordered `Vec`s** for every structured collection (keybinds, window/layer
  rules, monitors, workspaces, env, exec, beziers, animations, submaps, vars,
  gestures, devices, permissions, plugins).
- `Value` is the scalar sum type (`Bool`/`Int`/`Float`/`Color`/`Gradient`/
  `String`/`Enum`/`Vec2`/`CssGap`) with Hyprland text (de)serialization in
  `core::value`.
- `Tracked<T>` carries `Provenance` (source file, line span, leading/trailing
  comments) so edits can be surgical and round-trips can preserve formatting.

Reading **either** format produces the same `Config`; the output format is an
independent choice at save time. This is the abstraction the whole app is built
around.

## Parsers & serializers

Both formats use a **two-layer** design: a *lossless document* (the source of
truth for byte-faithful round-trips) plus a *semantic projection* into `Config`.

- **conf** (`core::conf`): `ConfDocument` keeps every physical line verbatim;
  `ConfParser` resolves `section { … }` nesting into `:`-paths, follows
  `source=` includes (relative/abs/glob, cycle/missing detected as typed
  errors), expands `$variables`. `ConfSerializer`/`config_to_conf` write back —
  `set_option` edits a single value in place, preserving the rest.
- **lua** (`core::lua`): parsed *losslessly* with [`full_moon`] (a CST parser) —
  **the config is never executed.** Only the declarative `hl.*` subset is
  interpreted into `Config`; anything dynamic (loops, functions, closures) is
  preserved verbatim and flagged as a `DynamicRegion`. `LuaSerializer` emits
  fresh Lua against the `hl` API.

Nothing is dropped on read: unknown keys / unparseable values are kept (as
`String`) and surfaced as warnings.

### Why `.conf` → Lua is not a syntax change

The two formats do not just spell the same things differently — Lua's bindings
are **typed and strict**, and the mapping is genuinely semantic. Two sub-modules
exist purely because of that, and both are bidirectional:

- **`lua::dispatch`** maps hyprlang dispatcher names onto `hl.dsp.*`
  *constructors and their argument shapes*: `killactive` →
  `hl.dsp.window.close()`, `resizeactive 10 0` →
  `hl.dsp.window.resize({ x = 10, y = 0 })`. A string dispatcher is rejected at
  load, and there is no dispatch-by-name escape hatch, so the table has to be
  complete; anything genuinely unknown falls back to a `hyprctl dispatch` shim
  and is reported.
- **`lua::rules`** maps rule text onto typed table fields: `opacity 0.9 0.9` →
  `opacity = "0.9 0.9"`. This one matters most because getting it wrong is
  *silent* — a rule left in `name` loads without complaint and simply never
  applies.

Anything that cannot be expressed exactly becomes a `LuaNote` rather than a
dropped line, which is what the migration UI shows the user before writing.

## Verifying against the real compositor (`core::verify`)

Unit tests can only assert that the output matches what we *believe* the API to
be — which is exactly how the original conversion bugs survived. `core::verify`
wraps `Hyprland --verify-config`, so the compositor's own parser is the arbiter:

- `verify_text` writes the generated config to a temp file and returns a
  `Verdict` (`Ok` / `Problems` / `Unavailable`, never conflating the last two
  with success).
- Top-level `hl.exec_cmd` calls are commented out first, because
  `--verify-config` really does execute them.
- `tests/hyprland_verify.rs` runs conversions through it and skips itself when no
  Hyprland is installed, so CI stays green.

This is also what powers the migration flow's **Check** step.

## Schema & its provenance (`core::schema`, `meta/`)

The option surface is a **data-driven** table of `OptionSpec`s (path, label,
description, `ValueType`, default, range, optional `since`) grouped into
`Section`s and `CollectionSpec`s. It is built as compile-time `const` data in
`schema/data.rs`, so every default is type-checked by the compiler and
`Schema::load()` is infallible.

Option **keys** are verified against vendored upstream data in `meta/`:
`hl.meta.lua` is Hyprland's autogenerated Lua stub (0.56.1), and
`hyprland-config-keys.txt` is its flat `HL.ConfigKey` list. The test
`schema::tests::every_option_path_exists_in_vendored_stub` maps each schema path
(`:` → `.`) and asserts membership, so the schema can never reference an option
the shipped Hyprland doesn't know.

`hyprland-descriptions.json` is the compositor's own `hyprctl descriptions -j`
(minus the machine-specific current values). Three tests hold the schema to it:
**coverage** (every described option is in the schema and vice versa), **value
maps** (an option Hyprland describes with `0 = disabled, 1 = …` must be an enum
over exactly those values) and **defaults**. Its `min`/`max` become UI
*slider hints* (`OptionSpec::slider`, generated into `schema/sliders.rs` by
`meta/gen_sliders.py`) — deliberately *not* validation limits, because
Hyprland accepts and keeps values beyond them. Hard limits stay in
`OptionSpec::range`. See [`meta/README.md`](meta/README.md).

Two schema features exist because Hyprland's surface is not purely
enumerable:

- **Open choices** — `OptionSpec::suggestions` on a `String` option
  (`general:layout`, font weights): the UI offers the list, but custom values
  (`lua:my-layout`) stay valid, which a strict `Enum` would reject.
- **`ValueType::CssGap`** — `gaps_in`/`gaps_out`/`float_gaps` take one to four
  sides (CSS shorthand in `.conf`, an integer or `{ top, right, bottom, left }`
  table in Lua, which rejects the string form).

### Canonical paths and `.conf` spellings

Options are keyed by their **canonical** path: the stub spelling with `:`
separators. A few options are spelled differently in hyprlang
(`tap-to-click`, `tap-and-drag`, `input-capture`) and Hyprland is strict in
both directions — `.conf` rejects `tap_to_click`, Lua rejects `tap-to-click`.
`schema::canonical_path` is applied by every reader and `schema::conf_path` by
every `.conf` writer (and by live `hyprctl keyword`), so the model never sees
two spellings of one option.

## Validation, FS, hyprctl (`core::{validate,fs,hyprctl}`)

- `validate_config` checks set values against the schema (out-of-range/invalid =
  error, unknown = warning); `unsupported_options` flags options newer than the
  running Hyprland.
- `fs` provides `atomic_write` (temp file → `fsync` → rename) and
  `backup_existing` (timestamped `.bak`), composed as `save_atomically`.
- `hyprctl` wraps the CLI (`detect`, `apply_option`, `apply_monitor`, `eval`,
  `reload`) and degrades gracefully when Hyprland isn't running. A session
  running a Lua config refuses `hyprctl keyword` — with exit status 0 — so
  replies are judged by their *text*, and a refusal falls back to
  `hyprctl eval` with the equivalent `hl.config` / `hl.monitor` call.

## The GUI (`hyprconf-gui`)

Standard Iced 0.14 functional application
(`iced::application(boot, update, view)`):

- **`App`** holds the `LoadState`, selection/search, undo/redo stacks, settings,
  detected Hyprland, and the open color picker. **`Message`** enumerates every
  event; **`update`** is the single mutation point and returns `Task`s for
  off-thread work (load, `hyprctl`).

### `view` renders; it does not compute

`view` runs on every frame, so anything costlier than a field read is a
*derived cache* on `App`, recomputed in `update` when its inputs change:

| Cache              | Rebuilt when                     | What it replaces per frame        |
| ------------------ | -------------------------------- | --------------------------------- |
| `hits`             | the search query changes         | a fuzzy scan of the whole schema  |
| `profiles`         | the panel opens / a profile saves| a `read_dir` on the render path   |
| `stale_options`    | config or Hyprland version changes | a schema-wide version check     |
| `dirty_by_section` | a scalar edit / undo / load      | a schema-wide dirty-set scan      |
| `save_preview`     | the model or output format changes | plan + validation + per-file diff|

The same rule drives the render signatures: panes take `&'a App` / `&'a Loaded`
and return `Element<'a, Message>`, so labels, descriptions and option paths are
**borrowed** from the `'static` schema rather than cloned into owned `String`s on
every frame. Only strings that must travel inside a `Message` are allocated.
- **`load.rs`** locates and parses the config off the UI thread.
- **`edit.rs`** is the UI-free editing engine: applies `EditAction`/
  `CollectionAction` to the model, tracks per-field drafts/errors and dirty
  state, and provides undo snapshots. This is where most GUI unit tests live.
- **`save.rs`** computes a reviewable `SavePlan` (Preserve vs Regenerate),
  multi-file aware, and performs atomic writes. The whole `SavePreview`
  (plan + validation + per-file diffs) is built in `update`, not `view`, so
  rendering stays free of filesystem I/O and diffing. Settings-only edits are
  preserved in **both** formats: `.conf` documents are edited in place, and a
  hand-written Lua file gets a fenced `hl.config` block appended at its end
  (`MANAGED_BEGIN`/`MANAGED_END`). Hyprland applies `hl.config` calls in
  order, so the block overrides the hand-written part while every other byte
  — loops, functions, comments — survives; later saves rewrite only the
  block and carry over what it already held. Regenerating a Lua file that
  contains code is a save-blocking warning (`plan_problems`) until the user
  explicitly accepts the loss.
- **`migrate.rs`** owns the `.conf` → Lua flow: it generates the Lua up front,
  collects the `LuaNote`s, and tracks the four steps (Review → Preview → Check →
  Apply). The Check step's verdict comes from `core::verify`; writing is gated on
  it passing or being explicitly overridden. The write itself keeps the original
  `.conf` in place so a migration is undone by deleting one file.
- **`color_picker.rs`** implements the canvas-based saturation/value square and
  hue strip; **`diff.rs`** is a dependency-free LCS diff for the save preview;
  **`fuzzy.rs`** powers search.
- **`view/`** renders everything:

  | Module           | Renders                                                              |
  | ---------------- | -------------------------------------------------------------------- |
  | `mod.rs`         | the shell (header, status bar, modals) and the save/changes/profile panels |
  | `nav.rs`         | the task-grouped sidebar (`NAV`; a test keeps it complete)           |
  | `options.rs`     | settings sections as grouped cards, per-type editors, search results |
  | `collections.rs` | every list as expandable summaries, with per-kind editors            |
  | `monitors.rs`, `monitor_canvas.rs` | the hardware-driven Monitors screen and its layout canvas |
  | `migrate.rs`     | the `.conf` → Lua migration flow                                     |
  | `styles.rs`      | theme-aware widget styles                                            |

  Per-row UI state (expanded entries, split gaps, the list filter) lives on
  `App`, keyed by index; structural edits and undo/redo prune it only when
  indices actually shift.
- **`settings.rs`** / **`profiles.rs`** persist under XDG dirs.

Headless tests (`ui_tests.rs`) drive `boot → update → view` directly — no
window — to cover the load → edit → add keybind → convert → preview → save
cycle. The opt-in `screenshots` test renders the real `view()` offscreen with
`iced_test` and writes PNGs of the main screens (`just shots`), so layout
changes can be reviewed without a running compositor.

[`full_moon`]: https://github.com/Kampfkarren/full-moon
