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

use crate::lua::document::LuaDocument;
use crate::lua::extract::{callee_path, expr_to_luaval, LuaField, LuaVal};
use crate::lua::parser::{resolve_require, LuaBundle};
use crate::model::{Config, ConfigFormat, Provenance, Tracked};
use crate::schema::{Schema, ValueType};
use crate::structured::{
    Animation, Bezier, EnvVar, Exec, ExecKind, Keybind, KeybindFlags, LayerRule, MonitorRule,
    Variable, WindowRule, WorkspaceRule,
};
use crate::value::{Color, Gradient, Value, Vec2};

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
            Stmt::FunctionCall(fc) => match callee_path(fc) {
                Some((callee, args)) => self.eval_call(&callee, &args, source, visiting),
                None => self.dynamic(stmt),
            },
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
            other => self.warnings.push(LuaWarning::DynamicRegion {
                snippet: format!("{other}(...)"),
            }),
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
        flatten("", fields, &mut leaves);
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
        let (Some(keys), Some(disp)) = (str_arg(args, 0), str_arg(args, 1)) else {
            self.unparsable("hl.bind", "expected (keys, dispatcher[, opts])");
            return;
        };
        let (mods, key) = split_once_trim(&keys, ',');
        let (dispatcher, dargs) = split_once_trim(&disp, ' ');

        let opts = args.get(2).and_then(table_fields);
        let flag = |name: &str| opts.is_some_and(|f| field_bool(f, name).unwrap_or(false));
        let submap = opts.and_then(|f| field_str(f, "submap"));

        let prov = self.provenance(source);
        self.config.keybinds.push(Tracked::with_provenance(
            Keybind {
                flags: KeybindFlags {
                    locked: flag("locked"),
                    release: flag("release"),
                    repeat: flag("repeating"),
                    non_consuming: flag("non_consuming"),
                    mouse: flag("mouse"),
                    transparent: flag("transparent"),
                    ignore_mods: flag("ignore_mods"),
                },
                mods,
                key,
                dispatcher,
                args: dargs,
                submap,
            },
            prov,
        ));
    }

    fn eval_window_rule(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.window_rule", "expected a table");
            return;
        };
        let rule = field_str(fields, "name").unwrap_or_default();
        let matchers = field_matchers(fields);
        let v2 = field_bool(fields, "v2").unwrap_or(true);
        let prov = self.provenance(source);
        self.config.window_rules.push(Tracked::with_provenance(
            WindowRule { v2, rule, matchers },
            prov,
        ));
    }

    fn eval_layer_rule(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.layer_rule", "expected a table");
            return;
        };
        let rule = field_str(fields, "name").unwrap_or_default();
        let namespace = field_matchers(fields);
        let prov = self.provenance(source);
        self.config.layer_rules.push(Tracked::with_provenance(
            LayerRule { rule, namespace },
            prov,
        ));
    }

    fn eval_monitor(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(fields) = args.first().and_then(table_fields) else {
            self.unparsable("hl.monitor", "expected a table");
            return;
        };
        let prov = self.provenance(source);
        self.config.monitors.push(Tracked::with_provenance(
            MonitorRule {
                name: field_str(fields, "output").unwrap_or_default(),
                mode: field_str(fields, "mode").unwrap_or_default(),
                position: field_str(fields, "position").unwrap_or_default(),
                scale: field_str(fields, "scale").unwrap_or_default(),
                extra: field_string_array(fields, "extra"),
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
        self.config.workspaces.push(Tracked::with_provenance(
            WorkspaceRule {
                selector: field_str(fields, "workspace").unwrap_or_default(),
                rules: field_str(fields, "rules").unwrap_or_default(),
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
        let kind = match args
            .get(1)
            .and_then(table_fields)
            .and_then(|f| field_str(f, "when"))
        {
            Some(w) if w == "reload" => ExecKind::Exec,
            Some(w) if w == "shutdown" => ExecKind::ExecShutdown,
            _ => ExecKind::ExecOnce,
        };
        let prov = self.provenance(source);
        self.config
            .execs
            .push(Tracked::with_provenance(Exec { kind, command }, prov));
    }

    fn eval_animation(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let Some(name) = str_arg(args, 0) else {
            self.unparsable(
                "hl.animation",
                "expected (name, enabled, speed, curve[, style])",
            );
            return;
        };
        let enabled = match args.get(1) {
            Some(LuaVal::Bool(b)) => *b,
            Some(LuaVal::Num(n)) => n != "0",
            _ => false,
        };
        let speed = num_arg(args, 2).unwrap_or(0.0);
        let curve = str_arg(args, 3).unwrap_or_default();
        let style = str_arg(args, 4).filter(|s| !s.is_empty());
        let prov = self.provenance(source);
        self.config.animations.push(Tracked::with_provenance(
            Animation {
                name,
                enabled,
                speed,
                curve,
                style,
            },
            prov,
        ));
    }

    fn eval_curve(&mut self, args: &[LuaVal], source: &Option<PathBuf>) {
        let (Some(name), Some(x0), Some(y0), Some(x1), Some(y1)) = (
            str_arg(args, 0),
            num_arg(args, 1),
            num_arg(args, 2),
            num_arg(args, 3),
            num_arg(args, 4),
        ) else {
            self.unparsable("hl.curve", "expected (name, x0, y0, x1, y1)");
            return;
        };
        let prov = self.provenance(source);
        self.config.beziers.push(Tracked::with_provenance(
            Bezier {
                name,
                p0: Vec2::new(x0, y0),
                p1: Vec2::new(x1, y1),
            },
            prov,
        ));
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
fn flatten<'a>(prefix: &str, fields: &'a [LuaField], out: &mut Vec<(String, &'a LuaVal)>) {
    for field in fields {
        let Some(key) = field.key.as_deref() else {
            continue; // positional entries are not options
        };
        let path = if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}:{key}")
        };
        match &field.value {
            LuaVal::Table(sub) => flatten(&path, sub, out),
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
        ValueType::Gradient => Gradient::from_hyprland_str(&expect_str(leaf)?)
            .map(Value::Gradient)
            .map_err(|e| e.to_string()),
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
                other => return Err(format!("expected a string or number, found {other:?}")),
            };
            if variants.iter().any(|v| v.name == s) {
                Ok(Value::Enum(s))
            } else {
                Err(format!("`{s}` is not a valid variant"))
            }
        }
        _ => Err("option has a non-scalar value type".into()),
    }
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
        LuaVal::Table(_) | LuaVal::Other => String::new(),
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

/// Read the `match` field as a single matcher string (our serializer emits it as
/// a string; a hand-written `match = { k = v }` table is re-joined as `k:v`).
fn field_matchers(fields: &[LuaField]) -> String {
    match field(fields, "match") {
        Some(LuaVal::Str(s)) => s.clone(),
        Some(LuaVal::Table(items)) => items
            .iter()
            .filter_map(|f| {
                f.key
                    .as_ref()
                    .map(|k| format!("{k}:{}", render_scalar(&f.value)))
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

fn str_arg(args: &[LuaVal], i: usize) -> Option<String> {
    match args.get(i)? {
        LuaVal::Str(s) => Some(s.clone()),
        LuaVal::Num(n) => Some(n.clone()),
        _ => None,
    }
}

fn num_arg(args: &[LuaVal], i: usize) -> Option<f64> {
    match args.get(i)? {
        LuaVal::Num(n) => n.parse::<f64>().ok(),
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
