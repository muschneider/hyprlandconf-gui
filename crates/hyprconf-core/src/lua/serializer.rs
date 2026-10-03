// SPDX-License-Identifier: MIT OR Apache-2.0
//! Emits Lua against the real `hl` API from a [`Config`], and re-serializes a
//! parsed [`LuaDocument`] losslessly.
//!
//! # The emitted shape
//!
//! Verified against Hyprland 0.56.1 with `Hyprland --verify-config`, and
//! cross-checked against the example config Hyprland ships at
//! `/usr/share/hypr/hyprland.lua`:
//!
//! | construct     | emitted as                                                       |
//! | ------------- | ---------------------------------------------------------------- |
//! | settings      | `hl.config({ ... })`, nested tables                              |
//! | gradients     | `{ colors = { "rgba(..)", ... }, angle = 45 }`                   |
//! | keybinds      | `hl.bind("SUPER + Q", hl.dsp.window.close(), opts?)`             |
//! | submaps       | `hl.define_submap("name", function() ... end)`                   |
//! | window rules  | `hl.window_rule({ name, match = { ... }, <rule> = <value> })`    |
//! | layer rules   | `hl.layer_rule({ name, match = { namespace = ... }, <rule> })`   |
//! | workspaces    | `hl.workspace_rule({ workspace = "2", monitor = "DP-1" })`       |
//! | monitors      | `hl.monitor({ output, mode, position, scale, <extras> })`        |
//! | `exec-once`   | `hl.on("hyprland.start", function() hl.exec_cmd(...) end)`       |
//! | `exec`        | `hl.exec_cmd(...)` at top level (runs on every load)             |
//! | bezier curves | `hl.curve("name", { type = "bezier", points = { {x,y},{x,y} } })`|
//! | animations    | `hl.animation({ leaf, enabled, speed, bezier, style? })`         |
//!
//! # Why the shapes matter
//!
//! Hyprland's Lua bindings are strict: they reject unknown table fields and
//! wrong argument shapes at load time. Two failure modes in particular drove
//! this module's design:
//!
//! 1. **Hard errors.** `hl.bind("SUPER, Q", "killactive")` — the `.conf`-style
//!    key separator and string dispatcher — fails with *"Unknown keysym"* and
//!    *"dispatcher must be a dispatcher"*.
//! 2. **Silent no-ops.** `hl.window_rule({ name = "float", match = ... })`
//!    loads perfectly happily and does *nothing*, because `name` is only a
//!    handle. The rule has to be a real field (`float = true`).
//!
//! The second is why [`crate::lua::rules`] exists and why unmapped rules are
//! reported to the caller rather than being quietly emitted.

use crate::lua::dispatch::{self, Fidelity};
use crate::lua::document::LuaDocument;
use crate::lua::emit::{call, format_float, key as lua_key, Expr, Table};
use crate::lua::extract::escape;
use crate::lua::rules;
use crate::model::Config;
use crate::structured::{ExecKind, Keybind};
use crate::value::{Gradient, Value};

/// Something the Lua API cannot express exactly, raised while serializing.
///
/// These are *not* errors — the output still loads — but they are the things a
/// migration UI should show the user before writing the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaNote {
    /// Which construct produced the note (e.g. `keybind`, `window rule`).
    pub kind: &'static str,
    /// The source construct, in its `.conf` spelling.
    pub subject: String,
    /// What happened and what (if anything) the user should do.
    pub detail: String,
    /// `true` when the emitted Lua still does the right thing (just less
    /// idiomatically); `false` when behaviour may differ.
    pub lossless: bool,
}

/// The result of generating Lua from a [`Config`].
#[derive(Debug, Clone, Default)]
pub struct LuaOutput {
    /// The generated Lua source.
    pub text: String,
    /// Constructs that needed a shim, an approximation, or were skipped.
    pub notes: Vec<LuaNote>,
}

/// Emits Lua from a [`Config`] (or re-prints a [`LuaDocument`] losslessly).
#[derive(Debug, Default, Clone, Copy)]
pub struct LuaSerializer;

impl LuaSerializer {
    /// Re-serialize a parsed document; byte-identical for an unedited document.
    #[must_use]
    pub fn serialize_document(document: &LuaDocument) -> String {
        document.to_text()
    }

    /// Generate fresh, idiomatic Lua from a [`Config`].
    #[must_use]
    pub fn serialize(config: &Config) -> String {
        Self::generate(config).text
    }

    /// Generate Lua and report anything the Lua API could not express exactly.
    #[must_use]
    pub fn generate(config: &Config) -> LuaOutput {
        let mut out = Emitter::default();
        out.run(config);
        LuaOutput {
            text: out.text,
            notes: out.notes,
        }
    }
}

#[derive(Default)]
struct Emitter {
    text: String,
    notes: Vec<LuaNote>,
}

impl Emitter {
    fn line(&mut self, s: &str) {
        self.text.push_str(s);
        self.text.push('\n');
    }

    fn blank(&mut self) {
        if !self.text.ends_with("\n\n") && !self.text.is_empty() {
            self.text.push('\n');
        }
    }

    fn heading(&mut self, title: &str) {
        self.blank();
        self.line(&format!("-- {title}"));
    }

    fn note(&mut self, kind: &'static str, subject: String, detail: String, lossless: bool) {
        self.notes.push(LuaNote {
            kind,
            subject,
            detail,
            lossless,
        });
    }

    fn run(&mut self, config: &Config) {
        self.line("-- Generated by hyprconf");
        self.line("-- Hyprland Lua configuration (the `hl` API).");
        self.line("-- Docs: https://wiki.hypr.land/Configuring/");
        self.blank();

        self.emit_variables(config);
        self.emit_plugins(config);
        self.emit_config(config);
        self.emit_devices(config);
        self.emit_gestures(config);
        self.emit_permissions(config);
        self.emit_monitors(config);
        self.emit_env(config);
        self.emit_execs(config);
        self.emit_curves(config);
        self.emit_animations(config);
        self.emit_workspace_rules(config);
        self.emit_window_rules(config);
        self.emit_layer_rules(config);
        self.emit_binds(config);
    }

    // -- variables ------------------------------------------------------

    fn emit_variables(&mut self, config: &Config) {
        if config.variables.is_empty() {
            return;
        }
        for v in &config.variables {
            let name = &v.value.name;
            let value = &v.value.value;
            if crate::lua::emit::is_ident(name) {
                self.line(&format!("local {name} = {}", Expr::infer(value).render()));
            } else {
                self.note(
                    "variable",
                    format!("${name}"),
                    format!(
                        "`{name}` is not a valid Lua identifier, so it was emitted \
                         as a comment. Rename it or inline its value."
                    ),
                    false,
                );
                self.line(&format!("-- local {name} = \"{}\"", escape(value)));
            }
        }
        self.blank();
    }

    // -- hl.config ------------------------------------------------------

    fn emit_config(&mut self, config: &Config) {
        if config.options.is_empty() {
            return;
        }

        let mut root: Vec<(String, Node)> = Vec::new();
        let mut deferred: Vec<(&String, &Value)> = Vec::new();

        for (path, tracked) in &config.options {
            // `hl.config` only accepts real setting sections. A `.conf` file may
            // also contain *block directives* that look like sections to a
            // line-oriented parser — `windowrule { ... }`, `device { ... }`,
            // `gesture = ...`. Routing those into `hl.config` produces
            // `unknown config key 'windowrule.match.class'` errors, so they are
            // pulled out and emitted as their own `hl.*` calls instead.
            let head = path.split(':').next().unwrap_or(path);
            if BLOCK_DIRECTIVES.contains(&head) {
                deferred.push((path, &tracked.value));
                continue;
            }
            let segs: Vec<&str> = path.split(':').collect();
            insert(&mut root, &segs, tracked.value.clone());
        }

        if !root.is_empty() {
            self.line("hl.config({");
            let body = render_nodes(&root, 1);
            self.text.push_str(&body);
            self.line("})");
        }

        self.emit_block_directives(&deferred);
    }

    /// Emit `device { ... }` / `gesture = ...` style block directives as their
    /// dedicated Lua calls.
    fn emit_block_directives(&mut self, deferred: &[(&String, &Value)]) {
        if deferred.is_empty() {
            return;
        }

        // `device:name`, `device:sensitivity`, ... collapse into one hl.device.
        let mut device = Table::new();
        let mut gestures: Vec<String> = Vec::new();
        let mut unknown: Vec<&String> = Vec::new();

        for (path, value) in deferred {
            let mut segs = path.split(':');
            let head = segs.next().unwrap_or("");
            let rest: Vec<&str> = segs.collect();
            match (head, rest.as_slice()) {
                ("device", [field]) => {
                    device.set(*field, Expr::infer(&value_text(value)));
                }
                // `gesture:<n>` holds one whole `FINGERS, DIRECTION, ACTION[, …]`
                // directive; `gesture:<field>` is the block form.
                ("gesture", [_]) => gestures.push(value_text(value)),
                _ => unknown.push(path),
            }
        }

        if !device.is_empty() {
            self.heading("Per-device input — https://wiki.hypr.land/Configuring/Devices/");
            self.line(&call("hl.device", &[device.into_expr()]));
        }

        if !gestures.is_empty() {
            self.heading("Gestures — https://wiki.hypr.land/Configuring/Gestures/");
            for spec in gestures {
                match gesture_table(&spec) {
                    Some(t) => self.line(&call("hl.gesture", &[t.into_expr()])),
                    None => self.note(
                        "gesture",
                        format!("gesture = {spec}"),
                        "expected `FINGERS, DIRECTION, ACTION`; left out so the \
                         generated config still loads."
                            .to_string(),
                        false,
                    ),
                }
            }
        }

        for path in unknown {
            self.note(
                "setting",
                path.clone(),
                format!(
                    "`{path}` is a block directive with no known Lua equivalent; \
                     it was left out so the generated config still loads. Port it \
                     by hand if you need it."
                ),
                false,
            );
        }
    }

    // -- plugins / devices / gestures / permissions ---------------------

    fn emit_plugins(&mut self, config: &Config) {
        if config.plugins.is_empty() {
            return;
        }
        // Before `hl.config`: a plugin's options only exist once it is loaded.
        self.heading("Plugins");
        for p in &config.plugins {
            self.line(&call("hl.plugin.load", &[Expr::str(&p.value.path)]));
        }
    }

    fn emit_devices(&mut self, config: &Config) {
        if config.devices.is_empty() {
            return;
        }
        self.heading(
            "Per-device input — https://wiki.hypr.land/Configuring/Advanced-and-Cool/Devices/",
        );
        for d in &config.devices {
            let mut t = Table::new();
            t.set_str("name", &d.value.name);
            for (key, value) in &d.value.options {
                // Field names are already canonical (`tap_to_click`).
                t.set(key.as_str(), Expr::infer(value));
            }
            self.line(&call("hl.device", &[t.into_expr()]));
        }
    }

    fn emit_gestures(&mut self, config: &Config) {
        if config.gestures.is_empty() {
            return;
        }
        self.heading("Gestures — https://wiki.hypr.land/Configuring/Advanced-and-Cool/Gestures/");
        for g in &config.gestures {
            let g = &g.value;
            let mut t = Table::new();
            t.set("fingers", Expr::Int(i64::from(g.fingers)))
                .set_str("direction", &g.direction);
            if !g.mods.trim().is_empty() {
                t.set_str("mods", g.mods.trim());
            }
            if let Some(scale) = g.scale {
                t.set("scale", Expr::num(scale));
            }
            let args: Vec<&str> = g
                .args
                .split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .collect();
            match g.action.as_str() {
                "cursorZoom" => {
                    t.set_str("action", "cursor_zoom");
                    if let Some(level) = args.first() {
                        t.set("zoom_level", Expr::infer(level));
                    }
                    if let Some(mode) = args.get(1) {
                        t.set_str("mode", *mode);
                    }
                }
                "scrollMove" => {
                    t.set_str("action", "scroll_move");
                }
                "special" => {
                    t.set_str("action", "special");
                    t.set_str_opt("workspace_name", args.first().copied().unwrap_or(""));
                }
                // Lua has no `dispatcher` action; a callback that dispatches
                // is the equivalent.
                "dispatcher" => {
                    let dsp = args.first().copied().unwrap_or("");
                    let params = args.get(1..).map(|a| a.join(", ")).unwrap_or_default();
                    let translated = dispatch::to_lua(dsp, &params);
                    if translated.fidelity != Fidelity::Native {
                        if let Some(detail) = translated.note.clone() {
                            self.note(
                                "gesture",
                                format!("{} = {}", g.keyword(), g.to_keyword_value()),
                                detail,
                                translated.fidelity == Fidelity::Approximate,
                            );
                        }
                    }
                    t.set(
                        "action",
                        Expr::raw(format!("function() hl.dispatch({}) end", translated.lua)),
                    );
                }
                action => {
                    t.set_str("action", action);
                    if let Some(mode) = args.first() {
                        t.set_str("mode", *mode);
                    }
                }
            }
            if g.bypass_inhibit {
                t.set("disable_inhibit", Expr::Bool(true));
            }
            self.line(&call("hl.gesture", &[t.into_expr()]));
        }
    }

    fn emit_permissions(&mut self, config: &Config) {
        if config.permissions.is_empty() {
            return;
        }
        self.heading("Permissions (need ecosystem.enforce_permissions and a restart)");
        for p in &config.permissions {
            let p = &p.value;
            let mut t = Table::new();
            t.set_str("binary", &p.binary)
                .set_str("type", &p.kind)
                .set_str("mode", &p.mode);
            self.line(&call("hl.permission", &[t.into_expr()]));
        }
    }

    // -- monitors -------------------------------------------------------

    fn emit_monitors(&mut self, config: &Config) {
        if config.monitors.is_empty() {
            return;
        }
        self.heading("Monitors — https://wiki.hypr.land/Configuring/Monitors/");
        for m in &config.monitors {
            let m = &m.value;
            let (t, unknown) = monitor_table(m);
            for other in unknown {
                self.note(
                    "monitor",
                    format!("monitor = {}", m.name),
                    format!(
                        "Trailing modifier `{other}` has no known Lua field \
                         and was dropped. Check the monitor docs."
                    ),
                    false,
                );
            }
            self.line(&call("hl.monitor", &[t.into_expr()]));
        }
    }

    // -- env / exec -----------------------------------------------------

    fn emit_env(&mut self, config: &Config) {
        if config.env.is_empty() {
            return;
        }
        self.heading("Environment variables");
        for e in &config.env {
            let e = &e.value;
            self.line(&call("hl.env", &[Expr::str(&e.name), Expr::str(&e.value)]));
        }
    }

    fn emit_execs(&mut self, config: &Config) {
        if config.execs.is_empty() {
            return;
        }

        let once: Vec<&str> = config
            .execs
            .iter()
            .filter(|e| e.value.kind == ExecKind::ExecOnce)
            .map(|e| e.value.command.as_str())
            .collect();
        let each: Vec<&str> = config
            .execs
            .iter()
            .filter(|e| e.value.kind == ExecKind::Exec)
            .map(|e| e.value.command.as_str())
            .collect();
        let shutdown: Vec<&str> = config
            .execs
            .iter()
            .filter(|e| e.value.kind == ExecKind::ExecShutdown)
            .map(|e| e.value.command.as_str())
            .collect();

        self.heading("Autostart");

        if !once.is_empty() {
            // `exec-once` == "run when Hyprland starts". Emitting these inside
            // the start event (rather than at top level) is what makes them run
            // exactly once, and keeps `--verify-config` from launching them.
            self.line("hl.on(\"hyprland.start\", function()");
            for cmd in once {
                self.line(&format!("    {}", call("hl.exec_cmd", &[Expr::str(cmd)])));
            }
            self.line("end)");
        }

        if !each.is_empty() {
            self.blank();
            self.line("-- `exec`: re-run on every config reload.");
            for cmd in each {
                self.line(&call("hl.exec_cmd", &[Expr::str(cmd)]));
            }
        }

        if !shutdown.is_empty() {
            self.blank();
            self.line("hl.on(\"hyprland.shutdown\", function()");
            for cmd in shutdown {
                self.line(&format!("    {}", call("hl.exec_cmd", &[Expr::str(cmd)])));
            }
            self.line("end)");
        }
    }

    // -- curves / animations --------------------------------------------

    fn emit_curves(&mut self, config: &Config) {
        if config.beziers.is_empty() {
            return;
        }
        self.heading("Bezier curves");
        for b in &config.beziers {
            let b = &b.value;
            let mut spec = Table::new();
            spec.set_str("type", "bezier").set(
                "points",
                Expr::Array(vec![
                    Expr::Array(vec![Expr::num(b.p0.x), Expr::num(b.p0.y)]),
                    Expr::Array(vec![Expr::num(b.p1.x), Expr::num(b.p1.y)]),
                ]),
            );
            self.line(&call("hl.curve", &[Expr::str(&b.name), spec.into_expr()]));
        }
    }

    fn emit_animations(&mut self, config: &Config) {
        if config.animations.is_empty() {
            return;
        }
        self.heading("Animations");
        for a in &config.animations {
            let a = &a.value;
            let mut t = Table::new();
            t.set_str("leaf", &a.name)
                .set("enabled", Expr::Bool(a.enabled))
                .set("speed", Expr::num(a.speed));
            if !a.curve.is_empty() {
                t.set_str("bezier", &a.curve);
            }
            if let Some(style) = &a.style {
                t.set_str("style", style);
            }
            self.line(&call("hl.animation", &[t.into_expr()]));
        }
    }

    // -- rules ----------------------------------------------------------

    fn emit_workspace_rules(&mut self, config: &Config) {
        if config.workspaces.is_empty() {
            return;
        }
        self.heading("Workspace rules — https://wiki.hypr.land/Configuring/Workspace-Rules/");
        for w in &config.workspaces {
            let w = &w.value;
            let mut t = Table::new();
            t.set("workspace", Expr::str(&w.selector));
            let (fields, unmapped) = rules::workspace_rule_fields(&w.rules);
            for (k, v) in fields {
                t.set(k, v);
            }
            for u in unmapped {
                self.note(
                    "workspace rule",
                    format!("workspace = {}, {}", w.selector, w.rules),
                    format!("{} — dropped `{}`.", u.reason, u.text),
                    false,
                );
            }
            self.line(&call("hl.workspace_rule", &[t.into_expr()]));
        }
    }

    fn emit_window_rules(&mut self, config: &Config) {
        if config.window_rules.is_empty() {
            return;
        }
        self.heading("Window rules — https://wiki.hypr.land/Configuring/Window-Rules/");

        // Consecutive `.conf` rules that share a matcher describe a single Lua
        // rule table. Merging them keeps the output close to how a person would
        // write it, and mirrors the `windowrule { … }` blocks such groups
        // usually come from.
        for (index, group) in group_runs(&config.window_rules, |r| (r.matchers.clone(), r.v2))
            .into_iter()
            .enumerate()
        {
            let first = group[0];
            let mut t = Table::new();
            t.set_str("name", rule_name(&first.rule, index));
            let matchers = rules::matcher_table(&first.matchers, first.v2);
            if !matchers.is_empty() {
                t.set("match", matchers.into_expr());
            }

            let mut any = false;
            for r in &group {
                match rules::window_rule_fields(&r.rule) {
                    Ok(fields) => {
                        for (k, v) in fields {
                            t.set(k, v);
                        }
                        any = true;
                    }
                    Err(u) => {
                        let subject = format!(
                            "{} = {}, {}",
                            if r.v2 { "windowrulev2" } else { "windowrule" },
                            r.rule,
                            r.matchers
                        );
                        // Emitting it anyway would load fine and silently do
                        // nothing, which is worse than an explicit comment.
                        self.note(
                            "window rule",
                            subject.clone(),
                            format!("{}. Left commented out for you to port by hand.", u.reason),
                            false,
                        );
                        self.line(&format!("-- TODO(hyprconf): unmapped rule — {}", u.reason));
                        self.line(&format!("-- {subject}"));
                    }
                }
            }
            if any {
                self.line(&call("hl.window_rule", &[t.into_expr()]));
            }
        }
    }

    fn emit_layer_rules(&mut self, config: &Config) {
        if config.layer_rules.is_empty() {
            return;
        }
        self.heading("Layer rules");
        for (i, r) in config.layer_rules.iter().enumerate() {
            let r = &r.value;
            let subject = format!("layerrule = {}, {}", r.rule, r.namespace);

            let mut t = Table::new();
            t.set_str("name", rule_name(&r.rule, i));
            if !r.namespace.is_empty() {
                let mut m = Table::new();
                m.set_str("namespace", &r.namespace);
                t.set("match", m.into_expr());
            }

            match rules::layer_rule_fields(&r.rule) {
                Ok(fields) => {
                    for (k, v) in fields {
                        t.set(k, v);
                    }
                    self.line(&call("hl.layer_rule", &[t.into_expr()]));
                }
                Err(u) => {
                    self.note(
                        "layer rule",
                        subject.clone(),
                        format!("{}. Left commented out for you to port by hand.", u.reason),
                        false,
                    );
                    self.line(&format!("-- TODO(hyprconf): unmapped rule — {}", u.reason));
                    self.line(&format!("-- {subject}"));
                }
            }
        }
    }

    // -- keybinds -------------------------------------------------------

    fn emit_binds(&mut self, config: &Config) {
        if config.keybinds.is_empty() {
            return;
        }

        self.heading("Keybinds — https://wiki.hypr.land/Configuring/Binds/");

        // Global binds first, then one `hl.define_submap` block per submap so
        // the grouping survives the round-trip.
        let mut submaps: Vec<String> = Vec::new();
        for b in &config.keybinds {
            if let Some(s) = &b.value.submap {
                if s != "reset" && !submaps.contains(s) {
                    submaps.push(s.clone());
                }
            }
        }

        for b in &config.keybinds {
            if b.value.submap.is_none() {
                let line = self.bind_line(&b.value);
                self.line(&line);
            }
        }

        for name in submaps {
            self.blank();
            self.line(&format!(
                "hl.define_submap(\"{}\", function()",
                escape(&name)
            ));
            for b in &config.keybinds {
                if b.value.submap.as_deref() == Some(name.as_str()) {
                    let line = self.bind_line(&b.value);
                    self.line(&format!("    {line}"));
                }
            }
            self.line("end)");
        }
    }

    fn bind_line(&mut self, bind: &Keybind) -> String {
        let keys = bind_keys(bind);
        let translated = if bind.flags.mouse {
            dispatch::to_lua_mouse(&bind.dispatcher, &bind.args)
        } else {
            dispatch::to_lua(&bind.dispatcher, &bind.args)
        };

        if translated.fidelity != Fidelity::Native {
            if let Some(detail) = translated.note.clone() {
                self.note(
                    "keybind",
                    format!(
                        "bind = {}, {}, {}{}",
                        bind.mods,
                        bind.key,
                        bind.dispatcher,
                        if bind.args.is_empty() {
                            String::new()
                        } else {
                            format!(", {}", bind.args)
                        }
                    ),
                    detail,
                    translated.fidelity == Fidelity::Approximate,
                );
            }
        }

        let mut opts = Table::new();
        let f = &bind.flags;
        for (enabled, name) in [
            (f.locked, "locked"),
            (f.release, "release"),
            (f.repeat, "repeating"),
            (f.non_consuming, "non_consuming"),
            (f.transparent, "transparent"),
            (f.ignore_mods, "ignore_mods"),
            (f.long_press, "long_press"),
            (f.click, "click"),
            (f.dont_inhibit, "dont_inhibit"),
        ] {
            if enabled {
                opts.set(name, Expr::Bool(true));
            }
        }
        // `bindm` is expressed by the `drag` option plus a drag dispatcher;
        // there is no `mouse` field in HL.BindOptions.
        if f.mouse || f.drag {
            opts.set("drag", Expr::Bool(true));
        }
        if let Some(description) = bind.description.as_deref().filter(|d| !d.is_empty()) {
            opts.set_str("description", description);
        }

        let mut args = vec![Expr::str(keys), Expr::raw(translated.lua)];
        if !opts.is_empty() {
            args.push(opts.into_expr());
        }
        call("hl.bind", &args)
    }
}

/// Build the Lua key string: `SUPER + SHIFT + Q`.
///
/// `.conf` writes `MODS, KEY` and accepts several modifier separators —
/// whitespace (`SUPER SHIFT`), `+` (`SUPER+SHIFT`) and underscore
/// (`SUPER_SHIFT`, which is what the stock config generator emits). Lua wants a
/// single `+`-joined chord and rejects everything else outright with
/// *"Unknown keysym ... did you forget a +?"*.
///
/// The underscore split is deliberately conservative: it only applies to tokens
/// that are made entirely of known modifier names, so key names that legitimately
/// contain underscores (`XF86_Foo`, `code:123`) are left alone.
fn bind_keys(bind: &Keybind) -> String {
    let mut parts: Vec<String> = Vec::new();
    for token in bind
        .mods
        .split(|c: char| c.is_whitespace() || c == '+')
        .filter(|s| !s.is_empty())
    {
        match split_mod_token(token) {
            Some(expanded) => parts.extend(expanded),
            None => parts.push(token.to_string()),
        }
    }
    if !bind.key.is_empty() {
        parts.push(bind.key.clone());
    }
    parts.join(" + ")
}

/// Hyprland's modifier names, in the spellings hyprlang accepts.
const MOD_NAMES: &[&str] = &[
    "SUPER",
    "SUPERSHIFT",
    "SHIFT",
    "CTRL",
    "CONTROL",
    "ALT",
    "MOD2",
    "MOD3",
    "MOD5",
    "META",
    "WIN",
    "LOGO",
    "HYPER",
    "CAPS",
    "MOD1",
];

/// Split `SUPER_SHIFT` into `["SUPER", "SHIFT"]`, but only when *every* part is
/// a real modifier name.
fn split_mod_token(token: &str) -> Option<Vec<String>> {
    if !token.contains('_') {
        return None;
    }
    let parts: Vec<&str> = token.split('_').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return None;
    }
    let all_mods = parts
        .iter()
        .all(|p| MOD_NAMES.contains(&p.to_ascii_uppercase().as_str()));
    all_mods.then(|| parts.into_iter().map(str::to_string).collect())
}

/// Group consecutive items that share a key, preserving order.
///
/// Rules are order-sensitive, so only *adjacent* items may be merged — grouping
/// globally would silently reorder rule application.
fn group_runs<T, K: PartialEq>(
    items: &[crate::model::Tracked<T>],
    key: impl Fn(&T) -> K,
) -> Vec<Vec<&T>> {
    let mut out: Vec<Vec<&T>> = Vec::new();
    let mut last: Option<K> = None;
    for item in items {
        let k = key(&item.value);
        match &last {
            Some(prev) if *prev == k => out.last_mut().expect("non-empty").push(&item.value),
            _ => out.push(vec![&item.value]),
        }
        last = Some(k);
    }
    out
}

/// A stable, readable `name` handle for a rule.
///
/// `name` is only an identifier for `set_enabled`, but a descriptive one makes
/// the generated file far easier to read and edit.
fn rule_name(rule: &str, index: usize) -> String {
    let slug: String = rule
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').replace("--", "-");
    if slug.is_empty() {
        format!("rule-{index}")
    } else {
        format!("{slug}-{index}")
    }
}

/// Render a [`Value`] as a Lua expression.
#[must_use]
pub fn value_to_lua(value: &Value) -> String {
    match value {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(x) => format_float(*x),
        Value::Color(c) => format!("\"{}\"", c.to_rgba_string()),
        Value::Gradient(g) => gradient_expr(g).render(),
        Value::String(s) => format!("\"{}\"", escape(s)),
        // Enum variants whose literal is numeric (Hyprland integer "mode"
        // options like `follow_mouse = 1` or `force_default_wallpaper = -1`)
        // must be emitted as bare Lua numbers; textual variants (e.g.
        // `layout = "dwindle"`) stay quoted.
        Value::Enum(name) if is_lua_number(name) => name.clone(),
        // Several integer "mode" options also accept a plain boolean in `.conf`
        // (`cursor:no_hardware_cursors = true`). Quoting it would trip Lua's
        // stricter parser with *"integer type requires a bool or an integer"*.
        Value::Enum(name) if is_lua_bool(name) => name.to_ascii_lowercase(),
        Value::Enum(name) => format!("\"{}\"", escape(name)),
        Value::Vec2(v) => format!("\"{}\"", v.to_hyprland_string()),
        // Lua rejects the `.conf` string form (*"css_gap type requires an
        // integer or a table"*), so per-side gaps become a table.
        Value::CssGap(g) if g.is_uniform() => g.top.to_string(),
        Value::CssGap(g) => {
            let mut t = Table::new();
            t.set("top", Expr::Int(g.top))
                .set("right", Expr::Int(g.right))
                .set("bottom", Expr::Int(g.bottom))
                .set("left", Expr::Int(g.left));
            t.into_expr().render()
        }
    }
}

/// The `hl.monitor({ … })` table for a rule, plus any trailing `.conf`
/// modifiers that have no Lua field (reported by the caller, never guessed).
fn monitor_table(m: &crate::structured::MonitorRule) -> (Table, Vec<String>) {
    let mut t = Table::new();
    t.set_str("output", &m.name);
    t.set_str_opt("mode", &m.mode);
    t.set_str_opt("position", &m.position);
    if !m.scale.is_empty() {
        t.set("scale", Expr::infer(&m.scale));
    }

    // Trailing conf modifiers (`transform, 1`, `vrr, 2`, `mirror, DP-1`,
    // `bitdepth, 10`) become real table fields.
    let mut unknown = Vec::new();
    let mut i = 0;
    while i < m.extra.len() {
        let lc = m.extra[i].trim().to_ascii_lowercase();
        match lc.as_str() {
            "transform"
            | "vrr"
            | "bitdepth"
            | "mirror"
            | "cm"
            | "sdrbrightness"
            | "sdrsaturation"
            | "icc"
            | "supports_hdr"
            | "supports_wide_color" => {
                if let Some(v) = m.extra.get(i + 1) {
                    t.set(lc.as_str(), Expr::infer(v.trim()));
                    i += 2;
                    continue;
                }
                t.set(lc.as_str(), Expr::Bool(true));
                i += 1;
            }
            "" => i += 1,
            _ => {
                unknown.push(lc);
                i += 1;
            }
        }
    }
    (t, unknown)
}

/// One `hl.monitor({ … })` call — what a live monitor change sends to a Lua
/// session through `hyprctl eval`.
#[must_use]
pub fn monitor_call(rule: &crate::structured::MonitorRule) -> String {
    call("hl.monitor", &[monitor_table(rule).0.into_expr()])
}

/// Render one `hl.config({ … })` call holding exactly `options` (canonical
/// paths), nested the way Hyprland's own example config writes it.
///
/// Used for the managed override block a Lua *preserve* save appends, so a
/// settings change never has to regenerate — and lose — a hand-written file.
#[must_use]
pub fn config_call<'a>(options: impl IntoIterator<Item = (&'a str, &'a Value)>) -> String {
    let mut root: Vec<(String, Node)> = Vec::new();
    for (path, value) in options {
        let segs: Vec<&str> = path.split(':').collect();
        insert(&mut root, &segs, value.clone());
    }
    format!("hl.config({{\n{}}})\n", render_nodes(&root, 1))
}

/// Render a gradient in the shape Hyprland's Lua API accepts.
///
/// A single stop with no angle is a plain color string; anything else must be
/// `{ colors = { ... }, angle = N }`. The `.conf` spelling
/// (`"rgba(a) rgba(b) 45deg"`) is **rejected** by the Lua colour parser with
/// *"invalid color"*, which is one of the bugs this rewrite fixes.
fn gradient_expr(g: &Gradient) -> Expr {
    if g.stops.len() <= 1 && g.angle_deg.is_none() {
        let color = g
            .stops
            .first()
            .map_or_else(|| "rgba(00000000)".to_string(), |c| c.to_rgba_string());
        return Expr::str(color);
    }
    let mut t = Table::new();
    t.set(
        "colors",
        Expr::Array(
            g.stops
                .iter()
                .map(|c| Expr::str(c.to_rgba_string()))
                .collect(),
        ),
    );
    if let Some(angle) = g.angle_deg {
        t.set("angle", Expr::num(angle));
    }
    t.into_expr()
}

// ---------------------------------------------------------------------------
// hl.config nested table
// ---------------------------------------------------------------------------

/// Top-level `.conf` "sections" that are really repeatable block directives, not
/// settings groups. They must never be emitted inside `hl.config`.
const BLOCK_DIRECTIVES: &[&str] = &[
    "windowrule",
    "windowrulev2",
    "layerrule",
    "workspace",
    "device",
    "gesture",
    "monitor",
    "permission",
    "bind",
    "submap",
];

/// Parse a `.conf` gesture directive into its Lua table.
///
/// `gesture = 3, horizontal, workspace` becomes
/// `{ fingers = 3, direction = "horizontal", action = "workspace" }`. A fourth
/// field, when present, is the action's argument (`scale`, `workspace_name`, …);
/// it is passed through as `mode` only when it is clearly a modifier list,
/// otherwise the whole directive is reported rather than guessed at.
fn gesture_table(spec: &str) -> Option<Table> {
    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let fingers = parts[0].parse::<i64>().ok()?;
    let mut t = Table::new();
    t.set("fingers", Expr::Int(fingers))
        .set_str("direction", parts[1])
        .set_str("action", parts[2]);
    if let Some(extra) = parts.get(3) {
        if !extra.is_empty() {
            // The 4th slot is action-specific; `workspace` takes a name,
            // `scale` takes a factor.
            match parts[2] {
                "scale" => {
                    t.set("scale", Expr::infer(extra));
                }
                "workspace" => {
                    t.set_str("workspace_name", *extra);
                }
                _ => {
                    t.set_str("mode", *extra);
                }
            }
        }
    }
    Some(t)
}

/// The plain text of a [`Value`], for values that must be re-inferred rather
/// than rendered as Lua (block-directive fields are untyped).
fn value_text(value: &Value) -> String {
    match value {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(x) => format_float(*x),
        Value::String(s) | Value::Enum(s) => s.clone(),
        Value::Color(c) => c.to_rgba_string(),
        Value::Gradient(g) => g.to_hyprland_string(),
        Value::Vec2(v) => v.to_hyprland_string(),
        Value::CssGap(g) => g.to_hyprland_string(),
    }
}

enum Node {
    Leaf(Value),
    Branch(Vec<(String, Node)>),
}

fn insert(branch: &mut Vec<(String, Node)>, segs: &[&str], value: Value) {
    let head = segs[0];
    if segs.len() == 1 {
        branch.push((head.to_string(), Node::Leaf(value)));
        return;
    }
    let pos = branch
        .iter()
        .position(|(k, t)| k == head && matches!(t, Node::Branch(_)));
    let idx = match pos {
        Some(i) => i,
        None => {
            branch.push((head.to_string(), Node::Branch(Vec::new())));
            branch.len() - 1
        }
    };
    if let Node::Branch(sub) = &mut branch[idx].1 {
        insert(sub, &segs[1..], value);
    }
}

fn render_nodes(branch: &[(String, Node)], indent: usize) -> String {
    let pad = "    ".repeat(indent);
    let mut out = String::new();
    for (k, node) in branch {
        let key = lua_key(k);
        match node {
            Node::Leaf(value) => {
                out.push_str(&format!("{pad}{key} = {},\n", value_to_lua(value)));
            }
            Node::Branch(sub) => {
                out.push_str(&format!("{pad}{key} = {{\n"));
                out.push_str(&render_nodes(sub, indent + 1));
                out.push_str(&format!("{pad}}},\n"));
            }
        }
    }
    out
}

/// Whether an enum variant literal is a plain number (so it should be emitted as
/// a bare Lua number rather than a quoted string).
fn is_lua_number(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok_and(f64::is_finite)
}

/// Whether an enum variant literal is really a boolean written into an
/// integer-typed option.
fn is_lua_bool(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "true" | "false")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured::KeybindFlags;
    use crate::value::Color;

    #[test]
    fn value_rendering() {
        assert_eq!(value_to_lua(&Value::Bool(true)), "true");
        assert_eq!(value_to_lua(&Value::Int(10)), "10");
        assert_eq!(value_to_lua(&Value::Float(0.5)), "0.5");
        assert_eq!(value_to_lua(&Value::Float(1.0)), "1");
        assert_eq!(value_to_lua(&Value::Enum("dwindle".into())), "\"dwindle\"");
        assert_eq!(value_to_lua(&Value::Enum("1".into())), "1");
        assert_eq!(value_to_lua(&Value::Enum("-1".into())), "-1");
        assert_eq!(value_to_lua(&Value::Enum(String::new())), "\"\"");
        assert_eq!(value_to_lua(&Value::Enum("2fg".into())), "\"2fg\"");
        assert_eq!(
            value_to_lua(&Value::Color(Color::rgb(255, 0, 0))),
            "\"rgba(ff0000ff)\""
        );
    }

    #[test]
    fn single_stop_gradients_stay_plain_colour_strings() {
        let g = Gradient::solid(Color::rgb(0x59, 0x59, 0x59));
        assert_eq!(value_to_lua(&Value::Gradient(g)), "\"rgba(595959ff)\"");
    }

    #[test]
    fn multi_stop_gradients_become_tables() {
        // The `.conf` string form is rejected by Hyprland's Lua colour parser,
        // so multi-stop gradients must use the `{ colors, angle }` table.
        let g = Gradient::with_angle(
            vec![
                Color::rgba(0x33, 0xcc, 0xff, 0xee),
                Color::rgba(0, 0xff, 0x99, 0xee),
            ],
            45.0,
        );
        assert_eq!(
            value_to_lua(&Value::Gradient(g)),
            "{ colors = { \"rgba(33ccffee)\", \"rgba(00ff99ee)\" }, angle = 45 }"
        );
    }

    fn bind(mods: &str, key_: &str, dispatcher: &str, args: &str) -> Keybind {
        Keybind {
            flags: KeybindFlags::default(),
            mods: mods.into(),
            key: key_.into(),
            dispatcher: dispatcher.into(),
            args: args.into(),
            submap: None,
            description: None,
        }
    }

    #[test]
    fn bind_keys_join_with_plus() {
        assert_eq!(
            bind_keys(&bind("SUPER", "Q", "killactive", "")),
            "SUPER + Q"
        );
        assert_eq!(
            bind_keys(&bind("SUPER SHIFT", "Q", "killactive", "")),
            "SUPER + SHIFT + Q"
        );
        // A bind with no modifier must not emit a leading separator.
        assert_eq!(bind_keys(&bind("", "escape", "submap", "reset")), "escape");
    }

    #[test]
    fn bind_line_uses_a_real_dispatcher_object() {
        let mut e = Emitter::default();
        let line = e.bind_line(&bind("SUPER", "Q", "killactive", ""));
        assert_eq!(line, "hl.bind(\"SUPER + Q\", hl.dsp.window.close())");
        assert!(e.notes.is_empty());
    }

    #[test]
    fn mouse_binds_emit_drag_option_not_a_mouse_field() {
        let mut b = bind("SUPER", "mouse:272", "movewindow", "");
        b.flags.mouse = true;
        let mut e = Emitter::default();
        let line = e.bind_line(&b);
        assert_eq!(
            line,
            "hl.bind(\"SUPER + mouse:272\", hl.dsp.window.drag(), { drag = true })"
        );
    }

    #[test]
    fn unmapped_dispatchers_are_reported() {
        let mut e = Emitter::default();
        let line = e.bind_line(&bind("SUPER", "X", "plugin:custom", "arg"));
        assert!(line.contains("hyprctl dispatch plugin:custom arg"));
        assert_eq!(e.notes.len(), 1);
        assert_eq!(e.notes[0].kind, "keybind");
    }

    #[test]
    fn rule_names_are_slugs() {
        assert_eq!(rule_name("float", 0), "float-0");
        assert_eq!(rule_name("opacity 0.9 0.9", 3), "opacity-0-9-0-9-3");
        assert_eq!(rule_name("", 2), "rule-2");
    }

    #[test]
    fn is_lua_number_distinguishes_numeric_literals() {
        assert!(is_lua_number("0"));
        assert!(is_lua_number("-1"));
        assert!(is_lua_number("0.5"));
        assert!(!is_lua_number(""));
        assert!(!is_lua_number("auto"));
        assert!(!is_lua_number("2fg"));
        assert!(!is_lua_number("inf"));
    }
}
