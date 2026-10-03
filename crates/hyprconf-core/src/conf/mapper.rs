// SPDX-License-Identifier: MIT OR Apache-2.0
//! Builds the semantic [`Config`] from a parsed `.conf` document/bundle.
//!
//! This is where the lossless document is interpreted: `$variables` are
//! expanded, scalar assignments are mapped onto the [`Schema`]'s [`OptionSpec`]s
//! and parsed into typed [`Value`]s, and the repeatable directives are turned
//! into the structured collections. Includes are followed in evaluation order
//! so later values win, exactly as hyprlang would evaluate them.
//!
//! Nothing is ever dropped: unknown keys and unparseable values are recorded as
//! [`ConfWarning`]s and still stored (unknown scalars as `Value::String`), while
//! the lossless document remains the source of truth for serialization.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::conf::document::{Assignment, ConfDocument, Directive, LineKind};
use crate::conf::parser::ConfBundle;
use crate::model::{Config, ConfigFormat, Provenance, Tracked};
use crate::schema::{canonical_path, Schema, ValueType};
use crate::structured::{
    Animation, Bezier, Device, EnvVar, Exec, ExecKind, Gesture, Keybind, KeybindFlags, LayerRule,
    MonitorRule, Permission, Plugin, Submap, Variable, WindowRule, WorkspaceRule,
};
use crate::value::{Color, CssGap, Gradient, Value, Vec2};

/// A non-fatal issue encountered while mapping a document onto the schema.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfWarning {
    /// An assignment used a key the schema does not know about. It is kept
    /// verbatim in the document and stored as a `String` value.
    #[error("unknown option `{path}`{}", at(.file, *.line))]
    UnknownOption {
        /// The unknown dotted path.
        path: String,
        /// The source file, if known.
        file: Option<PathBuf>,
        /// The 1-based line number, if known.
        line: Option<u32>,
    },

    /// A known option's value could not be parsed into its declared type.
    #[error("could not parse value {value:?} for `{path}`: {reason}{}", at(.file, *.line))]
    UnparsableValue {
        /// The option path.
        path: String,
        /// The offending value text.
        value: String,
        /// Why parsing failed.
        reason: String,
        /// The source file, if known.
        file: Option<PathBuf>,
        /// The 1-based line number, if known.
        line: Option<u32>,
    },

    /// A directive's arguments could not be parsed.
    #[error("could not parse {keyword} directive {args:?}: {reason}{}", at(.file, *.line))]
    UnparsableDirective {
        /// The directive keyword.
        keyword: String,
        /// The raw argument text.
        args: String,
        /// Why parsing failed.
        reason: String,
        /// The source file, if known.
        file: Option<PathBuf>,
        /// The 1-based line number, if known.
        line: Option<u32>,
    },
}

fn at(file: &Option<PathBuf>, line: Option<u32>) -> String {
    match (file, line) {
        (Some(p), Some(l)) => format!(" ({}:{l})", p.display()),
        (Some(p), None) => format!(" ({})", p.display()),
        (None, Some(l)) => format!(" (line {l})"),
        (None, None) => String::new(),
    }
}

/// Build a [`Config`] from a single document (does not follow includes).
#[must_use]
pub fn document_to_config(document: &ConfDocument, schema: &Schema) -> (Config, Vec<ConfWarning>) {
    let docs = std::slice::from_ref(document);
    let mut ctx = Ctx::new(schema, docs);
    ctx.config.source = document.path.clone();
    ctx.eval(0, &mut Vec::new());
    (ctx.config, ctx.warnings)
}

/// Build a [`Config`] from a full bundle, following includes in evaluation
/// order (so later assignments override earlier ones).
#[must_use]
pub fn bundle_to_config(bundle: &ConfBundle, schema: &Schema) -> (Config, Vec<ConfWarning>) {
    let mut ctx = Ctx::new(schema, &bundle.documents);
    ctx.config.source = bundle.root().path.clone();
    ctx.eval(bundle.root, &mut Vec::new());
    (ctx.config, ctx.warnings)
}

/// A `windowrule { … }` / `layerrule { … }` block being collected.
///
/// Hyprland 0.56's `.conf` grew a block form for rules that looks exactly like a
/// settings section to a line-oriented parser. Treating it as one turns
/// `windowrule { float = yes }` into a bogus `windowrule:float` *option* — the
/// rule silently disappears, which is the worst possible migration outcome.
struct RuleBlock {
    keyword: String,
    fields: Vec<(String, String)>,
    line: u32,
}

struct Ctx<'a> {
    schema: &'a Schema,
    docs: &'a [ConfDocument],
    config: Config,
    vars: HashMap<String, String>,
    submap: Option<String>,
    warnings: Vec<ConfWarning>,
    /// The rule block currently being collected, if any.
    block: Option<RuleBlock>,
    /// The `device { … }` block currently being collected, if any.
    device: Option<(Vec<(String, String)>, Provenance)>,
}

/// Whether a `name { … }` section is really a repeatable rule directive.
fn is_rule_block(name: &str) -> bool {
    matches!(name.trim(), "windowrule" | "windowrulev2" | "layerrule")
}

/// Assemble a [`Device`] from a block's `(key, value)` pairs. `name` must be
/// present; every other key is an option, stored under its canonical spelling.
fn device_from_fields(fields: Vec<(String, String)>) -> Option<Device> {
    let mut name = None;
    let mut options = Vec::new();
    for (key, value) in fields {
        if key == "name" {
            name = Some(value);
        } else {
            options.push((canonical_path(&key).into_owned(), value));
        }
    }
    Some(Device {
        name: name.filter(|n| !n.is_empty())?,
        options,
    })
}

impl<'a> Ctx<'a> {
    fn new(schema: &'a Schema, docs: &'a [ConfDocument]) -> Self {
        let mut config = Config::empty();
        config.format = Some(ConfigFormat::Conf);
        Self {
            schema,
            docs,
            config,
            vars: HashMap::new(),
            submap: None,
            warnings: Vec::new(),
            block: None,
            device: None,
        }
    }

    /// Turn a collected `windowrule`/`layerrule` block into real rule entries.
    ///
    /// One block can carry several rules (`float = yes` *and* `size = 800 600`),
    /// all sharing the block's matchers, so it expands to one entry per rule
    /// field. The Lua serializer merges them back into a single call.
    fn flush_rule_block(&mut self, block: RuleBlock, prov: Provenance, source: &Option<PathBuf>) {
        let layer = block.keyword.trim() == "layerrule";

        // `match:class = foo` -> `class:foo`; a bare `match = ...` is taken
        // verbatim.
        let mut matchers: Vec<String> = Vec::new();
        let mut rules: Vec<(String, String)> = Vec::new();
        for (key, value) in &block.fields {
            let value = unescape_hash(&expand_vars(value, &self.vars));
            match key.split_once(':') {
                Some(("match", field)) => matchers.push(format!("{field}:{value}")),
                _ if key == "match" => matchers.push(value),
                // `name` is a Lua-side handle, not a rule.
                _ if key == "name" || key == "enabled" => {}
                _ => rules.push((key.clone(), value)),
            }
        }
        let matchers = matchers.join(", ");

        if rules.is_empty() {
            self.warnings.push(ConfWarning::UnparsableDirective {
                keyword: block.keyword.clone(),
                args: format!("{} field(s)", block.fields.len()),
                reason: "block declares no rule fields".to_string(),
                file: source.clone(),
                line: Some(block.line),
            });
            return;
        }

        for (key, value) in rules {
            // The block form already uses the Lua field spellings, so reuse the
            // Lua<->conf rule table to recover the `.conf` rule text. Values are
            // still hyprlang's (`float = yes`), so normalise them first.
            let value = crate::lua::rules::normalise_block_value(&key, &value);
            let rule = if layer {
                crate::lua::rules::layer_field_to_conf(&key, &value)
            } else {
                crate::lua::rules::window_field_to_conf(&key, &value)
            };
            let Some(rule) = rule else {
                self.warnings.push(ConfWarning::UnparsableDirective {
                    keyword: block.keyword.clone(),
                    args: format!("{key} = {value}"),
                    reason: format!("`{key}` is not a known rule"),
                    file: source.clone(),
                    line: Some(block.line),
                });
                continue;
            };
            if layer {
                self.config.layer_rules.push(Tracked::with_provenance(
                    LayerRule {
                        rule,
                        namespace: matchers.clone(),
                    },
                    prov.clone(),
                ));
            } else {
                self.config.window_rules.push(Tracked::with_provenance(
                    WindowRule {
                        v2: true,
                        rule,
                        matchers: matchers.clone(),
                    },
                    prov.clone(),
                ));
            }
        }
    }

    fn eval(&mut self, doc_index: usize, visiting: &mut Vec<usize>) {
        if visiting.contains(&doc_index) {
            return; // defensive: parse_file already rejects real cycles
        }
        visiting.push(doc_index);

        let doc = &self.docs[doc_index];
        let source = doc.path.clone();
        let mut pending_comments: Vec<String> = Vec::new();

        for (idx, line) in doc.lines.iter().enumerate() {
            let line_no = (idx + 1) as u32;
            match &line.kind {
                LineKind::Comment => pending_comments.push(line.raw.clone()),
                LineKind::Blank => pending_comments.clear(),
                LineKind::Assignment(a) => {
                    let prov =
                        self.provenance(&source, line_no, &pending_comments, a.trailing.as_str());
                    // Inside a `windowrule { … }` / `layerrule { … }` block the
                    // assignments describe one rule, not scalar options.
                    if let Some(block) = self.block.as_mut() {
                        block
                            .fields
                            .push((a.key.clone(), a.value.trim().to_string()));
                    } else if let Some((fields, _)) = self.device.as_mut() {
                        let value = unescape_hash(&expand_vars(a.value.trim(), &self.vars));
                        fields.push((a.key.clone(), value));
                    } else {
                        self.record_assignment(a, prov);
                    }
                    pending_comments.clear();
                }
                LineKind::Directive(d) => {
                    let prov =
                        self.provenance(&source, line_no, &pending_comments, d.trailing.as_str());
                    self.record_directive(d, prov, &source, line_no);
                    pending_comments.clear();
                }
                LineKind::Source(s) => {
                    pending_comments.clear();
                    let children = s.resolved.clone();
                    for child in children {
                        self.eval(child, visiting);
                    }
                }
                LineKind::SectionOpen { name } => {
                    if self.block.is_none() && self.device.is_none() {
                        if is_rule_block(name) {
                            self.block = Some(RuleBlock {
                                keyword: name.clone(),
                                fields: Vec::new(),
                                line: line_no,
                            });
                        } else if name.trim() == "device" {
                            // Each `device { … }` is one entry; reading it as a
                            // settings section made every block overwrite the
                            // previous one's `device:name`.
                            let prov = self.provenance(&source, line_no, &pending_comments, "");
                            self.device = Some((Vec::new(), prov));
                        }
                    }
                    pending_comments.clear();
                }
                LineKind::SectionClose => {
                    if let Some(block) = self.block.take() {
                        let prov = self.provenance(&source, block.line, &[], "");
                        self.flush_rule_block(block, prov, &source);
                    } else if let Some((fields, prov)) = self.device.take() {
                        match device_from_fields(fields) {
                            Some(device) => self
                                .config
                                .devices
                                .push(Tracked::with_provenance(device, prov)),
                            None => self.warnings.push(ConfWarning::UnparsableDirective {
                                keyword: "device".to_string(),
                                args: String::new(),
                                reason: "device block has no name".to_string(),
                                file: source.clone(),
                                line: prov.line,
                            }),
                        }
                    }
                    pending_comments.clear();
                }
                LineKind::Unknown => {
                    pending_comments.clear();
                }
            }
        }

        visiting.pop();
    }

    fn provenance(
        &self,
        source: &Option<PathBuf>,
        line: u32,
        leading: &[String],
        trailing: &str,
    ) -> Provenance {
        let trailing_comment = trailing.find('#').map(|i| trailing[i..].to_string());
        Provenance {
            source: source.clone(),
            span: None,
            line: Some(line),
            leading_comments: leading.to_vec(),
            trailing_comment,
        }
    }

    fn record_assignment(&mut self, a: &Assignment, prov: Provenance) {
        let expanded = unescape_hash(&expand_vars(&a.value, &self.vars));

        if a.is_variable {
            let name = a.key.trim_start_matches('$').to_string();
            self.vars.insert(name.clone(), expanded.clone());
            self.config.variables.push(Tracked::with_provenance(
                Variable {
                    name,
                    value: expanded,
                },
                prov,
            ));
            return;
        }

        // `device[NAME]:option = value` is the one-line form of a device block;
        // consecutive lines for the same device extend one entry.
        if let Some((name, option)) = a
            .full_path
            .strip_prefix("device[")
            .and_then(|rest| rest.split_once("]:"))
        {
            let option = canonical_path(option).into_owned();
            match self.config.devices.last_mut() {
                Some(last) if last.value.name == name => {
                    last.value.options.push((option, expanded));
                }
                _ => self.config.devices.push(Tracked::with_provenance(
                    Device {
                        name: name.to_string(),
                        options: vec![(option, expanded)],
                    },
                    prov,
                )),
            }
            return;
        }

        // Options are keyed by their canonical spelling (`tap_to_click`), not
        // the `.conf` one (`tap-to-click`) — see `schema::canonical_path`.
        let path = canonical_path(&a.full_path).into_owned();
        match self.schema.option(&path) {
            Some(spec) => match parse_value(&spec.value_type, &expanded) {
                Ok(value) => {
                    self.config
                        .options
                        .insert(path, Tracked::with_provenance(value, prov));
                }
                Err(reason) => {
                    self.warnings.push(ConfWarning::UnparsableValue {
                        path: path.clone(),
                        value: expanded.clone(),
                        reason,
                        file: prov.source.clone(),
                        line: prov.line,
                    });
                    // Keep the value so nothing is lost semantically.
                    self.config.options.insert(
                        path,
                        Tracked::with_provenance(Value::String(expanded), prov),
                    );
                }
            },
            None => {
                self.warnings.push(ConfWarning::UnknownOption {
                    path: path.clone(),
                    file: prov.source.clone(),
                    line: prov.line,
                });
                self.config.options.insert(
                    path,
                    Tracked::with_provenance(Value::String(expanded), prov),
                );
            }
        }
    }

    fn record_directive(
        &mut self,
        d: &Directive,
        prov: Provenance,
        source: &Option<PathBuf>,
        line: u32,
    ) {
        let args = unescape_hash(&expand_vars(&d.args, &self.vars));
        let keyword = d.keyword.as_str();

        match keyword {
            "monitor" => {
                let f = split_commas(&args);
                self.config.monitors.push(Tracked::with_provenance(
                    MonitorRule {
                        name: nth(&f, 0),
                        mode: nth(&f, 1),
                        position: nth(&f, 2),
                        scale: nth(&f, 3),
                        extra: f.iter().skip(4).map(|s| s.trim().to_string()).collect(),
                    },
                    prov,
                ));
            }
            "workspace" => {
                let (selector, rules) = split_once_trim(&args, ',');
                self.config.workspaces.push(Tracked::with_provenance(
                    WorkspaceRule { selector, rules },
                    prov,
                ));
            }
            "windowrule" | "windowrulev2" => {
                let (rule, matchers) = split_once_trim(&args, ',');
                self.config.window_rules.push(Tracked::with_provenance(
                    WindowRule {
                        v2: keyword == "windowrulev2",
                        rule,
                        matchers,
                    },
                    prov,
                ));
            }
            "gesture" | "gesturep" => match Gesture::parse(&args, keyword == "gesturep") {
                Ok(gesture) => self
                    .config
                    .gestures
                    .push(Tracked::with_provenance(gesture, prov)),
                Err(reason) => self.warnings.push(ConfWarning::UnparsableDirective {
                    keyword: keyword.to_string(),
                    args,
                    reason,
                    file: source.clone(),
                    line: Some(line),
                }),
            },
            "permission" => {
                // `BINARY, TYPE, MODE`; the binary regex may itself contain
                // commas only in Lua, so splitting from the right is safest.
                let mut parts: Vec<String> =
                    args.rsplitn(3, ',').map(|s| s.trim().to_string()).collect();
                parts.reverse();
                match parts.as_slice() {
                    [binary, kind, mode] => self.config.permissions.push(Tracked::with_provenance(
                        Permission {
                            binary: binary.clone(),
                            kind: kind.clone(),
                            mode: mode.clone(),
                        },
                        prov,
                    )),
                    _ => self.warnings.push(ConfWarning::UnparsableDirective {
                        keyword: keyword.to_string(),
                        args,
                        reason: "expected BINARY, TYPE, MODE".to_string(),
                        file: source.clone(),
                        line: Some(line),
                    }),
                }
            }
            "plugin" => self.config.plugins.push(Tracked::with_provenance(
                Plugin {
                    path: args.trim().to_string(),
                },
                prov,
            )),
            "layerrule" => {
                let (rule, namespace) = split_once_trim(&args, ',');
                self.config.layer_rules.push(Tracked::with_provenance(
                    LayerRule { rule, namespace },
                    prov,
                ));
            }
            "env" | "envd" => {
                let (name, value) = split_once_trim(&args, ',');
                self.config
                    .env
                    .push(Tracked::with_provenance(EnvVar { name, value }, prov));
            }
            "exec" | "exec-once" | "exec-shutdown" | "execr" | "exec-once-r" => {
                let kind = match keyword {
                    "exec-once" | "exec-once-r" => ExecKind::ExecOnce,
                    "exec-shutdown" => ExecKind::ExecShutdown,
                    _ => ExecKind::Exec,
                };
                self.config.execs.push(Tracked::with_provenance(
                    Exec {
                        kind,
                        command: args,
                    },
                    prov,
                ));
            }
            "submap" => {
                if args == "reset" {
                    self.submap = None;
                } else {
                    self.submap = Some(args.clone());
                }
                self.config
                    .submaps
                    .push(Tracked::with_provenance(Submap { name: args }, prov));
            }
            "bezier" => match parse_bezier(&args) {
                Ok(bezier) => self
                    .config
                    .beziers
                    .push(Tracked::with_provenance(bezier, prov)),
                Err(reason) => self.warnings.push(ConfWarning::UnparsableDirective {
                    keyword: keyword.to_string(),
                    args,
                    reason,
                    file: source.clone(),
                    line: Some(line),
                }),
            },
            "animation" => match parse_animation(&args) {
                Ok(animation) => self
                    .config
                    .animations
                    .push(Tracked::with_provenance(animation, prov)),
                Err(reason) => self.warnings.push(ConfWarning::UnparsableDirective {
                    keyword: keyword.to_string(),
                    args,
                    reason,
                    file: source.clone(),
                    line: Some(line),
                }),
            },
            _ if is_bind(keyword) => {
                let letters = keyword.strip_prefix("bind").unwrap_or("");
                // `d` inserts a description as the third field, shifting the
                // dispatcher and its arguments one slot to the right.
                let described = letters.contains('d');
                let f = splitn_commas(&args, if described { 5 } else { 4 });
                let at = |i: usize| nth(&f, if described && i >= 2 { i + 1 } else { i });
                self.config.keybinds.push(Tracked::with_provenance(
                    Keybind {
                        flags: KeybindFlags::from_letters(letters),
                        mods: nth(&f, 0),
                        key: nth(&f, 1),
                        dispatcher: at(2),
                        args: at(3),
                        submap: self.submap.clone(),
                        description: described.then(|| nth(&f, 2)),
                    },
                    prov,
                ));
            }
            _ => {
                // Keywords we recognise as directives but do not yet model
                // (e.g. `plugin`, `permission`) are preserved in the document
                // and surfaced as a warning rather than dropped.
                self.warnings.push(ConfWarning::UnparsableDirective {
                    keyword: keyword.to_string(),
                    args,
                    reason: "directive not modelled".to_string(),
                    file: source.clone(),
                    line: Some(line),
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// value / directive parsing helpers
// ---------------------------------------------------------------------------

fn parse_value(value_type: &ValueType, text: &str) -> Result<Value, String> {
    match value_type {
        ValueType::Bool => parse_bool(text)
            .map(Value::Bool)
            .ok_or_else(|| "expected a boolean".to_string()),
        ValueType::Int => text
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|e| e.to_string()),
        ValueType::Float => text
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| e.to_string()),
        ValueType::Color => Color::from_hyprland_str(text)
            .map(Value::Color)
            .map_err(|e| e.to_string()),
        ValueType::Gradient => Gradient::from_hyprland_str(text)
            .map(Value::Gradient)
            .map_err(|e| e.to_string()),
        ValueType::String => Ok(Value::String(text.to_string())),
        ValueType::Enum(variants) => {
            if variants.iter().any(|v| v.name == text) {
                return Ok(Value::Enum(text.to_string()));
            }
            // Hyprland's integer "mode" options (`cursor:no_hardware_cursors`,
            // `misc:vrr`, ...) are modelled as enums over numeric literals, but
            // hyprlang also accepts a boolean for them (`true` is `1`). Keeping
            // the literal `true` used to fail validation and block saving, so
            // it is normalised to the integer it means. Preserve-mode saves
            // leave the original line untouched unless the user edits it.
            bool_as_mode(variants, text)
                .map(Value::Enum)
                .ok_or_else(|| format!("`{text}` is not a valid variant"))
        }
        ValueType::Vec2 => Vec2::from_hyprland_str(text)
            .map(Value::Vec2)
            .map_err(|e| e.to_string()),
        ValueType::CssGap => CssGap::from_hyprland_str(text)
            .map(Value::CssGap)
            .map_err(|e| e.to_string()),
        _ => Err("option has a non-scalar value type".to_string()),
    }
}

/// Map a boolean written into an integer "mode" enum (`no_hardware_cursors =
/// true`) to the variant it means (`1` / `0`), if the enum has those variants.
pub(crate) fn bool_as_mode(variants: &[crate::schema::EnumVariant], text: &str) -> Option<String> {
    let numeric = variants.iter().all(|v| v.name.parse::<i64>().is_ok());
    let literal = match parse_bool(text)? {
        true => "1",
        false => "0",
    };
    (numeric && variants.iter().any(|v| v.name == literal)).then(|| literal.to_string())
}

/// Parse a hyprlang boolean.
///
/// hyprlang only inspects the leading token, which is why the stock Hyprland
/// config can write `animations { enabled = yes, please :) }` and have it mean
/// `true`. Matching that leniency matters for conversion: treating the value as
/// an opaque string would emit `enabled = "yes, please :)"` into Lua, where the
/// stricter Lua binding rejects it with *"boolean type requires a bool"*.
fn parse_bool(text: &str) -> Option<bool> {
    let trimmed = text.trim();
    let exact = |s: &str| match s.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    };
    if let Some(v) = exact(trimmed) {
        return Some(v);
    }
    // Fall back to the first word, as hyprlang does.
    let head = trimmed
        .split(|c: char| c.is_whitespace() || c == ',')
        .find(|s| !s.is_empty())?;
    exact(head)
}

fn parse_bezier(args: &str) -> Result<Bezier, String> {
    let f = split_commas(args);
    if f.len() < 5 {
        return Err("expected NAME, x0, y0, x1, y1".to_string());
    }
    let num = |s: &str| s.trim().parse::<f64>().map_err(|e| e.to_string());
    Ok(Bezier {
        name: nth(&f, 0),
        p0: Vec2::new(num(&f[1])?, num(&f[2])?),
        p1: Vec2::new(num(&f[3])?, num(&f[4])?),
    })
}

fn parse_animation(args: &str) -> Result<Animation, String> {
    let f = split_commas(args);
    if f.len() < 4 {
        return Err("expected NAME, ONOFF, SPEED, CURVE[, STYLE]".to_string());
    }
    let enabled = parse_bool(&f[1]).unwrap_or(false);
    let speed = f[2].trim().parse::<f64>().map_err(|e| e.to_string())?;
    let style = if f.len() > 4 {
        let joined = f[4..].join(",").trim().to_string();
        (!joined.is_empty()).then_some(joined)
    } else {
        None
    };
    Ok(Animation {
        name: nth(&f, 0),
        enabled,
        speed,
        curve: nth(&f, 3),
        style,
    })
}

fn is_bind(keyword: &str) -> bool {
    keyword == "bind"
        || keyword.strip_prefix("bind").is_some_and(|flags| {
            !flags.is_empty() && flags.bytes().all(|b| b"lrenmtiopdcg".contains(&b))
        })
}

/// Expand `$name` / `${name}` references using `vars`; unknown references are
/// left verbatim (e.g. environment variables we don't resolve here).
fn expand_vars(text: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];

        if let Some(stripped) = after.strip_prefix('{') {
            if let Some(end) = stripped.find('}') {
                let name = &stripped[..end];
                if let Some(value) = vars.get(name) {
                    out.push_str(value);
                    rest = &stripped[end + 1..];
                    continue;
                }
            }
            out.push('$');
            rest = after;
            continue;
        }

        let name_len = after
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0);
        if name_len > 0 {
            if let Some(value) = vars.get(&after[..name_len]) {
                out.push_str(value);
                rest = &after[name_len..];
                continue;
            }
        }

        out.push('$');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Un-escape hyprlang's `##` -> `#`.
fn unescape_hash(text: &str) -> String {
    text.replace("##", "#")
}

fn split_commas(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().to_string()).collect()
}

fn splitn_commas(s: &str, n: usize) -> Vec<String> {
    s.splitn(n, ',').map(|p| p.trim().to_string()).collect()
}

fn split_once_trim(s: &str, sep: char) -> (String, String) {
    match s.split_once(sep) {
        Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
        None => (s.trim().to_string(), String::new()),
    }
}

fn nth(parts: &[String], i: usize) -> String {
    parts
        .get(i)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_known_variables_only() {
        let mut vars = HashMap::new();
        vars.insert("mainMod".to_string(), "SUPER".to_string());
        assert_eq!(expand_vars("$mainMod, Q", &vars), "SUPER, Q");
        assert_eq!(expand_vars("${mainMod} SHIFT", &vars), "SUPER SHIFT");
        assert_eq!(expand_vars("$HOME/x", &vars), "$HOME/x");
        assert_eq!(expand_vars("no vars here", &vars), "no vars here");
    }

    #[test]
    fn unescape_hash_collapses_double() {
        assert_eq!(unescape_hash("a##b"), "a#b");
        assert_eq!(unescape_hash("plain"), "plain");
    }

    #[test]
    fn parse_bool_accepts_hyprlang_forms() {
        for t in ["true", "yes", "on", "1"] {
            assert_eq!(parse_bool(t), Some(true));
        }
        for f in ["false", "no", "off", "0"] {
            assert_eq!(parse_bool(f), Some(false));
        }
        assert_eq!(parse_bool("maybe"), None);
        // hyprlang only looks at the leading token, so Hyprland's own
        // `enabled = yes, please :)` easter egg must parse as `true`.
        assert_eq!(parse_bool("yes, please :)"), Some(true));
        assert_eq!(parse_bool("no thanks"), Some(false));
    }

    #[test]
    fn parses_bezier_and_animation() {
        let b = parse_bezier("myCurve, 0.05, 0.9, 0.1, 1.0").unwrap();
        assert_eq!(b.name, "myCurve");
        assert_eq!(b.p0, Vec2::new(0.05, 0.9));

        let a = parse_animation("windows, 1, 7, myCurve, slide").unwrap();
        assert!(a.enabled);
        assert_eq!(a.speed, 7.0);
        assert_eq!(a.curve, "myCurve");
        assert_eq!(a.style.as_deref(), Some("slide"));
    }

    #[test]
    fn bind_flags_parse_from_keyword() {
        let f = KeybindFlags::from_letters("el");
        assert!(f.repeat && f.locked);
        assert!(!f.mouse);
        assert!(is_bind("bind"));
        assert!(is_bind("bindm"));
        assert!(is_bind("bindd"));
        assert!(!is_bind("binds"));
    }

    fn map(text: &str) -> Config {
        let doc = crate::conf::ConfParser::parse_str(text, None);
        document_to_config(&doc, Schema::shared()).0
    }

    #[test]
    fn described_binds_shift_the_dispatcher_and_round_trip() {
        let config = map("bindd = SUPER, Q, Close the window, killactive\n");
        let kb = &config.keybinds[0].value;
        assert_eq!(kb.description.as_deref(), Some("Close the window"));
        assert_eq!(kb.dispatcher, "killactive");
        assert!(kb.args.is_empty());
        let out = crate::conf::config_to_conf(&config);
        assert!(
            out.contains("bindd = SUPER, Q, Close the window, killactive"),
            "{out}"
        );
    }

    #[test]
    fn gestures_devices_permissions_and_plugins_are_collections() {
        let config = map("plugin = /opt/hy3.so\n\
             gesture = 3, horizontal, workspace\n\
             gesturep = 4, down, mod: SUPER, special, magic\n\
             device {\n    name = epic-mouse\n    sensitivity = -0.5\n    tap-to-click = false\n}\n\
             device {\n    name = other-pad\n    natural_scroll = true\n}\n\
             device[late-kbd]:repeat_rate = 50\n\
             permission = /usr/bin/grim, screencopy, allow\n");
        assert_eq!(config.plugins.len(), 1);
        assert_eq!(config.gestures.len(), 2);
        assert!(config.gestures[1].value.bypass_inhibit);
        assert_eq!(config.devices.len(), 3, "each block is its own device");
        assert_eq!(
            config.devices[0].value.options,
            vec![
                ("sensitivity".to_string(), "-0.5".to_string()),
                // Canonical spelling in the model…
                ("tap_to_click".to_string(), "false".to_string()),
            ]
        );
        assert_eq!(config.permissions[0].value.kind, "screencopy");
        assert!(config.options.keys().all(|k| !k.starts_with("device")));

        // …and the `.conf` spelling Hyprland requires on the way out.
        let out = crate::conf::config_to_conf(&config);
        assert!(out.contains("    tap-to-click = false"), "{out}");
        assert!(
            out.contains("gesturep = 4, down, mod: SUPER, special, magic"),
            "{out}"
        );
        assert!(
            out.contains("permission = /usr/bin/grim, screencopy, allow"),
            "{out}"
        );
        assert!(out.contains("plugin = /opt/hy3.so"), "{out}");
    }

    #[test]
    fn aliased_and_typed_options_read_correctly() {
        let config = map(
            "input {\n    touchpad {\n        tap-to-click = false\n    }\n}\n\
             input-capture {\n    capture_modifiers = true\n}\n\
             general {\n    gaps_in = 5 10\n}\n\
             cursor {\n    no_hardware_cursors = true\n}\n",
        );
        assert_eq!(
            config.get("input:touchpad:tap_to_click"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            config.get("input_capture:capture_modifiers"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            config.get("general:gaps_in"),
            Some(&Value::CssGap(CssGap::from_sides([5, 10, 5, 10])))
        );
        // A boolean on an integer mode option means 1 — and must validate.
        assert_eq!(
            config.get("cursor:no_hardware_cursors"),
            Some(&Value::Enum("1".into()))
        );
        assert!(crate::validate_config(Schema::shared(), &config).is_empty());

        let out = crate::conf::config_to_conf(&config);
        assert!(out.contains("input:touchpad:tap-to-click = false"), "{out}");
        assert!(
            out.contains("input-capture:capture_modifiers = true"),
            "{out}"
        );
        assert!(out.contains("general:gaps_in = 5 10"), "{out}");
    }
}
