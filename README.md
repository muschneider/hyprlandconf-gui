<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# hyprconf

A Linux-first desktop GUI for viewing and editing the **full** surface of
[Hyprland](https://hyprland.org) configuration — for both the modern **Lua**
format and the legacy **conf** (hyprlang) format, over one shared model.

![hyprconf: the task-grouped sidebar and the General section](assets/screenshot.png)

> [!IMPORTANT]
> **Hyprland 0.56 warns that `.conf` support will be removed in 0.57.** hyprconf
> ships a guided **Convert to Lua** flow that translates your whole config and
> then hands the result to `Hyprland --verify-config` — so the compositor itself
> confirms the file loads *before* anything is written. See
> [Migrating from `.conf` to Lua](#migrating-from-conf-to-lua).

## Why

Hyprland has a large, fast-moving configuration surface. Editing it by hand
means memorising option names, value formats (`rgba(...)`, gradients, `Vec2`,
bind flags), and — since 0.55 — juggling **two** on-disk formats. hyprconf gives
you a typed, validated, searchable UI over that surface while treating your file
as the source of truth: it preserves comments and ordering where it can, shows
you an exact diff before writing, and never silently drops anything it doesn't
understand.

## Features

- **Both formats, first-class.** Reads `hyprland.lua` (the `hl` API, `require()`
  sourcing) **and** `hyprland.conf` (`key = value`, `{}` sections, `source=`,
  `$variables`, `windowrule[v2]`, and the 0.56 block forms `windowrule { … }` /
  `device { … }`). You choose which format to write.
- **Guided `.conf` → Lua migration**, verified by Hyprland itself before it
  writes. See [below](#migrating-from-conf-to-lua).
- **All 353 options Hyprland 0.56 has**, checked by tests against the
  compositor's own option descriptions (coverage, value lists and defaults).
- **A task-oriented layout** — *Look & feel, Layouts, Input, Keyboard
  shortcuts, Windows & workspaces, Displays, Startup, System* — with options
  grouped into cards (Blur, Shadow, Group bar, …) and described inline.
- **Editors shaped by the value** — one-click segmented choices, sliders with
  −/+ steppers, per-side gaps (linked or split), gradients with a live preview,
  open choices that also take custom values (`lua:my-layout`), and a **visual
  color picker** (saturation/value square + hue strip, HEX + RGBA).
- **Every list in the config** — keybinds (modifiers, 62 described
  dispatchers, labelled flags, descriptions, submaps), window/layer rules
  (effect and matcher pick-lists), workspace rules, monitors (with a
  drag-to-arrange layout), gesture bindings, per-device input, permissions,
  plugins, bezier curves (live curve preview), animations, env, exec,
  variables. Each entry is a one-line summary that expands into its editor.
- **Live fuzzy search** across the whole option surface.
- **Safe saving** — a validation pass (errors block, warnings are overridable),
  a per-file before/after **diff preview**, then **atomic writes with timestamped
  backups**. Comment-preserving in-place edits for same-format `.conf`.
- **Undo/redo** (with sensible coalescing) and `Ctrl+Z` / `Ctrl+Shift+Z`.
- **Live apply** — when a running Hyprland is detected, optionally push edits
  instantly (`hyprctl keyword` for `.conf` sessions, `hyprctl eval` with the
  equivalent `hl.config` call for Lua sessions), and reload on demand.
- **Profiles & recents** — save the current config as a named profile, reopen
  recents, import any file.
- **Persistent settings** — theme (22 built-in themes), last format, window
  size and recent files survive restarts.
- **`--check`** — a headless "load and summarise" mode for scripts/CI.

## Supported Hyprland versions

hyprconf's option **schema** is checked against the Hyprland **0.56** Lua stub
and the compositor's own option descriptions (`hyprctl descriptions`), both
vendored in `meta/` (see [`meta/README.md`](meta/README.md)) — that's the
version it knows the most about. It still reads and writes configs for older and newer Hyprland
releases; out-of-schema keys are **preserved, never dropped** (and surfaced as
warnings). When a running Hyprland is detected, options whose `since` version is
newer than what's running are flagged in the status bar.

## Migrating from `.conf` to Lua

Hyprland 0.56 prints:

> You are using the .conf config format, support for which will be removed in
> Hyprland 0.57.

Converting is not a syntax change — it is a **semantic** one, and the sharp edges
are easy to miss:

| `.conf` | Lua | if you get it wrong |
| --- | --- | --- |
| `bind = SUPER, Q, killactive` | `hl.bind("SUPER + Q", hl.dsp.window.close())` | *"Unknown keysym… did you forget a `+`?"* — the bind is dead |
| `col.active_border = rgba(a) rgba(b) 45deg` | `{ colors = { … }, angle = 45 }` | *"invalid color"* |
| `windowrulev2 = float, class:^(kitty)$` | `{ name = …, match = { class = … }, float = true }` | **loads fine and does nothing** |
| `exec-once = waybar` | inside `hl.on("hyprland.start", …)` | re-runs on every reload |
| `bezier = name, …` | `hl.curve(name, { type = "bezier", points = … })` | rejected at load |

hyprconf handles all of these, and the ones it *cannot* express exactly are
listed up front rather than dropped quietly.

**The flow** (`hyprconf --migrate`, the banner, or *→ Lua* in the toolbar):

1. **Review** — what was found, and anything that needs a human eye.
2. **Preview** — the generated Lua, in full. Nothing has been written yet.
3. **Check** — hyprconf runs `Hyprland --verify-config` on the generated file in
   a temp directory and shows the verdict verbatim. Autostart commands are
   commented out for the check so nothing gets launched.
4. **Apply** — writes `hyprland.lua`, backing up anything it replaces, and
   **leaves your `.conf` in place**. Hyprland prefers the `.lua` file when both
   exist, so undoing the migration is deleting one file.

There is also a headless converter for scripting:

```sh
cargo run --example convert -- ~/.config/hypr/hyprland.conf > hyprland.lua
Hyprland --verify-config -c hyprland.lua
```

## Lua vs conf, and the dynamic-Lua caveat

- The in-memory model is **format-agnostic**. Reading either format produces the
  same `Config`; you pick the output format independently.
- **conf → conf, settings-only edits** are *preserved*: hyprconf edits the
  original document(s) in place, keeping comments, ordering and untouched lines
  byte-for-byte, and (for multi-file setups) rewrites only the files that changed.
- **Lua → Lua, settings-only edits** are *preserved* too: your file is left
  exactly as written and the changes go into one fenced block at its end,
  which later saves update in place (delete the block to undo them):

  ```lua
  -- >>> hyprconf: settings changed in hyprconf (delete this block to undo them) >>>
  hl.config({
      decoration = {
          rounding = 12,
      },
  })
  -- <<< hyprconf <<<
  ```
- **Format conversions and list edits** (keybinds, rules, …) *regenerate* a
  fresh file from the model. For a Lua file containing code, the save panel
  says how many blocks would be lost and requires explicit consent; a backup
  is always kept.
- **Dynamic Lua is never executed or flattened.** Hyprland's Lua config can
  contain loops, functions, conditionals, and `hl.on`/timer closures. hyprconf
  parses Lua *losslessly* (it does not run it) and only interprets the
  *declarative* subset. Anything dynamic is left untouched and reported as a
  read-only region — and **converting a Lua config that contains dynamic regions
  to conf will drop them**, which hyprconf warns about before you save.

## Install

### Arch Linux (AUR)

A `PKGBUILD` lives in [`packaging/aur/`](packaging/aur/PKGBUILD). With an AUR
helper once published, or directly from the repo:

```sh
cd packaging/aur && makepkg -si
```

### Nix (flake)

```sh
nix run github:hyprconf/hyprconf       # run without installing
nix profile install github:hyprconf/hyprconf   # install into your profile
```

A local checkout works too: `nix run .` / `nix build .`.

### cargo install

```sh
cargo install --git https://github.com/hyprconf/hyprconf hyprconf-gui
```

On Linux you need the usual iced/winit/wgpu system libraries at build time
(e.g. `libxkbcommon`, `wayland`); see [`packaging/`](packaging/) and the CI
workflow for the exact package lists.

### From source

```sh
git clone https://github.com/hyprconf/hyprconf
cd hyprconf
cargo run -p hyprconf-gui --release       # or: just run
```

## Usage

```sh
hyprconf                                 # auto-detect ~/.config/hypr/hyprland.{lua,conf}
hyprconf --config /path/to/hyprland.lua  # load a specific file
hyprconf --migrate                       # open the .conf → Lua flow straight away
hyprconf --check                         # headless: load, print a summary, exit
```

- **Search**: `Ctrl+F`, or type in the header box, to fuzzy-find any option.
- **Edit**: change values with the typed editors; the 🎨 icon on any color field
  opens the visual picker.
- **Undo/redo**: `Ctrl+Z` / `Ctrl+Shift+Z` (or `Ctrl+Y`). `Ctrl+S` opens the
  save panel.
- **Convert to Lua**: the banner, the *→ Lua* toolbar button, or `--migrate`.
- **Save**: open *save…*, pick the output format, review the diff, write. A
  backup of each overwritten file is kept.
- **Live apply**: if the status bar shows a detected Hyprland, toggle *live* to
  push setting and monitor edits immediately, or hit *reload*. Hover the ✓/✕
  for Hyprland's reply.

Logging honours `RUST_LOG` (e.g. `RUST_LOG=debug hyprconf`).

## Development

Requires a Rust toolchain (`mise install` honours [`mise.toml`](mise.toml)) and
[`just`](https://github.com/casey/just).

```sh
just            # list recipes
just run        # run the GUI
just test       # run the test suite (incl. headless UI tests)
just lint       # clippy -D warnings
just fmt        # format
just ci         # fmt-check + lint + test (the full local gate)
just shots      # render the main screens headlessly to /tmp/hyprconf-shots
just refresh-descriptions   # re-vendor option data from the running Hyprland
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the core/gui split and the model, and
[CONTRIBUTING.md](CONTRIBUTING.md) to get started.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. See [NOTICE](NOTICE) for
third-party attributions (including the vendored Hyprland metadata, which is
BSD-3-Clause). hyprconf is an independent project, not affiliated with Hyprland.
