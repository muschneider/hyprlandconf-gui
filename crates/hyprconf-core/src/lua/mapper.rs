// SPDX-License-Identifier: MIT OR Apache-2.0
//! Builds the semantic [`Config`] from a parsed `.lua` document/bundle by
//! walking the declarative subset of the AST.
//!
//! Recognised statements (`hl.config`, `hl.bind`, `hl.window_rule`,
//! `hl.monitor`, ... and top-level `require`) are interpreted into the model.
//! **Everything else is dynamic**: it stays untouched in the lossless
//! [`LuaDocument`] and is reported as [`LuaWarning::DynamicRegion`] so the GUI
//! can present it read-only and never flatten user logic.

use std::collections::HashMap;
use std::path::PathBuf;

use full_moon::ast::Stmt;

use crate::lua::dispatch;
use crate::lua::document::LuaDocument;
use crate::lua::extract::{callback_call, callee_path, expr_to_luaval, LuaField, LuaVal};
use crate::lua::parser::{resolve_require, LuaBundle};
use crate::lua::rules;
use crate::model::{Config, ConfigFormat, Provenance, Tracked};
use crate::schema::{Schema, ValueType};
use crate::structured::{
    Animation, Bezier, Device, EnvVar, Exec, ExecKind, Gesture, Keybind, KeybindFlags, LayerRule,
    MonitorRule, Permission, Plugin, Variable, WindowRule, WorkspaceRule,
};
use crate::value::{Color, CssGap, Gradient, Value, Vec2};

/// A non-fatal issue (or a preserved dynamic region) found while mapping Lua.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LuaWarning {
    /// A statement outside the declarative subset. It is preserved verbatim in
    /// the document and must be treated as read-only / externally managed.
    #[error("dynamic Lua preserved read-only: {snippet}")]
    DynamicRegion {
        /// The verbatim source of the dynamic statement (trimmed).
        snippet: String,
    },

    /// A `hl.config` key the schema does not know about (stored as a string).
    #[error("unknown option `{path}`")]
    UnknownOption {
        /// The unknown dotted path.
        path: String,
    },

    /// A known option's value could not be parsed into its declared type.
    #[error("could not parse value {value:?} for `{path}`: {reason}")]
    UnparsableValue {
        /// The option path.
        path: String,
        /// The offending value text.
        value: String,
        /// Why parsing failed.
        reason: String,
    },

    /// A recognised `hl.*` call had arguments we could not interpret.
    #[error("could not interpret {callee} call: {reason}")]
    UnparsableCall {
        /// The callee, e.g. `hl.bind`.
        callee: String,
        /// Why interpretation failed.
        reason: String,
    },

    /// A `require` could not be resolved to a loaded document.
    #[error("unresolved require {module:?}")]
    UnresolvedRequire {
        /// The module string.
        module: String,
    },
}

/// Build a [`Config`] from a single Lua document (does not follow `require`).
#[must_use]
pub fn document_to_config(document: &LuaDocument, schema: &Schema) -> (Config, Vec<LuaWarning>) {
    let docs = std::slice::from_ref(document);
    let by_path = HashMap::new();
    let mut ctx = Ctx::new(schema, docs, &by_path);
    ctx.config.source = document.path.clone();
    ctx.eval(0, &mut Vec::new());
    (ctx.config, ctx.warnings)
}

/// Build a [`Config`] from a bundle, following `require`s in evaluation order.
#[must_use]
pub fn bundle_to_config(bundle: &LuaBundle, schema: &Schema) -> (Config, Vec<LuaWarning>) {
    let mut ctx = Ctx::new(schema, &bundle.documents, &bundle.by_path);
    ctx.config.source = bundle.root().path.clone();
    ctx.eval(bundle.root, &mut Vec::new());
    (ctx.config, ctx.warnings)
}

struct Ctx<'a> {
    schema: &'a Schema,
    docs: &'a [LuaDocument],
    by_path: &'a HashMap<PathBuf, usize>,
    config: Config,
    warnings: Vec<LuaWarning>,
    /// Set while walking an `hl.define_submap` body, so nested binds inherit it.
    submap: Option<String>,
    /// Set while walking an `hl.on("hyprland.start"|"hyprland.shutdown", ...)`
    /// body, which is what distinguishes `exec-once` from `exec`.
    exec_kind: Option<ExecKind>,
}

impl<'a> Ctx<'a> {
    fn new(
        schema: &'a Schema,
        docs: &'a [LuaDocument],
        by_path: &'a HashMap<PathBuf, usize>,
    ) -> Self {
        let mut config = Config::empty();
        config.format = Some(ConfigFormat::Lua);
        Self {
            schema,
            docs,
            by_path,
            config,
            warnings: Vec::new(),
            submap: None,
            exec_kind: None,
        }
    }

    fn eval(&mut self, doc_index: usize, visiting: &mut Vec<usize>) {
        if visiting.contains(&doc_index) {
            return;
        }
        visiting.push(doc_index);

        let doc = &self.docs[doc_index];
        let source = doc.path.clone();
        let stmts: Vec<&Stmt> = doc.ast().nodes().stmts().collect();

        for stmt in stmts {
            self.eval_stmt(stmt, &source, visiting);
        }

        visiting.pop();
    }

    fn eval_stmt(&mut self, stmt: &Stmt, source: &Option<PathBuf>, visiting: &mut Vec<usize>) {
        match stmt {
            Stmt::FunctionCall(fc) => {
                // `hl.on(event, function() ... end)` and
                // `hl.define_submap(name, function() ... end)` carry their
                // payload in a callback body rather than in arguments, so they
                // are matched before the plain-call path.
                if let Some((callee, leading, block)) = callback_call(fc) {
                    if self.eval_callback(&callee, leading.as_deref(), block, source, visiting) {
                        return;
                    }
                }
                match callee_path(fc) {
                    Some((callee, args)) => self.eval_call(&callee, &args, source, visiting),
                    None => self.dynamic(stmt),
                }
            }
            Stmt::LocalAssignment(la) => {
                if let Some((name, value)) = simple_local(la) {
                    let prov = self.provenance(source);
                    self.config
                        .variables
                        .push(Tracked::with_provenance(Variable { name, value }, prov));
                } else {
                    self.dynamic(stmt);
                }
            }
            _ => self.dynamic(stmt),
        }
    }

    fn eval_call(
        &mut self,
        callee: &str,
        args: &[LuaVal],
        source: &Option<PathBuf>,
        visiting: &mut Vec<usize>,
    ) {
        match callee {
            "require" => self.follow_require(args, source, visiting),
            "hl.config" => self.eval_config(args, source),
            "hl.bind" => self.eval_bind(args, source),
            "hl.window_rule" => self.eval_window_rule(args, source),
            "hl.layer_rule" => self.eval_layer_rule(args, source),
            "hl.monitor" => self.eval_monitor(args, source),
            "hl.workspace_rule" => self.eval_workspace_rule(args, source),
            "hl.env" => self.eval_env(args, source),
            "hl.exec_cmd" => self.eval_exec(args, source),
            "hl.animation" => self.eval_animation(args, source),
            "hl.curve" => self.eval_curve(args, source),
            "hl.device" => self.eval_device(args, source),
            "hl.gesture" => self.eval_gesture(args, source),
            "hl.permission" => self.eval_permission(args, source),
            "hl.plugin.load" => match str_arg(args, 0) {
                Some(path) => {
                    let prov = self.provenance(source);
                    self.config
                        .plugins
                        .push(Tracked::with_provenance(Plugin { path }, prov));
                }
                None => self.unparsable("hl.plugin.load", "expected a path string"),
            },
            other => self.warnings.push(LuaWarning::DynamicRegion {
                snippet: format!("{other}(...)"),
            }),
        }
    }

    /// `hl.device({ name = "…", sensitivity = -0.5, … })`.
    fn eval_device(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.device", "expected a table");
            return;
        };
        let Some(name) = field_str(fields, "name").filter(|n| !n.is_empty()) else {
            self.unparsable("hl.device", "missing `name`");
            return;
        };
        let options = fields
            .iter()
            .filter_map(|f| {
                let key = f.key.as_deref().filter(|k| *k != "name")?;
                Some((key.to_string(), render_flat(&f.value)))
            })
            .collect();
        let prov = self.provenance(source);
        self.config
            .devices
            .push(Tracked::with_provenance(Device { name, options }, prov));
    }

    /// `hl.gesture({ fingers, direction, action, mods?, scale?, … })`, mapped
    /// back to the `.conf` action spelling and argument list.
    fn eval_gesture(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.gesture", "expected a table");
            return;
        };
        let fingers = field_str(fields, "fingers").and_then(|f| f.parse::<u32>().ok());
        let direction = field_str(fields, "direction");
        // A function (or start/update/finish table) action is code, not data.
        let action = match field(fields, "action") {
            Some(LuaVal::Str(a)) => Some(a.clone()),
            _ => None,
        };
        let (Some(fingers), Some(direction), Some(action)) = (fingers, direction, action) else {
            self.warnings.push(LuaWarning::DynamicRegion {
                snippet: "hl.gesture({ … action = function … })".to_string(),
            });
            return;
        };
        let action = match action.as_str() {
            "cursor_zoom" => "cursorZoom".to_string(),
            "scroll_move" => "scrollMove".to_string(),
            _ => action,
        };
        let mode = field_str(fields, "mode").unwrap_or_default();
        let args = match action.as_str() {
            "special" => field_str(fields, "workspace_name").unwrap_or_default(),
            "cursorZoom" => {
                let level = field_str(fields, "zoom_level").unwrap_or_default();
                if mode.is_empty() {
                    level
                } else {
                    format!("{level}, {mode}")
                }
            }
            _ => mode,
        };
        let prov = self.provenance(source);
        self.config.gestures.push(Tracked::with_provenance(
            Gesture {
                fingers,
                direction,
                mods: field_str(fields, "mods").unwrap_or_default(),
                scale: field_str(fields, "scale").and_then(|s| s.parse().ok()),
                action,
                args,
                bypass_inhibit: field_bool(fields, "disable_inhibit").unwrap_or(false),
            },
            prov,
        ));
    }

    /// `hl.permission({ binary, type, mode })` or `hl.permission(binary, type, mode)`.
    fn eval_permission(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let triple = match args.first() {
            Some(LuaVal::Table(fields)) => (
                field_str(fields, "binary"),
                field_str(fields, "type"),
                field_str(fields, "mode"),
            ),
            _ => (str_arg(args, 0), str_arg(args, 1), str_arg(args, 2)),
        };
        let (Some(binary), Some(kind), Some(mode)) = triple else {
            self.unparsable("hl.permission", "expected { binary, type, mode }");
            return;
        };
        let prov = self.provenance(source);
        self.config.permissions.push(Tracked::with_provenance(
            Permission { binary, kind, mode },
            prov,
        ));
    }

    /// Interpret a callback-carrying call. Returns `true` when handled.
    fn eval_callback(
        &mut self,
        callee: &str,
        leading: Option<&str>,
        block: &full_moon::ast::Block,
        source: &Option<PathBuf>,
        visiting: &mut Vec<usize>,
    ) -> bool {
        match callee {
            // `hl.on("hyprland.start", ...)` is how `exec-once` is expressed.
            "hl.on" => {
                let kind = match leading {
                    Some("hyprland.start") => ExecKind::ExecOnce,
                    Some("hyprland.shutdown") => ExecKind::ExecShutdown,
                    // Any other event is genuine runtime logic, not config.
                    _ => return false,
                };
                let prev = self.exec_kind.replace(kind);
                for stmt in block.stmts() {
                    self.eval_stmt(stmt, source, visiting);
                }
                self.exec_kind = prev;
                true
            }
            "hl.define_submap" => {
                let Some(name) = leading else { return false };
                let prev = self.submap.replace(name.to_string());
                for stmt in block.stmts() {
                    self.eval_stmt(stmt, source, visiting);
                }
                self.submap = prev;
                true
            }
            _ => false,
        }
    }

    fn follow_require(
        &mut self,
        args: &[LuaVal],
        source: &Option<PathBuf>,
        visiting: &mut Vec<usize>,
    ) {
        let Some(LuaVal::Str(module)) = args.first() else {
            return;
        };
        let dir = source
            .as_ref()
            .and_then(|p| p.parent())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let candidate = resolve_require(&dir, module);
        match std::fs::canonicalize(&candidate)
            .ok()
            .and_then(|c| self.by_path.get(&c))
        {
            Some(&idx) => self.eval(idx, visiting),
            None => self.warnings.push(LuaWarning::UnresolvedRequire {
                module: module.clone(),
            }),
        }
    }

    fn eval_config(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(LuaVal::Table(fields)) = args.first() else {
            self.warnings.push(LuaWarning::UnparsableCall {
                callee: "hl.config".to_string(),
                reason: "expected a single table argument".to_string(),
            });
            return;
        };
        let mut leaves = Vec::new();
        flatten(self.schema, "", fields, &mut leaves);
        for (path, leaf) in leaves {
            self.record_option(&path, leaf, source);
        }
    }

    fn record_option(&mut self, path: &str, leaf: &LuaVal, source: &Option<PathBuf>) {
        let prov = self.provenance(source);
        match self.schema.option(path) {
            Some(spec) => match luaval_to_value(&spec.value_type, leaf) {
                Ok(value) => {
                    self.config
                        .options
                        .insert(path.to_string(), Tracked::with_provenance(value, prov));
                }
                Err(reason) => {
                    self.warnings.push(LuaWarning::UnparsableValue {
                        path: path.to_string(),
                        value: render_scalar(leaf),
                        reason,
                    });
                    self.config.options.insert(
                        path.to_string(),
                        Tracked::with_provenance(Value::String(render_scalar(leaf)), prov),
                    );
                }
            },
            None => {
                self.warnings.push(LuaWarning::UnknownOption {
                    path: path.to_string(),
                });
                self.config.options.insert(
                    path.to_string(),
                    Tracked::with_provenance(Value::String(render_scalar(leaf)), prov),
                );
            }
        }
    }

    fn eval_bind(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(keys) = str_arg(args, 0) else {
            self.unparsable("hl.bind", "expected (keys, dispatcher[, opts])");
            return;
        };
        // Lua chords are `MOD + MOD + KEY`; the model keeps mods and key apart.
        let (mods, key) = split_chord(&keys);

        let (dispatcher, dargs) = match args.get(1) {
            Some(LuaVal::Call { path, args: dargs }) => {
                let dsp_path = path.strip_prefix("hl.dsp.").unwrap_or(path);
                let (fields, positional) = call_shape(dargs);
                dispatch::from_lua(dsp_path, &fields, &positional).unwrap_or_else(|| {
                    self.warnings.push(LuaWarning::UnparsableCall {
                        callee: "hl.bind".to_string(),
                        reason: format!("unrecognised dispatcher `{path}`"),
                    });
                    (path.clone(), String::new())
                })
            }
            // Tolerate the legacy string form so configs written by older
            // hyprconf builds still open (Hyprland itself rejects it).
            Some(LuaVal::Str(s)) => split_once_trim(s, ' '),
            _ => {
                self.unparsable("hl.bind", "dispatcher must be an hl.dsp.* call");
                (String::new(), String::new())
            }
        };

        let opts = args.get(2).and_then(table_fields);
        let flag = |name: &str| opts.is_some_and(|f| field_bool(f, name).unwrap_or(false));

        let prov = self.provenance(source);
        self.config.keybinds.push(Tracked::with_provenance(
            Keybind {
                flags: KeybindFlags {
                    locked: flag("locked"),
                    release: flag("release"),
                    repeat: flag("repeating"),
                    non_consuming: flag("non_consuming"),
                    // `bindm` is expressed as the `drag` option in Lua.
                    mouse: flag("drag") || flag("mouse"),
                    transparent: flag("transparent"),
                    ignore_mods: flag("ignore_mods"),
                    long_press: flag("long_press"),
                    click: flag("click"),
                    drag: false,
                    dont_inhibit: flag("dont_inhibit"),
                },
                mods,
                key,
                dispatcher,
                args: dargs,
                // A bind's submap comes from the enclosing `hl.define_submap`.
                submap: self
                    .submap
                    .clone()
                    .or_else(|| opts.and_then(|f| field_str(f, "submap"))),
                description: opts
                    .and_then(|f| field_str(f, "description").or_else(|| field_str(f, "desc")))
                    .filter(|d| !d.is_empty()),
            },
            prov,
        ));
    }

    fn eval_window_rule(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.window_rule", "expected a table");
            return;
        };
        let matchers = field_matchers(fields);
        let prov = self.provenance(source);

        // The *rules* are the table's non-reserved fields; `name` is only a
        // handle. One Lua table can therefore carry several `.conf` rules.
        let mut emitted = 0;
        for f in fields {
            let Some(k) = f.key.as_deref() else { continue };
            let value = render_scalar(&f.value);
            if let Some(rule) = rules::window_field_to_conf(k, &value) {
                self.config.window_rules.push(Tracked::with_provenance(
                    WindowRule {
                        v2: true,
                        rule,
                        matchers: matchers.clone(),
                    },
                    prov.clone(),
                ));
                emitted += 1;
            }
        }
        if emitted == 0 {
            self.warnings.push(LuaWarning::UnparsableCall {
                callee: "hl.window_rule".to_string(),
                reason: "table declares no recognised rule fields".to_string(),
            });
        }
    }

    fn eval_layer_rule(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.layer_rule", "expected a table");
            return;
        };
        let namespace = field_match_key(fields, "namespace");
        let prov = self.provenance(source);

        let mut emitted = 0;
        for f in fields {
            let Some(k) = f.key.as_deref() else { continue };
            let value = render_scalar(&f.value);
            if let Some(rule) = rules::layer_field_to_conf(k, &value) {
                self.config.layer_rules.push(Tracked::with_provenance(
                    LayerRule {
                        rule,
                        namespace: namespace.clone(),
                    },
                    prov.clone(),
                ));
                emitted += 1;
            }
        }
        if emitted == 0 {
            self.warnings.push(LuaWarning::UnparsableCall {
                callee: "hl.layer_rule".to_string(),
                reason: "table declares no recognised rule fields".to_string(),
            });
        }
    }

    fn eval_monitor(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.monitor", "expected a table");
            return;
        };
        let prov = self.provenance(source);
        // Everything past the four positional slots is a trailing conf modifier
        // (`transform, 1`), which Lua spells as its own field.
        const CORE: &[&str] = &["output", "mode", "position", "scale"];
        let mut extra = field_string_array(fields, "extra");
        for f in fields {
            let Some(k) = f.key.as_deref() else { continue };
            if CORE.contains(&k) || k == "extra" {
                continue;
            }
            extra.push(k.to_string());
            extra.push(render_scalar(&f.value));
        }
        self.config.monitors.push(Tracked::with_provenance(
            MonitorRule {
                name: field_str(fields, "output").unwrap_or_default(),
                mode: field_str(fields, "mode").unwrap_or_default(),
                position: field_str(fields, "position").unwrap_or_default(),
                scale: field_str(fields, "scale").unwrap_or_default(),
                extra,
            },
            prov,
        ));
    }

    fn eval_workspace_rule(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.workspace_rule", "expected a table");
            return;
        };
        let prov = self.provenance(source);
        // Workspace rules are flat fields in Lua, one comma-joined string in conf.
        let joined = fields
            .iter()
            .filter_map(|f| {
                let k = f.key.as_deref()?;
                rules::workspace_field_to_conf(k, &render_scalar(&f.value))
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.config.workspaces.push(Tracked::with_provenance(
            WorkspaceRule {
                selector: field_str(fields, "workspace").unwrap_or_default(),
                rules: joined,
            },
            prov,
        ));
    }

    fn eval_env(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let (Some(name), Some(value)) = (str_arg(args, 0), str_arg(args, 1)) else {
            self.unparsable("hl.env", "expected (name, value)");
            return;
        };
        let prov = self.provenance(source);
        self.config
            .env
            .push(Tracked::with_provenance(EnvVar { name, value }, prov));
    }

    fn eval_exec(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(command) = str_arg(args, 0) else {
            self.unparsable("hl.exec_cmd", "expected (command[, opts])");
            return;
        };
        // Bare `hl.exec_cmd` at file scope runs on every load (`exec`); inside
        // an `hl.on("hyprland.start", ...)` block it is `exec-once`.
        let kind = self.exec_kind.unwrap_or(ExecKind::Exec);
        let prov = self.provenance(source);
        self.config
            .execs
            .push(Tracked::with_provenance(Exec { kind, command }, prov));
    }

    /// `hl.animation({ leaf, enabled, speed, bezier|spring, style? })`.
    fn eval_animation(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable(
                "hl.animation",
                "expected a table, e.g. { leaf = \"global\", enabled = true, speed = 5, bezier = \"default\" }",
            );
            return;
        };
        let Some(name) = field_str(fields, "leaf") else {
            self.unparsable("hl.animation", "missing `leaf`");
            return;
        };
        let prov = self.provenance(source);
        self.config.animations.push(Tracked::with_provenance(
            Animation {
                name,
                enabled: field_bool(fields, "enabled").unwrap_or(true),
                speed: field_str(fields, "speed")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0),
                // Hyprland 0.56 supports spring curves alongside beziers; the
                // conf model has one `curve` slot, so either populates it.
                curve: field_str(fields, "bezier")
                    .or_else(|| field_str(fields, "spring"))
                    .unwrap_or_default(),
                style: field_str(fields, "style").filter(|s| !s.is_empty()),
            },
            prov,
        ));
    }

    /// `hl.curve("name", { type = "bezier", points = { {x0,y0}, {x1,y1} } })`.
    fn eval_curve(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(name) = str_arg(args, 0) else {
            self.unparsable("hl.curve", "expected (name, spec)");
            return;
        };
        let Some(spec) = args.get(1).and_then(table_fields) else {
            self.unparsable(
                "hl.curve",
                "expected a spec table, e.g. { type = \"bezier\", points = { {0,0}, {1,1} } }",
            );
            return;
        };

        // Spring curves have no bezier control points; keep them as a dynamic
        // region rather than inventing coordinates.
        if field_str(spec, "type").as_deref() == Some("spring") {
            self.warnings.push(LuaWarning::DynamicRegion {
                snippet: format!("hl.curve(\"{name}\", {{ type = \"spring\", ... }})"),
            });
            return;
        }

        let Some(LuaVal::Table(points)) = field(spec, "points") else {
            self.unparsable("hl.curve", "missing `points`");
            return;
        };
        let point = |i: usize| -> Option<Vec2> {
            let LuaVal::Table(pair) = &points.get(i)?.value else {
                return None;
            };
            let n = |j: usize| -> Option<f64> { render_scalar(&pair.get(j)?.value).parse().ok() };
            Some(Vec2::new(n(0)?, n(1)?))
        };
        let (Some(p0), Some(p1)) = (point(0), point(1)) else {
            self.unparsable("hl.curve", "expected two control points");
            return;
        };

        let prov = self.provenance(source);
        self.config
            .beziers
            .push(Tracked::with_provenance(Bezier { name, p0, p1 }, prov));
    }

    fn dynamic(&mut self, stmt: &Stmt) {
        self.warnings.push(LuaWarning::DynamicRegion {
            snippet: stmt.to_string().trim().to_string(),
        });
    }

    fn unparsable(&mut self, callee: &str, reason: &str) {
        self.warnings.push(LuaWarning::UnparsableCall {
            callee: callee.to_string(),
            reason: reason.to_string(),
        });
    }

    fn provenance(&self, source: &Option<PathBuf>) -> Provenance {
        Provenance {
            source: source.clone(),
            ..Provenance::default()
        }
    }
}

// ---------------------------------------------------------------------------
// free helpers
// ---------------------------------------------------------------------------

/// Flatten a nested config table into `(dotted_path, leaf)` pairs.
///
/// Recursion stops as soon as a path names a real option, because some option
/// *values* are themselves tables — a gradient is
/// `{ colors = { ... }, angle = 45 }`. Descending into one would invent bogus
/// `general:col.active_border:angle` options instead of reading the gradient.
fn flatten<'a>(
    schema: &Schema,
    prefix: &str,
    fields: &'a [LuaField],
    out: &mut Vec<(String, &'a LuaVal)>,
) {
    for field in fields {
        let Some(key) = field.key.as_deref() else {
            continue; // positional entries are not options
        };
        // Colour options are named `col.<x>` (`general:col.active_border`), and
        // Lua spells that as a nested `col = { active_border = … }` table —
        // which is exactly how Hyprland's own example config writes it.
        // Joining with `:` invented a `general:col:active_border:angle` option
        // and silently lost the gradient's colours.
        let path = if prefix.is_empty() {
            key.to_string()
        } else if prefix == "col" || prefix.ends_with(":col") {
            format!("{prefix}.{key}")
        } else {
            format!("{prefix}:{key}")
        };
        match &field.value {
            LuaVal::Table(sub) if schema.option(&path).is_none() => {
                flatten(schema, &path, sub, out);
            }
            leaf => out.push((path, leaf)),
        }
    }
}

fn luaval_to_value(value_type: &ValueType, leaf: &LuaVal) -> Result<Value, String> {
    match value_type {
        ValueType::Bool => match leaf {
            LuaVal::Bool(b) => Ok(Value::Bool(*b)),
            LuaVal::Str(s) => parse_bool(s)
                .map(Value::Bool)
                .ok_or_else(|| "expected a boolean".into()),
            LuaVal::Num(n) => Ok(Value::Bool(n != "0")),
            _ => Err("expected a boolean".into()),
        },
        ValueType::Int => num_str(leaf)?
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|e| e.to_string()),
        ValueType::Float => num_str(leaf)?
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| e.to_string()),
        ValueType::Color => Color::from_hyprland_str(&expect_str(leaf)?)
            .map(Value::Color)
            .map_err(|e| e.to_string()),
        // A gradient is either a plain colour string or Hyprland's
        // `{ colors = { "rgba(..)", ... }, angle = 45 }` table.
        ValueType::Gradient => match leaf {
            LuaVal::Table(fields) => gradient_from_table(fields),
            other => Gradient::from_hyprland_str(&expect_str(other)?)
                .map(Value::Gradient)
                .map_err(|e| e.to_string()),
        },
        ValueType::Vec2 => Vec2::from_hyprland_str(&expect_str(leaf)?)
            .map(Value::Vec2)
            .map_err(|e| e.to_string()),
        ValueType::String => Ok(Value::String(render_scalar(leaf))),
        ValueType::Enum(variants) => {
            // Accept both string literals (`layout = "dwindle"`) and numeric
            // literals (`follow_mouse = 1`), since Hyprland's integer "mode"
            // options are modelled as enums whose variant names are integers.
            let s = match leaf {
                LuaVal::Str(s) => s.clone(),
                LuaVal::Num(n) => n.clone(),
                // Integer "mode" options accept a boolean (`true` is `1`).
                LuaVal::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
                other => return Err(format!("expected a string or number, found {other:?}")),
            };
            if variants.iter().any(|v| v.name == s) {
                Ok(Value::Enum(s))
            } else {
                crate::conf::bool_as_mode(variants, &s)
                    .map(Value::Enum)
                    .ok_or_else(|| format!("`{s}` is not a valid variant"))
            }
        }
        // `gaps_in = 5` or `gaps_in = { top = 5, right = 10, … }`.
        ValueType::CssGap => match leaf {
            LuaVal::Table(fields) => {
                let side = |name: &str| -> Result<i64, String> {
                    match field_str(fields, name) {
                        None => Ok(0),
                        Some(text) => text
                            .parse::<f64>()
                            .map(|f| f.round() as i64)
                            .map_err(|_| format!("`{name}` is not a number")),
                    }
                };
                Ok(Value::CssGap(CssGap::from_sides([
                    side("top")?,
                    side("right")?,
                    side("bottom")?,
                    side("left")?,
                ])))
            }
            other => CssGap::from_hyprland_str(&render_scalar(other))
                .map(Value::CssGap)
                .map_err(|e| e.to_string()),
        },
        _ => Err("option has a non-scalar value type".into()),
    }
}

/// Read Hyprland's `{ colors = { ... }, angle = N }` gradient table.
fn gradient_from_table(fields: &[LuaField]) -> Result<Value, String> {
    let Some(LuaVal::Table(items)) = field(fields, "colors") else {
        return Err("gradient table needs a `colors` array".into());
    };
    let mut stops = Vec::with_capacity(items.len());
    for item in items {
        let text = render_scalar(&item.value);
        stops.push(Color::from_hyprland_str(&text).map_err(|e| e.to_string())?);
    }
    if stops.is_empty() {
        return Err("gradient has no colors".into());
    }
    let angle_deg = field_str(fields, "angle").and_then(|a| a.parse::<f64>().ok());
    Ok(Value::Gradient(Gradient { stops, angle_deg }))
}

fn expect_str(leaf: &LuaVal) -> Result<String, String> {
    match leaf {
        LuaVal::Str(s) => Ok(s.clone()),
        other => Err(format!("expected a string, found {other:?}")),
    }
}

fn num_str(leaf: &LuaVal) -> Result<String, String> {
    match leaf {
        LuaVal::Num(n) => Ok(n.clone()),
        LuaVal::Str(s) => Ok(s.clone()),
        other => Err(format!("expected a number, found {other:?}")),
    }
}

fn render_scalar(leaf: &LuaVal) -> String {
    match leaf {
        LuaVal::Str(s) => s.clone(),
        LuaVal::Num(n) => n.clone(),
        LuaVal::Bool(b) => b.to_string(),
        LuaVal::Nil => "nil".to_string(),
        LuaVal::Table(_) | LuaVal::Call { .. } | LuaVal::Other => String::new(),
    }
}

/// Like [`render_scalar`], but a positional table (`{ 100, 100 }`, a vec2) is
/// flattened to `.conf`'s space-separated form.
fn render_flat(value: &LuaVal) -> String {
    match value {
        LuaVal::Table(items) if items.iter().all(|f| f.key.is_none()) => items
            .iter()
            .map(|f| render_scalar(&f.value))
            .collect::<Vec<_>>()
            .join(" "),
        other => render_scalar(other),
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn table_fields(value: &LuaVal) -> Option<&[LuaField]> {
    match value {
        LuaVal::Table(fields) => Some(fields),
        _ => None,
    }
}

fn field<'a>(fields: &'a [LuaField], name: &str) -> Option<&'a LuaVal> {
    fields
        .iter()
        .find(|f| f.key.as_deref() == Some(name))
        .map(|f| &f.value)
}

fn field_str(fields: &[LuaField], name: &str) -> Option<String> {
    field(fields, name).map(render_scalar)
}

fn field_bool(fields: &[LuaField], name: &str) -> Option<bool> {
    match field(fields, name)? {
        LuaVal::Bool(b) => Some(*b),
        LuaVal::Str(s) => parse_bool(s),
        _ => None,
    }
}

fn field_string_array(fields: &[LuaField], name: &str) -> Vec<String> {
    match field(fields, name) {
        Some(LuaVal::Table(items)) => items.iter().map(|f| render_scalar(&f.value)).collect(),
        _ => Vec::new(),
    }
}

/// Read the `match` field as a `.conf` matcher string.
///
/// The canonical Lua form is a table (`match = { class = "^(kitty)$" }`); a bare
/// string is also accepted for configs written by older hyprconf builds.
fn field_matchers(fields: &[LuaField]) -> String {
    match field(fields, "match") {
        Some(LuaVal::Str(s)) => s.clone(),
        Some(LuaVal::Table(items)) => {
            let pairs: Vec<(String, String)> = items
                .iter()
                .filter_map(|f| f.key.as_ref().map(|k| (k.clone(), render_scalar(&f.value))))
                .collect();
            rules::matchers_to_conf(&pairs)
        }
        _ => String::new(),
    }
}

/// Read a single key out of a `match` table (layer rules match on `namespace`).
fn field_match_key(fields: &[LuaField], want: &str) -> String {
    match field(fields, "match") {
        Some(LuaVal::Str(s)) => s.clone(),
        Some(LuaVal::Table(items)) => items
            .iter()
            .find(|f| f.key.as_deref() == Some(want))
            .map(|f| render_scalar(&f.value))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Split a Lua chord (`"SUPER + SHIFT + Q"`) into `(mods, key)`.
fn split_chord(chord: &str) -> (String, String) {
    let parts: Vec<&str> = chord
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    match parts.split_last() {
        Some((key, mods)) => ((*mods).join(" "), (*key).to_string()),
        None => (String::new(), String::new()),
    }
}

/// Split a dispatcher call's arguments into named fields and positional values,
/// the shape [`dispatch::from_lua`] consumes.
fn call_shape(args: &[LuaVal]) -> (Vec<(String, String)>, Vec<String>) {
    let mut fields = Vec::new();
    let mut positional = Vec::new();
    for arg in args {
        match arg {
            LuaVal::Table(items) => {
                for f in items {
                    match f.key.as_deref() {
                        Some(k) => fields.push((k.to_string(), render_scalar(&f.value))),
                        None => positional.push(render_scalar(&f.value)),
                    }
                }
            }
            other => positional.push(render_scalar(other)),
        }
    }
    (fields, positional)
}

fn str_arg(args: &[LuaVal], i: usize) -> Option<String> {
    match args.get(i)? {
        LuaVal::Str(s) => Some(s.clone()),
        LuaVal::Num(n) => Some(n.clone()),
        _ => None,
    }
}

fn split_once_trim(s: &str, sep: char) -> (String, String) {
    match s.split_once(sep) {
        Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
        None => (s.trim().to_string(), String::new()),
    }
}

/// If a `local` assignment is exactly `local NAME = <literal>`, return it as a
/// variable; otherwise `None` (it is dynamic).
fn simple_local(la: &full_moon::ast::LocalAssignment) -> Option<(String, String)> {
    if la.names().len() != 1 || la.expressions().len() != 1 {
        return None;
    }
    let name = match la.names().iter().next()?.token().token_type() {
        full_moon::tokenizer::TokenType::Identifier { identifier } => {
            identifier.as_str().to_string()
        }
        _ => return None,
    };
    let value = match expr_to_luaval(la.expressions().iter().next()?) {
        LuaVal::Str(s) => s,
        LuaVal::Num(n) => n,
        LuaVal::Bool(b) => b.to_string(),
        _ => return None,
    };
    Some((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::{LuaParser, LuaSerializer};

    fn map(text: &str) -> (Config, Vec<LuaWarning>) {
        let doc = LuaParser::parse_str(text, None).expect("valid Lua");
        document_to_config(&doc, Schema::shared())
    }

    /// The shapes Hyprland's own example config uses — each of which used to
    /// be lost (or garbled) on the way through hyprconf.
    #[test]
    fn reads_the_shapes_the_stock_config_uses() {
        let (config, warnings) = map(r#"
hl.config({
    general = {
        gaps_in = { top = 1, right = 2, bottom = 3, left = 4 },
        col = {
            active_border = { colors = { "rgba(33ccffee)", "rgba(00ff99ee)" }, angle = 45 },
            inactive_border = "rgba(595959aa)",
        },
    },
    cursor = { no_hardware_cursors = true },
})
hl.gesture({ fingers = 3, direction = "horizontal", action = "workspace" })
hl.gesture({ fingers = 2, direction = "pinchin", action = "cursor_zoom", zoom_level = 1.5, mode = "mult" })
hl.device({ name = "epic-mouse-v1", sensitivity = -0.5, region_size = { 100, 50 } })
hl.device({ name = "pad", tap_to_click = false })
hl.permission("/usr/bin/grim", "screencopy", "allow")
hl.plugin.load("/opt/hy3.so")
"#);
        assert!(warnings.is_empty(), "{warnings:?}");

        match config.get("general:col.active_border") {
            Some(Value::Gradient(g)) => {
                assert_eq!(g.stops.len(), 2);
                assert_eq!(g.angle_deg, Some(45.0));
            }
            other => panic!("gradient lost: {other:?}"),
        }
        assert!(matches!(
            config.get("general:col.inactive_border"),
            Some(Value::Gradient(_))
        ));
        assert_eq!(
            config.get("general:gaps_in"),
            Some(&Value::CssGap(CssGap::from_sides([1, 2, 3, 4])))
        );
        assert_eq!(
            config.get("cursor:no_hardware_cursors"),
            Some(&Value::Enum("1".into()))
        );

        assert_eq!(config.gestures.len(), 2);
        assert_eq!(config.gestures[1].value.action, "cursorZoom");
        assert_eq!(config.gestures[1].value.args, "1.5, mult");
        assert_eq!(config.devices.len(), 2);
        assert_eq!(
            config.devices[0].value.options[1],
            ("region_size".to_string(), "100 50".to_string())
        );
        assert_eq!(config.permissions.len(), 1);
        assert_eq!(config.plugins[0].value.path, "/opt/hy3.so");

        // And it all survives regeneration.
        let lua = LuaSerializer::serialize(&config);
        for needle in [
            "rgba(00ff99ee)",
            "angle = 45",
            "top = 1",
            "no_hardware_cursors = 1",
            "action = \"cursor_zoom\"",
            "zoom_level = 1.5",
            "hl.device({ name = \"pad\", tap_to_click = false })",
            "hl.permission({ binary = \"/usr/bin/grim\", type = \"screencopy\", mode = \"allow\" })",
            "hl.plugin.load(\"/opt/hy3.so\")",
        ] {
            assert!(lua.contains(needle), "missing {needle:?} in:\n{lua}");
        }
        let (again, _) = map(&lua);
        assert_eq!(
            again.get("general:col.active_border"),
            config.get("general:col.active_border")
        );
        assert_eq!(again.gestures, config.gestures);
        assert_eq!(again.devices, config.devices);
    }

    #[test]
    fn bind_descriptions_and_extra_flags_survive() {
        let (config, _) = map(
            r#"hl.bind("SUPER + Q", hl.dsp.window.close(), { description = "Close", long_press = true })"#,
        );
        let kb = &config.keybinds[0].value;
        assert_eq!(kb.description.as_deref(), Some("Close"));
        assert!(kb.flags.long_press);
        let lua = LuaSerializer::serialize(&config);
        assert!(lua.contains("description = \"Close\""), "{lua}");
        assert!(lua.contains("long_press = true"), "{lua}");
    }
}
