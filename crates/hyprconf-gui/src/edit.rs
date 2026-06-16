// SPDX-License-Identifier: MIT OR Apache-2.0
//! The editing engine: applies user edits to the in-memory [`Config`], tracks
//! per-field drafts, validation errors and dirty state, and supports
//! reset-to-default. This is deliberately UI-free so it can be unit-tested.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hyprconf_core::schema::{CollectionId, NumericRange, OptionSpec, Schema, ValueType};
use hyprconf_core::structured::{
    Animation, Bezier, EnvVar, Exec, ExecKind, Keybind, KeybindFlags, LayerRule, MonitorRule,
    Submap, Variable, WindowRule, WorkspaceRule,
};
use hyprconf_core::value::{Color, Gradient, Vec2};
use hyprconf_core::{Config, Tracked, Value};

use crate::load::Loaded;

/// Which sub-field of a (possibly compound) editor a draft/error belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The single value of a scalar editor.
    Main,
    /// The x component of a [`Vec2`].
    X,
    /// The y component of a [`Vec2`].
    Y,
    /// A color's `rgba(...)` hex field.
    Hex,
    /// A gradient's angle field.
    Angle,
    /// A gradient color stop, by index.
    Stop(usize),
}

/// Identifies one editable field (an option path + a slot).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldId {
    /// The option's dotted path.
    pub path: String,
    /// The sub-field.
    pub slot: Slot,
}

impl FieldId {
    fn new(path: &str, slot: Slot) -> Self {
        Self {
            path: path.to_string(),
            slot,
        }
    }
}

/// A color channel, for the color picker sliders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChannel {
    /// Red.
    R,
    /// Green.
    G,
    /// Blue.
    B,
    /// Alpha.
    A,
}

/// A single edit operation produced by an editor widget.
#[derive(Debug, Clone)]
pub enum EditAction {
    /// Toggle a boolean option.
    SetBool(String, bool),
    /// Choose an enum variant.
    SetEnum(String, String),
    /// Move an integer slider.
    SetIntSlider(String, i64),
    /// Move a float slider.
    SetFloatSlider(String, f64),
    /// Set a whole color at once (from the visual picker's 2D area / hue strip).
    SetColor(String, Color),
    /// Set one gradient stop's color (from the visual picker).
    SetStopColor(String, usize, Color),
    /// Type into a text-based field (path, slot, raw text).
    EditText(String, Slot, String),
    /// Append a gradient color stop.
    AddStop(String),
    /// Remove a gradient color stop by index.
    RemoveStop(String, usize),
    /// Reset an option to its schema default.
    Reset(String),
}

impl Loaded {
    /// The current effective value of an option (edited value, or its default).
    #[must_use]
    pub fn value_for(&self, opt: &OptionSpec) -> Value {
        self.config
            .get(&opt.path)
            .cloned()
            .unwrap_or_else(|| opt.default.clone())
    }

    /// Whether an option currently differs from its load-time baseline.
    #[must_use]
    pub fn is_dirty(&self, path: &str) -> bool {
        self.dirty.contains(path)
    }

    /// The in-progress draft text for a field, if the user has typed into it.
    #[must_use]
    pub fn draft(&self, path: &str, slot: Slot) -> Option<&str> {
        self.drafts
            .get(&FieldId::new(path, slot))
            .map(String::as_str)
    }

    /// The validation error for a specific field, if any.
    #[must_use]
    pub fn field_error(&self, path: &str, slot: Slot) -> Option<&str> {
        self.errors
            .get(&FieldId::new(path, slot))
            .map(String::as_str)
    }

    /// Any validation error for an option (across its slots).
    #[must_use]
    pub fn first_error(&self, path: &str) -> Option<&str> {
        self.errors
            .iter()
            .find(|(id, _)| id.path == path)
            .map(|(_, msg)| msg.as_str())
    }

    /// The list of pending changes as `(path, baseline, current)` text triples,
    /// sorted by path — the "debug pending diff" surface.
    #[must_use]
    pub fn pending_diff(&self) -> Vec<(String, String, String)> {
        let mut diff: Vec<_> = self
            .dirty
            .iter()
            .map(|path| {
                let base = self.baseline.get(path).map(value_text).unwrap_or_default();
                let current = self.config.get(path).map(value_text).unwrap_or_default();
                (path.clone(), base, current)
            })
            .collect();
        diff.sort_by(|a, b| a.0.cmp(&b.0));
        diff
    }

    /// Apply an [`EditAction`] to the model.
    pub fn apply(&mut self, action: EditAction, schema: &Schema) {
        match action {
            EditAction::SetBool(path, b) => self.commit(&path, Value::Bool(b)),
            EditAction::SetEnum(path, v) => self.commit(&path, Value::Enum(v)),
            EditAction::SetIntSlider(path, i) => {
                self.commit(&path, Value::Int(i));
                self.set_draft(&path, Slot::Main, i.to_string());
            }
            EditAction::SetFloatSlider(path, x) => {
                self.commit(&path, Value::Float(x));
                self.set_draft(&path, Slot::Main, fmt_num(x));
            }
            EditAction::SetColor(path, color) => {
                self.commit(&path, Value::Color(color));
                self.set_draft(&path, Slot::Hex, color.to_rgba_string());
            }
            EditAction::SetStopColor(path, index, color) => {
                self.set_stop_color(&path, index, color, schema)
            }
            EditAction::EditText(path, slot, text) => self.edit_text(&path, slot, text, schema),
            EditAction::AddStop(path) => self.add_stop(&path, schema),
            EditAction::RemoveStop(path, i) => self.remove_stop(&path, i, schema),
            EditAction::Reset(path) => self.reset(&path, schema),
        }
    }

    /// Commit a fully-formed, valid value: update the model and recompute the
    /// dirty flag against the baseline.
    fn commit(&mut self, path: &str, value: Value) {
        let clean = self.baseline.get(path) == Some(&value);
        self.config.set(path.to_string(), value);
        if clean {
            self.dirty.remove(path);
        } else {
            self.dirty.insert(path.to_string());
        }
    }

    fn reset(&mut self, path: &str, schema: &Schema) {
        let Some(opt) = schema.option(path) else {
            return;
        };
        let default = opt.default.clone();
        self.config.set(path.to_string(), default.clone());
        // Rebaseline so the field reads as clean after a reset. `make_mut`
        // clones the shared baseline only if an undo snapshot still references it.
        Arc::make_mut(&mut self.baseline).insert(path.to_string(), default);
        self.dirty.remove(path);
        self.drafts.retain(|id, _| id.path != path);
        self.errors.retain(|id, _| id.path != path);
    }

    fn set_draft(&mut self, path: &str, slot: Slot, text: String) {
        self.errors.remove(&FieldId::new(path, slot.clone()));
        self.drafts.insert(FieldId::new(path, slot), text);
    }

    fn set_error(&mut self, path: &str, slot: Slot, message: impl Into<String>) {
        self.errors.insert(FieldId::new(path, slot), message.into());
    }

    fn clear_error(&mut self, path: &str, slot: Slot) {
        self.errors.remove(&FieldId::new(path, slot));
    }

    fn edit_text(&mut self, path: &str, slot: Slot, text: String, schema: &Schema) {
        self.drafts
            .insert(FieldId::new(path, slot.clone()), text.clone());
        let Some(opt) = schema.option(path) else {
            return;
        };

        match &opt.value_type {
            ValueType::Int => match parse_int(&text, opt.range.as_ref()) {
                Ok(i) => {
                    self.clear_error(path, Slot::Main);
                    self.commit(path, Value::Int(i));
                }
                Err(e) => self.set_error(path, Slot::Main, e),
            },
            ValueType::Float => match parse_float(&text, opt.range.as_ref()) {
                Ok(x) => {
                    self.clear_error(path, Slot::Main);
                    self.commit(path, Value::Float(x));
                }
                Err(e) => self.set_error(path, Slot::Main, e),
            },
            ValueType::String => {
                self.clear_error(path, Slot::Main);
                self.commit(path, Value::String(text));
            }
            ValueType::Color => match Color::from_hyprland_str(&text) {
                Ok(c) => {
                    self.clear_error(path, Slot::Hex);
                    self.commit(path, Value::Color(c));
                }
                Err(e) => self.set_error(path, Slot::Hex, e.to_string()),
            },
            ValueType::Vec2 => self.commit_vec2(path, schema),
            ValueType::Gradient => self.commit_gradient(path, schema),
            _ => {}
        }
    }

    fn commit_vec2(&mut self, path: &str, schema: &Schema) {
        let current = self.current_vec2(path, schema);
        let xs = self
            .draft(path, Slot::X)
            .map(str::to_string)
            .unwrap_or_else(|| fmt_num(current.x));
        let ys = self
            .draft(path, Slot::Y)
            .map(str::to_string)
            .unwrap_or_else(|| fmt_num(current.y));

        let x = xs.trim().parse::<f64>();
        let y = ys.trim().parse::<f64>();
        match (&x, &y) {
            (Ok(x), Ok(y)) => {
                self.clear_error(path, Slot::X);
                self.clear_error(path, Slot::Y);
                self.commit(path, Value::Vec2(Vec2::new(*x, *y)));
            }
            _ => {
                if x.is_err() {
                    self.set_error(path, Slot::X, "not a number");
                } else {
                    self.clear_error(path, Slot::X);
                }
                if y.is_err() {
                    self.set_error(path, Slot::Y, "not a number");
                } else {
                    self.clear_error(path, Slot::Y);
                }
            }
        }
    }

    fn commit_gradient(&mut self, path: &str, schema: &Schema) {
        let current = self.current_gradient(path, schema);
        let mut stops = Vec::with_capacity(current.stops.len());
        for (i, stop) in current.stops.iter().enumerate() {
            let text = self
                .draft(path, Slot::Stop(i))
                .map(str::to_string)
                .unwrap_or_else(|| stop.to_rgba_string());
            match Color::from_hyprland_str(&text) {
                Ok(c) => stops.push(c),
                Err(e) => {
                    self.set_error(path, Slot::Stop(i), e.to_string());
                    return;
                }
            }
        }

        let angle_text = self
            .draft(path, Slot::Angle)
            .map(str::to_string)
            .unwrap_or_else(|| current.angle_deg.map(fmt_num).unwrap_or_default());
        let angle = if angle_text.trim().is_empty() {
            None
        } else {
            match angle_text.trim().parse::<f64>() {
                Ok(a) => Some(a),
                Err(_) => {
                    self.set_error(path, Slot::Angle, "not a number");
                    return;
                }
            }
        };

        self.errors
            .retain(|id, _| id.path != path || !matches!(id.slot, Slot::Stop(_) | Slot::Angle));
        self.commit(
            path,
            Value::Gradient(Gradient {
                stops,
                angle_deg: angle,
            }),
        );
    }

    /// Set one gradient stop's color outright (from the visual picker), syncing
    /// that stop's hex draft.
    fn set_stop_color(&mut self, path: &str, index: usize, color: Color, schema: &Schema) {
        let mut gradient = self.current_gradient(path, schema);
        let Some(stop) = gradient.stops.get_mut(index) else {
            return;
        };
        *stop = color;
        self.clear_error(path, Slot::Stop(index));
        self.set_draft(path, Slot::Stop(index), color.to_rgba_string());
        self.commit(path, Value::Gradient(gradient));
    }

    fn add_stop(&mut self, path: &str, schema: &Schema) {
        let mut g = self.current_gradient(path, schema);
        g.stops.push(Color::rgba(0xff, 0xff, 0xff, 0xff));
        self.commit(path, Value::Gradient(g));
    }

    fn remove_stop(&mut self, path: &str, index: usize, schema: &Schema) {
        let mut g = self.current_gradient(path, schema);
        if g.stops.len() > 1 && index < g.stops.len() {
            g.stops.remove(index);
        }
        // Stop indices shift; drop stale stop drafts/errors so they re-derive.
        self.drafts
            .retain(|id, _| id.path != path || !matches!(id.slot, Slot::Stop(_)));
        self.errors
            .retain(|id, _| id.path != path || !matches!(id.slot, Slot::Stop(_)));
        self.commit(path, Value::Gradient(g));
    }

    fn current_vec2(&self, path: &str, schema: &Schema) -> Vec2 {
        match self.config.get(path) {
            Some(Value::Vec2(v)) => *v,
            _ => match schema.option(path).map(|o| &o.default) {
                Some(Value::Vec2(v)) => *v,
                _ => Vec2::new(0.0, 0.0),
            },
        }
    }

    fn current_gradient(&self, path: &str, schema: &Schema) -> Gradient {
        match self.config.get(path) {
            Some(Value::Gradient(g)) => g.clone(),
            _ => match schema.option(path).map(|o| &o.default) {
                Some(Value::Gradient(g)) => g.clone(),
                _ => Gradient::solid(Color::rgba(0xff, 0xff, 0xff, 0xff)),
            },
        }
    }
}

fn parse_int(text: &str, range: Option<&NumericRange>) -> Result<i64, String> {
    let value = text
        .trim()
        .parse::<i64>()
        .map_err(|_| "whole number expected".to_string())?;
    check_range(value as f64, range)?;
    Ok(value)
}

fn parse_float(text: &str, range: Option<&NumericRange>) -> Result<f64, String> {
    let value = text
        .trim()
        .parse::<f64>()
        .map_err(|_| "number expected".to_string())?;
    check_range(value, range)?;
    Ok(value)
}

fn check_range(value: f64, range: Option<&NumericRange>) -> Result<(), String> {
    if let Some(range) = range {
        if let Some(min) = range.min {
            if value < min {
                return Err(format!("must be ≥ {}", fmt_num(min)));
            }
        }
        if let Some(max) = range.max {
            if value > max {
                return Err(format!("must be ≤ {}", fmt_num(max)));
            }
        }
    }
    Ok(())
}

/// Format an `f64` without a trailing `.0` for whole numbers.
pub(crate) fn fmt_num(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Render a value for the pending-diff view (matches on-disk `conf` form).
fn value_text(value: &Value) -> String {
    hyprconf_core::conf::value_to_conf(value)
}

// ===========================================================================
// structured collections
// ===========================================================================

/// Direction for a reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Move the item one position earlier.
    Up,
    /// Move the item one position later.
    Down,
}

/// A bind flag (the `m`/`e`/`r`/`l`/`n`/`t`/`i` family).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindFlag {
    /// `l` — works on the lock screen.
    Locked,
    /// `r` — fires on release.
    Release,
    /// `e` — repeats.
    Repeat,
    /// `n` — non-consuming.
    NonConsuming,
    /// `m` — mouse bind.
    Mouse,
    /// `t` — transparent.
    Transparent,
    /// `i` — ignores mods.
    IgnoreMods,
}

/// The modifier keys offered as a multi-select.
pub const MODS: &[&str] = &["SUPER", "SHIFT", "CTRL", "ALT"];

/// Common dispatchers offered in the keybind dispatcher picker.
pub const DISPATCHERS: &[&str] = &[
    "exec",
    "killactive",
    "workspace",
    "movetoworkspace",
    "movetoworkspacesilent",
    "togglefloating",
    "fullscreen",
    "fakefullscreen",
    "movefocus",
    "movewindow",
    "resizeactive",
    "togglesplit",
    "pseudo",
    "pin",
    "togglespecialworkspace",
    "exit",
];

/// Field edits for a keybind row.
#[derive(Debug, Clone)]
pub enum KeybindEdit {
    /// Toggle a modifier on/off.
    ToggleMod(String, bool),
    /// Set the key/button.
    Key(String),
    /// Set the dispatcher.
    Dispatcher(String),
    /// Set the dispatcher arguments.
    Args(String),
    /// Set the submap (empty = global).
    Submap(String),
    /// Toggle a bind flag.
    Flag(BindFlag, bool),
}

/// Field edits for a window-rule row.
#[derive(Debug, Clone)]
pub enum WindowRuleEdit {
    /// Toggle `windowrulev2` vs legacy `windowrule`.
    V2(bool),
    /// Set the rule/effect.
    Rule(String),
    /// Set the raw matcher string.
    Matchers(String),
    /// Append an empty match criterion.
    AddMatch,
    /// Remove a match criterion.
    RemoveMatch(usize),
    /// Set a match criterion's key.
    MatchKey(usize, String),
    /// Set a match criterion's value.
    MatchValue(usize, String),
}

/// Field edits for a layer-rule row.
#[derive(Debug, Clone)]
pub enum LayerRuleEdit {
    /// Set the rule/effect.
    Rule(String),
    /// Set the target namespace.
    Namespace(String),
}

/// Field edits for a monitor row.
#[derive(Debug, Clone)]
pub enum MonitorEdit {
    /// Connector name/selector.
    Name(String),
    /// Resolution/mode.
    Mode(String),
    /// Position.
    Position(String),
    /// Scale.
    Scale(String),
    /// Transform (0-7).
    Transform(String),
    /// VRR (0-2).
    Vrr(String),
    /// Mirror target.
    Mirror(String),
}

/// Field edits for an env row.
#[derive(Debug, Clone)]
pub enum EnvEdit {
    /// Variable name.
    Name(String),
    /// Variable value.
    Value(String),
}

/// Field edits for an exec row.
#[derive(Debug, Clone)]
pub enum ExecEdit {
    /// Which exec flavour.
    Kind(ExecKind),
    /// The command.
    Command(String),
}

/// An edit to one of the structured collections.
#[derive(Debug, Clone)]
pub enum CollectionAction {
    /// Append a new default item.
    Add(CollectionId),
    /// Remove the item at an index.
    Remove(CollectionId, usize),
    /// Duplicate the item at an index.
    Duplicate(CollectionId, usize),
    /// Reorder the item at an index.
    Move(CollectionId, usize, Dir),
    /// Edit a keybind field.
    Keybind(usize, KeybindEdit),
    /// Edit a window-rule field.
    WindowRule(usize, WindowRuleEdit),
    /// Edit a layer-rule field.
    LayerRule(usize, LayerRuleEdit),
    /// Edit a monitor field.
    Monitor(usize, MonitorEdit),
    /// Set a submap name.
    Submap(usize, String),
    /// Edit an env field.
    Env(usize, EnvEdit),
    /// Edit an exec field.
    Exec(usize, ExecEdit),
}

enum StructOp {
    Remove(usize),
    Duplicate(usize),
    Move(usize, Dir),
}

impl Loaded {
    /// Total unsaved changes: edited scalar options plus touched collections.
    #[must_use]
    pub fn total_unsaved(&self) -> usize {
        self.dirty.len() + self.touched.len()
    }

    /// Collections that have been edited, for the changes view (stable order).
    #[must_use]
    pub fn touched_collections(&self) -> Vec<CollectionId> {
        let mut v: Vec<_> = self.touched.iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// Apply a [`CollectionAction`] to the model.
    pub fn apply_collection(&mut self, action: CollectionAction) {
        match action {
            CollectionAction::Add(id) => {
                self.touch(id);
                add_item(&mut self.config, id);
            }
            CollectionAction::Remove(id, i) => {
                self.touch(id);
                structural(&mut self.config, id, StructOp::Remove(i));
            }
            CollectionAction::Duplicate(id, i) => {
                self.touch(id);
                structural(&mut self.config, id, StructOp::Duplicate(i));
            }
            CollectionAction::Move(id, i, dir) => {
                self.touch(id);
                structural(&mut self.config, id, StructOp::Move(i, dir));
            }
            CollectionAction::Keybind(i, edit) => {
                self.touch(CollectionId::Keybinds);
                if let Some(t) = self.config.keybinds.get_mut(i) {
                    apply_keybind(&mut t.value, edit);
                }
            }
            CollectionAction::WindowRule(i, edit) => {
                self.touch(CollectionId::WindowRules);
                if let Some(t) = self.config.window_rules.get_mut(i) {
                    apply_window_rule(&mut t.value, edit);
                }
            }
            CollectionAction::LayerRule(i, edit) => {
                self.touch(CollectionId::LayerRules);
                if let Some(t) = self.config.layer_rules.get_mut(i) {
                    match edit {
                        LayerRuleEdit::Rule(s) => t.value.rule = s,
                        LayerRuleEdit::Namespace(s) => t.value.namespace = s,
                    }
                }
            }
            CollectionAction::Monitor(i, edit) => {
                self.touch(CollectionId::Monitors);
                if let Some(t) = self.config.monitors.get_mut(i) {
                    apply_monitor(&mut t.value, edit);
                }
            }
            CollectionAction::Submap(i, name) => {
                self.touch(CollectionId::Submaps);
                if let Some(t) = self.config.submaps.get_mut(i) {
                    t.value.name = name;
                }
            }
            CollectionAction::Env(i, edit) => {
                self.touch(CollectionId::Env);
                if let Some(t) = self.config.env.get_mut(i) {
                    match edit {
                        EnvEdit::Name(s) => t.value.name = s,
                        EnvEdit::Value(s) => t.value.value = s,
                    }
                }
            }
            CollectionAction::Exec(i, edit) => {
                self.touch(CollectionId::Execs);
                if let Some(t) = self.config.execs.get_mut(i) {
                    match edit {
                        ExecEdit::Kind(k) => t.value.kind = k,
                        ExecEdit::Command(s) => t.value.command = s,
                    }
                }
            }
        }
    }

    fn touch(&mut self, id: CollectionId) {
        self.touched.insert(id);
    }
}

fn add_item(c: &mut hyprconf_core::Config, id: CollectionId) {
    match id {
        CollectionId::Monitors => c.monitors.push(Tracked::new(MonitorRule {
            name: String::new(),
            mode: "preferred".into(),
            position: "auto".into(),
            scale: "1".into(),
            extra: Vec::new(),
        })),
        CollectionId::Workspaces => c.workspaces.push(Tracked::new(WorkspaceRule {
            selector: String::new(),
            rules: String::new(),
        })),
        CollectionId::WindowRules => c.window_rules.push(Tracked::new(WindowRule {
            v2: true,
            rule: "float".into(),
            matchers: String::new(),
        })),
        CollectionId::LayerRules => c.layer_rules.push(Tracked::new(LayerRule {
            rule: "blur".into(),
            namespace: String::new(),
        })),
        CollectionId::Keybinds => c.keybinds.push(Tracked::new(Keybind {
            flags: KeybindFlags::default(),
            mods: "SUPER".into(),
            key: String::new(),
            dispatcher: "killactive".into(),
            args: String::new(),
            submap: None,
        })),
        CollectionId::Submaps => c.submaps.push(Tracked::new(Submap {
            name: "submap".into(),
        })),
        CollectionId::Env => c.env.push(Tracked::new(EnvVar {
            name: String::new(),
            value: String::new(),
        })),
        CollectionId::Execs => c.execs.push(Tracked::new(Exec {
            kind: ExecKind::ExecOnce,
            command: String::new(),
        })),
        CollectionId::Variables => c.variables.push(Tracked::new(Variable {
            name: "var".into(),
            value: String::new(),
        })),
        CollectionId::Beziers => c.beziers.push(Tracked::new(Bezier {
            name: "curve".into(),
            p0: Vec2::new(0.05, 0.9),
            p1: Vec2::new(0.1, 1.0),
        })),
        CollectionId::Animations => c.animations.push(Tracked::new(Animation {
            name: "windows".into(),
            enabled: true,
            speed: 7.0,
            curve: "default".into(),
            style: None,
        })),
    }
}

fn structural(c: &mut hyprconf_core::Config, id: CollectionId, op: StructOp) {
    macro_rules! go {
        ($vec:expr) => {{
            match op {
                StructOp::Remove(i) => remove(&mut $vec, i),
                StructOp::Duplicate(i) => duplicate(&mut $vec, i),
                StructOp::Move(i, dir) => move_item(&mut $vec, i, dir),
            }
        }};
    }
    match id {
        CollectionId::Monitors => go!(c.monitors),
        CollectionId::Workspaces => go!(c.workspaces),
        CollectionId::WindowRules => go!(c.window_rules),
        CollectionId::LayerRules => go!(c.layer_rules),
        CollectionId::Keybinds => go!(c.keybinds),
        CollectionId::Submaps => go!(c.submaps),
        CollectionId::Env => go!(c.env),
        CollectionId::Execs => go!(c.execs),
        CollectionId::Variables => go!(c.variables),
        CollectionId::Beziers => go!(c.beziers),
        CollectionId::Animations => go!(c.animations),
    }
}

fn remove<T>(v: &mut Vec<Tracked<T>>, i: usize) {
    if i < v.len() {
        v.remove(i);
    }
}

fn duplicate<T: Clone>(v: &mut Vec<Tracked<T>>, i: usize) {
    if let Some(item) = v.get(i).cloned() {
        v.insert(i + 1, item);
    }
}

fn move_item<T>(v: &mut [Tracked<T>], i: usize, dir: Dir) {
    match dir {
        Dir::Up if i > 0 && i < v.len() => v.swap(i, i - 1),
        Dir::Down if i + 1 < v.len() => v.swap(i, i + 1),
        _ => {}
    }
}

fn apply_keybind(kb: &mut Keybind, edit: KeybindEdit) {
    match edit {
        KeybindEdit::ToggleMod(name, on) => kb.mods = toggle_mod(&kb.mods, &name, on),
        KeybindEdit::Key(s) => kb.key = s,
        KeybindEdit::Dispatcher(s) => kb.dispatcher = s,
        KeybindEdit::Args(s) => kb.args = s,
        KeybindEdit::Submap(s) => {
            kb.submap = if s.trim().is_empty() { None } else { Some(s) };
        }
        KeybindEdit::Flag(flag, on) => match flag {
            BindFlag::Locked => kb.flags.locked = on,
            BindFlag::Release => kb.flags.release = on,
            BindFlag::Repeat => kb.flags.repeat = on,
            BindFlag::NonConsuming => kb.flags.non_consuming = on,
            BindFlag::Mouse => kb.flags.mouse = on,
            BindFlag::Transparent => kb.flags.transparent = on,
            BindFlag::IgnoreMods => kb.flags.ignore_mods = on,
        },
    }
}

fn apply_window_rule(wr: &mut WindowRule, edit: WindowRuleEdit) {
    match edit {
        WindowRuleEdit::V2(b) => wr.v2 = b,
        WindowRuleEdit::Rule(s) => wr.rule = s,
        WindowRuleEdit::Matchers(s) => wr.matchers = s,
        WindowRuleEdit::AddMatch => {
            let mut m = parse_matchers(&wr.matchers);
            // Seed a key so the new (value-less) criterion survives the
            // round-trip through the matcher string.
            m.push(("class".into(), String::new()));
            wr.matchers = build_matchers(&m);
        }
        WindowRuleEdit::RemoveMatch(i) => {
            let mut m = parse_matchers(&wr.matchers);
            if i < m.len() {
                m.remove(i);
            }
            wr.matchers = build_matchers(&m);
        }
        WindowRuleEdit::MatchKey(i, k) => {
            let mut m = parse_matchers(&wr.matchers);
            if let Some(entry) = m.get_mut(i) {
                entry.0 = k;
            }
            wr.matchers = build_matchers(&m);
        }
        WindowRuleEdit::MatchValue(i, v) => {
            let mut m = parse_matchers(&wr.matchers);
            if let Some(entry) = m.get_mut(i) {
                entry.1 = v;
            }
            wr.matchers = build_matchers(&m);
        }
    }
}

fn apply_monitor(m: &mut MonitorRule, edit: MonitorEdit) {
    match edit {
        MonitorEdit::Name(s) => m.name = s,
        MonitorEdit::Mode(s) => m.mode = s,
        MonitorEdit::Position(s) => m.position = s,
        MonitorEdit::Scale(s) => m.scale = s,
        MonitorEdit::Transform(s) => {
            m.extra = build_extra(
                &s,
                &extra_field(&m.extra, "vrr"),
                &extra_field(&m.extra, "mirror"),
            );
        }
        MonitorEdit::Vrr(s) => {
            m.extra = build_extra(
                &extra_field(&m.extra, "transform"),
                &s,
                &extra_field(&m.extra, "mirror"),
            );
        }
        MonitorEdit::Mirror(s) => {
            m.extra = build_extra(
                &extra_field(&m.extra, "transform"),
                &extra_field(&m.extra, "vrr"),
                &s,
            );
        }
    }
}

// ---- mods / flags ----

/// Whether `mods` contains the given modifier (case-insensitive, CTRL≈CONTROL).
#[must_use]
pub fn has_mod(mods: &str, name: &str) -> bool {
    mods.split_whitespace().any(|t| is_mod(t, name))
}

fn is_mod(token: &str, name: &str) -> bool {
    token.eq_ignore_ascii_case(name) || (name == "CTRL" && token.eq_ignore_ascii_case("CONTROL"))
}

fn toggle_mod(mods: &str, name: &str, on: bool) -> String {
    let mut tokens: Vec<String> = mods
        .split_whitespace()
        .filter(|t| !is_mod(t, name))
        .map(String::from)
        .collect();
    if on {
        tokens.push(name.to_string());
    }
    tokens.join(" ")
}

// ---- window-rule matchers ----

/// Parse a `key:value, key:value` matcher string into pairs (best-effort:
/// splits on `,` then the first `:`).
#[must_use]
pub fn parse_matchers(matchers: &str) -> Vec<(String, String)> {
    if matchers.trim().is_empty() {
        return Vec::new();
    }
    matchers
        .split(',')
        .map(|part| match part.split_once(':') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => (part.trim().to_string(), String::new()),
        })
        .collect()
}

fn build_matchers(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                k.clone()
            } else {
                format!("{k}:{v}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- monitor extra ----

/// Extract a keyword's value from a monitor's trailing `extra` tokens.
#[must_use]
pub fn extra_field(extra: &[String], keyword: &str) -> String {
    extra
        .iter()
        .position(|t| t == keyword)
        .and_then(|i| extra.get(i + 1))
        .cloned()
        .unwrap_or_default()
}

fn build_extra(transform: &str, vrr: &str, mirror: &str) -> Vec<String> {
    let mut extra = Vec::new();
    for (keyword, value) in [("transform", transform), ("vrr", vrr), ("mirror", mirror)] {
        if !value.trim().is_empty() {
            extra.push(keyword.to_string());
            extra.push(value.trim().to_string());
        }
    }
    extra
}

// ---- validation ----

/// Returns a problem with a keybind, if any (empty key/dispatcher, or a
/// dispatcher that requires arguments but has none).
#[must_use]
pub fn keybind_issue(kb: &Keybind) -> Option<String> {
    if kb.key.trim().is_empty() {
        return Some("a key is required".into());
    }
    if kb.dispatcher.trim().is_empty() {
        return Some("a dispatcher is required".into());
    }
    if dispatcher_needs_args(&kb.dispatcher) && kb.args.trim().is_empty() {
        return Some(format!("`{}` needs arguments", kb.dispatcher));
    }
    None
}

fn dispatcher_needs_args(dispatcher: &str) -> bool {
    matches!(
        dispatcher,
        "exec"
            | "execr"
            | "workspace"
            | "movetoworkspace"
            | "movetoworkspacesilent"
            | "movefocus"
            | "movewindow"
            | "resizeactive"
            | "moveactive"
            | "focuswindow"
            | "swapwindow"
            | "togglespecialworkspace"
            | "layoutmsg"
            | "submap"
    )
}

/// Returns a problem with a window rule, if any.
#[must_use]
pub fn window_rule_issue(wr: &WindowRule) -> Option<String> {
    if wr.rule.trim().is_empty() {
        return Some("a rule is required".into());
    }
    if parse_matchers(&wr.matchers)
        .iter()
        .any(|(k, _)| k.trim().is_empty())
    {
        return Some("a match criterion has an empty key".into());
    }
    None
}

/// Returns a problem with a layer rule, if any.
#[must_use]
pub fn layer_rule_issue(rule: &LayerRule) -> Option<String> {
    if rule.rule.trim().is_empty() {
        return Some("a rule is required".into());
    }
    None
}

/// Returns a problem with a monitor, if any.
#[must_use]
pub fn monitor_issue(m: &MonitorRule) -> Option<String> {
    if m.name.trim().is_empty() {
        return Some("a connector/name is required".into());
    }
    None
}

/// Returns a problem with an env var, if any.
#[must_use]
pub fn env_issue(env: &EnvVar) -> Option<String> {
    if env.name.trim().is_empty() {
        return Some("a name is required".into());
    }
    None
}

/// Returns a problem with an exec entry, if any.
#[must_use]
pub fn exec_issue(exec: &Exec) -> Option<String> {
    if exec.command.trim().is_empty() {
        return Some("a command is required".into());
    }
    None
}

// ===========================================================================
// undo/redo snapshots + live-apply / coalescing metadata
// ===========================================================================

/// A snapshot of the editable state, for undo/redo.
#[derive(Debug, Clone)]
pub struct EditSnapshot {
    config: Config,
    baseline: Arc<HashMap<String, Value>>,
    dirty: HashSet<String>,
    touched: HashSet<CollectionId>,
    drafts: HashMap<FieldId, String>,
    errors: HashMap<FieldId, String>,
}

impl Loaded {
    /// Capture the current editable state.
    #[must_use]
    pub fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            config: self.config.clone(),
            baseline: self.baseline.clone(),
            dirty: self.dirty.clone(),
            touched: self.touched.clone(),
            drafts: self.drafts.clone(),
            errors: self.errors.clone(),
        }
    }

    /// Restore a previously-captured state.
    pub fn restore(&mut self, snapshot: EditSnapshot) {
        self.config = snapshot.config;
        self.baseline = snapshot.baseline;
        self.dirty = snapshot.dirty;
        self.touched = snapshot.touched;
        self.drafts = snapshot.drafts;
        self.errors = snapshot.errors;
    }
}

impl EditAction {
    /// A key that consecutive *continuous* edits (typing, dragging) share, so
    /// undo coalesces them into one step. Discrete edits return `None`.
    #[must_use]
    pub fn coalesce_key(&self) -> Option<String> {
        match self {
            EditAction::EditText(path, slot, _) => Some(format!("text:{path}:{slot:?}")),
            EditAction::SetIntSlider(path, _) | EditAction::SetFloatSlider(path, _) => {
                Some(format!("slider:{path}"))
            }
            EditAction::SetColor(path, _) => Some(format!("colorpick:{path}")),
            EditAction::SetStopColor(path, index, _) => Some(format!("colorpick:{path}:{index}")),
            _ => None,
        }
    }

    /// The scalar option path this edit affects (for live `hyprctl keyword`).
    #[must_use]
    pub fn option_path(&self) -> Option<&str> {
        match self {
            EditAction::SetBool(p, _)
            | EditAction::SetEnum(p, _)
            | EditAction::SetIntSlider(p, _)
            | EditAction::SetFloatSlider(p, _)
            | EditAction::SetColor(p, _)
            | EditAction::SetStopColor(p, _, _)
            | EditAction::EditText(p, _, _)
            | EditAction::Reset(p)
            | EditAction::AddStop(p)
            | EditAction::RemoveStop(p, _) => Some(p),
        }
    }
}

impl CollectionAction {
    /// See [`EditAction::coalesce_key`]; structural and toggle edits return `None`.
    #[must_use]
    pub fn coalesce_key(&self) -> Option<String> {
        let text = match self {
            CollectionAction::Keybind(i, KeybindEdit::Key(_)) => format!("kb:{i}:key"),
            CollectionAction::Keybind(i, KeybindEdit::Args(_)) => format!("kb:{i}:args"),
            CollectionAction::Keybind(i, KeybindEdit::Submap(_)) => format!("kb:{i}:submap"),
            CollectionAction::WindowRule(i, WindowRuleEdit::Rule(_)) => format!("wr:{i}:rule"),
            CollectionAction::WindowRule(i, WindowRuleEdit::Matchers(_)) => format!("wr:{i}:raw"),
            CollectionAction::WindowRule(i, WindowRuleEdit::MatchKey(j, _)) => {
                format!("wr:{i}:mk:{j}")
            }
            CollectionAction::WindowRule(i, WindowRuleEdit::MatchValue(j, _)) => {
                format!("wr:{i}:mv:{j}")
            }
            CollectionAction::LayerRule(i, LayerRuleEdit::Rule(_)) => format!("lr:{i}:rule"),
            CollectionAction::LayerRule(i, LayerRuleEdit::Namespace(_)) => format!("lr:{i}:ns"),
            CollectionAction::Monitor(i, edit) => format!("mon:{i}:{}", monitor_field_tag(edit)),
            CollectionAction::Submap(i, _) => format!("sm:{i}"),
            CollectionAction::Env(i, EnvEdit::Name(_)) => format!("env:{i}:name"),
            CollectionAction::Env(i, EnvEdit::Value(_)) => format!("env:{i}:value"),
            CollectionAction::Exec(i, ExecEdit::Command(_)) => format!("exec:{i}:cmd"),
            _ => return None,
        };
        Some(text)
    }
}

fn monitor_field_tag(edit: &MonitorEdit) -> &'static str {
    match edit {
        MonitorEdit::Name(_) => "name",
        MonitorEdit::Mode(_) => "mode",
        MonitorEdit::Position(_) => "position",
        MonitorEdit::Scale(_) => "scale",
        MonitorEdit::Transform(_) => "transform",
        MonitorEdit::Vrr(_) => "vrr",
        MonitorEdit::Mirror(_) => "mirror",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprconf_core::ConfigFormat;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use crate::load::Origin;
    use hyprconf_core::{ConfBundle, ConfParser, Config};

    fn synthetic_origin() -> Origin {
        Origin::Conf(ConfBundle {
            documents: vec![ConfParser::parse_str("", None)],
            root: 0,
        })
    }

    fn loaded() -> (Loaded, &'static Schema) {
        let schema = Schema::shared();
        let config = Config::default_from_schema(schema);
        let baseline: HashMap<String, Value> = schema
            .options()
            .map(|o| (o.path.clone(), config.get(&o.path).cloned().unwrap()))
            .collect();
        let baseline = Arc::new(baseline);
        let loaded = Loaded {
            format: ConfigFormat::Conf,
            source: PathBuf::from("test"),
            included_files: 0,
            warnings: 0,
            config,
            baseline,
            dirty: HashSet::new(),
            drafts: HashMap::new(),
            errors: HashMap::new(),
            touched: HashSet::new(),
            origin: synthetic_origin(),
            dynamic_regions: 0,
        };
        (loaded, schema)
    }

    #[test]
    fn collection_add_remove_duplicate_reorder() {
        let (mut l, _schema) = loaded();
        let id = CollectionId::Keybinds;

        l.apply_collection(CollectionAction::Add(id));
        l.apply_collection(CollectionAction::Add(id));
        assert_eq!(l.config.keybinds.len(), 2);

        l.apply_collection(CollectionAction::Keybind(0, KeybindEdit::Key("Q".into())));
        l.apply_collection(CollectionAction::Keybind(1, KeybindEdit::Key("W".into())));
        assert_eq!(l.config.keybinds[0].value.key, "Q");

        l.apply_collection(CollectionAction::Move(id, 0, Dir::Down));
        assert_eq!(l.config.keybinds[0].value.key, "W");
        assert_eq!(l.config.keybinds[1].value.key, "Q");

        l.apply_collection(CollectionAction::Duplicate(id, 0));
        assert_eq!(l.config.keybinds.len(), 3);
        assert_eq!(l.config.keybinds[1].value.key, "W");

        l.apply_collection(CollectionAction::Remove(id, 0));
        assert_eq!(l.config.keybinds.len(), 2);
        assert!(l.touched.contains(&id));
        assert!(l.total_unsaved() >= 1);
    }

    #[test]
    fn keybind_mods_and_flags_edit() {
        let (mut l, _schema) = loaded();
        l.apply_collection(CollectionAction::Add(CollectionId::Keybinds));
        // default mods is "SUPER"
        l.apply_collection(CollectionAction::Keybind(
            0,
            KeybindEdit::ToggleMod("SHIFT".into(), true),
        ));
        assert!(has_mod(&l.config.keybinds[0].value.mods, "SUPER"));
        assert!(has_mod(&l.config.keybinds[0].value.mods, "SHIFT"));
        l.apply_collection(CollectionAction::Keybind(
            0,
            KeybindEdit::ToggleMod("SUPER".into(), false),
        ));
        assert!(!has_mod(&l.config.keybinds[0].value.mods, "SUPER"));

        l.apply_collection(CollectionAction::Keybind(
            0,
            KeybindEdit::Flag(BindFlag::Repeat, true),
        ));
        assert!(l.config.keybinds[0].value.flags.repeat);
        assert_eq!(l.config.keybinds[0].value.flags.keyword(), "binde");
    }

    #[test]
    fn window_rule_match_builder() {
        let (mut l, _schema) = loaded();
        l.apply_collection(CollectionAction::Add(CollectionId::WindowRules));
        l.apply_collection(CollectionAction::WindowRule(0, WindowRuleEdit::AddMatch));
        l.apply_collection(CollectionAction::WindowRule(
            0,
            WindowRuleEdit::MatchKey(0, "class".into()),
        ));
        l.apply_collection(CollectionAction::WindowRule(
            0,
            WindowRuleEdit::MatchValue(0, "^(kitty)$".into()),
        ));
        assert_eq!(l.config.window_rules[0].value.matchers, "class:^(kitty)$");
    }

    #[test]
    fn monitor_transform_field_round_trips_through_extra() {
        let (mut l, _schema) = loaded();
        l.apply_collection(CollectionAction::Add(CollectionId::Monitors));
        l.apply_collection(CollectionAction::Monitor(
            0,
            MonitorEdit::Name("DP-1".into()),
        ));
        l.apply_collection(CollectionAction::Monitor(
            0,
            MonitorEdit::Transform("1".into()),
        ));
        l.apply_collection(CollectionAction::Monitor(0, MonitorEdit::Vrr("2".into())));
        let m = &l.config.monitors[0].value;
        assert_eq!(extra_field(&m.extra, "transform"), "1");
        assert_eq!(extra_field(&m.extra, "vrr"), "2");
    }

    fn empty_loaded() -> Loaded {
        Loaded {
            format: ConfigFormat::Conf,
            source: PathBuf::from("test"),
            included_files: 0,
            warnings: 0,
            config: Config::empty(),
            baseline: Arc::new(HashMap::new()),
            dirty: HashSet::new(),
            drafts: HashMap::new(),
            errors: HashMap::new(),
            touched: HashSet::new(),
            origin: synthetic_origin(),
            dynamic_regions: 0,
        }
    }

    fn values<T: Clone>(items: &[Tracked<T>]) -> Vec<T> {
        items.iter().map(|t| t.value.clone()).collect()
    }

    #[test]
    fn gui_built_collections_round_trip_through_both_formats() {
        use hyprconf_core::conf::{
            config_to_conf, document_to_config as conf_to_config, ConfParser,
        };
        use hyprconf_core::lua::{document_to_config as lua_to_config, LuaParser};
        use hyprconf_core::LuaSerializer;

        let schema = Schema::shared();
        let mut l = empty_loaded();

        // A keybind: SUPER, Q, killactive (defaults supply SUPER + killactive).
        l.apply_collection(CollectionAction::Add(CollectionId::Keybinds));
        l.apply_collection(CollectionAction::Keybind(0, KeybindEdit::Key("Q".into())));

        // A window rule: windowrulev2 float, class:^(kitty)$
        l.apply_collection(CollectionAction::Add(CollectionId::WindowRules));
        l.apply_collection(CollectionAction::WindowRule(0, WindowRuleEdit::AddMatch));
        l.apply_collection(CollectionAction::WindowRule(
            0,
            WindowRuleEdit::MatchValue(0, "^(kitty)$".into()),
        ));

        // A monitor: DP-1, 1920x1080@144, 0x0, 1
        l.apply_collection(CollectionAction::Add(CollectionId::Monitors));
        l.apply_collection(CollectionAction::Monitor(
            0,
            MonitorEdit::Name("DP-1".into()),
        ));
        l.apply_collection(CollectionAction::Monitor(
            0,
            MonitorEdit::Mode("1920x1080@144".into()),
        ));
        l.apply_collection(CollectionAction::Monitor(
            0,
            MonitorEdit::Position("0x0".into()),
        ));
        l.apply_collection(CollectionAction::Monitor(0, MonitorEdit::Scale("1".into())));

        let config = &l.config;

        // --- Lua: serialize -> parse -> equal ---
        let lua = LuaSerializer::serialize(config);
        let lua_doc = LuaParser::parse_str(&lua, None).expect("emitted lua parses");
        let (lua_cfg, _) = lua_to_config(&lua_doc, schema);
        assert_eq!(
            values(&lua_cfg.keybinds),
            values(&config.keybinds),
            "lua keybind\n{lua}"
        );
        assert_eq!(
            values(&lua_cfg.window_rules),
            values(&config.window_rules),
            "lua rule\n{lua}"
        );
        assert_eq!(
            values(&lua_cfg.monitors),
            values(&config.monitors),
            "lua monitor\n{lua}"
        );

        // --- conf: serialize -> parse -> equal ---
        let conf = config_to_conf(config);
        let conf_doc = ConfParser::parse_str(&conf, None);
        let (conf_cfg, _) = conf_to_config(&conf_doc, schema);
        assert_eq!(
            values(&conf_cfg.keybinds),
            values(&config.keybinds),
            "conf keybind\n{conf}"
        );
        assert_eq!(
            values(&conf_cfg.window_rules),
            values(&config.window_rules),
            "conf rule\n{conf}"
        );
        assert_eq!(
            values(&conf_cfg.monitors),
            values(&config.monitors),
            "conf monitor\n{conf}"
        );
    }

    #[test]
    fn snapshot_restore_reverts_scalar_and_collection_edits() {
        let (mut l, schema) = loaded();
        let before = l.snapshot();

        l.apply(
            EditAction::SetIntSlider("decoration:rounding".into(), 15),
            schema,
        );
        l.apply_collection(CollectionAction::Add(CollectionId::Keybinds));
        l.apply_collection(CollectionAction::Keybind(0, KeybindEdit::Key("Q".into())));
        let after = l.snapshot();
        assert_eq!(l.config.get("decoration:rounding"), Some(&Value::Int(15)));
        assert_eq!(l.config.keybinds.len(), 1);

        // undo
        l.restore(before);
        assert_eq!(l.config.get("decoration:rounding"), Some(&Value::Int(0)));
        assert_eq!(l.config.keybinds.len(), 0);

        // redo
        l.restore(after);
        assert_eq!(l.config.get("decoration:rounding"), Some(&Value::Int(15)));
        assert_eq!(l.config.keybinds[0].value.key, "Q");
    }

    #[test]
    fn coalesce_keys_group_typing_but_not_discrete() {
        assert_eq!(
            EditAction::EditText("a".into(), Slot::Main, "x".into()).coalesce_key(),
            EditAction::EditText("a".into(), Slot::Main, "xy".into()).coalesce_key()
        );
        assert!(EditAction::SetBool("a".into(), true)
            .coalesce_key()
            .is_none());
        assert!(CollectionAction::Add(CollectionId::Keybinds)
            .coalesce_key()
            .is_none());
        assert!(CollectionAction::Keybind(0, KeybindEdit::Args("x".into()))
            .coalesce_key()
            .is_some());
    }

    #[test]
    fn validation_flags_obviously_invalid_entries() {
        let (mut l, _schema) = loaded();
        l.apply_collection(CollectionAction::Add(CollectionId::Keybinds)); // key empty
        assert!(keybind_issue(&l.config.keybinds[0].value).is_some());
        l.apply_collection(CollectionAction::Keybind(0, KeybindEdit::Key("Q".into())));
        assert!(keybind_issue(&l.config.keybinds[0].value).is_none());
        // exec needs args
        l.apply_collection(CollectionAction::Keybind(
            0,
            KeybindEdit::Dispatcher("exec".into()),
        ));
        assert!(keybind_issue(&l.config.keybinds[0].value).is_some());

        l.apply_collection(CollectionAction::Add(CollectionId::Monitors));
        assert!(monitor_issue(&l.config.monitors[0].value).is_some()); // empty name
    }

    #[test]
    fn bool_edit_round_trips_and_tracks_dirty() {
        let (mut l, schema) = loaded();
        let path = "decoration:blur:enabled"; // default true
        assert!(!l.is_dirty(path));
        l.apply(EditAction::SetBool(path.into(), false), schema);
        assert_eq!(l.config.get(path), Some(&Value::Bool(false)));
        assert!(l.is_dirty(path));
        assert_eq!(l.total_unsaved(), 1);

        // editing back to the baseline clears dirty
        l.apply(EditAction::SetBool(path.into(), true), schema);
        assert!(!l.is_dirty(path));
        assert_eq!(l.total_unsaved(), 0);
    }

    #[test]
    fn int_text_edit_validates_range_without_corrupting_model() {
        let (mut l, schema) = loaded();
        let path = "decoration:rounding"; // Int, min 0

        l.apply(
            EditAction::EditText(path.into(), Slot::Main, "12".into()),
            schema,
        );
        assert_eq!(l.config.get(path), Some(&Value::Int(12)));
        assert!(l.field_error(path, Slot::Main).is_none());

        // out of range: error shown, model keeps the last valid value
        l.apply(
            EditAction::EditText(path.into(), Slot::Main, "-5".into()),
            schema,
        );
        assert!(l.field_error(path, Slot::Main).is_some());
        assert_eq!(
            l.config.get(path),
            Some(&Value::Int(12)),
            "model not corrupted"
        );

        // garbage: same — error, model unchanged
        l.apply(
            EditAction::EditText(path.into(), Slot::Main, "abc".into()),
            schema,
        );
        assert!(l.field_error(path, Slot::Main).is_some());
        assert_eq!(l.config.get(path), Some(&Value::Int(12)));
    }

    #[test]
    fn enum_edit_round_trips() {
        let (mut l, schema) = loaded();
        let path = "general:layout";
        l.apply(EditAction::SetEnum(path.into(), "master".into()), schema);
        assert_eq!(l.config.get(path), Some(&Value::Enum("master".into())));
        assert!(l.is_dirty(path));
    }

    #[test]
    fn float_slider_round_trips() {
        let (mut l, schema) = loaded();
        let path = "decoration:active_opacity";
        l.apply(EditAction::SetFloatSlider(path.into(), 0.5), schema);
        assert_eq!(l.config.get(path), Some(&Value::Float(0.5)));
        assert_eq!(l.draft(path, Slot::Main), Some("0.5"));
    }

    #[test]
    fn invalid_hex_edit_keeps_color_unchanged() {
        let (mut l, schema) = loaded();
        let path = "decoration:shadow:color";
        // Establish a known color, the way the visual picker does.
        let picked = Color::rgba(0x10, 0x20, 0x30, 0xff);
        l.apply(EditAction::SetColor(path.into(), picked), schema);
        // hex field synced
        assert!(l.draft(path, Slot::Hex).is_some());

        // invalid hex => error, model unchanged
        let before = l.config.get(path).cloned();
        l.apply(
            EditAction::EditText(path.into(), Slot::Hex, "nope".into()),
            schema,
        );
        assert!(l.field_error(path, Slot::Hex).is_some());
        assert_eq!(l.config.get(path).cloned(), before);
    }

    #[test]
    fn set_color_commits_value_and_syncs_hex_draft() {
        let (mut l, schema) = loaded();
        let path = "decoration:shadow:color";
        let picked = Color::rgba(0x33, 0x66, 0x99, 0xcc);
        l.apply(EditAction::SetColor(path.into(), picked), schema);
        assert_eq!(l.config.get(path), Some(&Value::Color(picked)));
        // the hex field reflects the picked color so text editors stay in sync
        assert_eq!(l.draft(path, Slot::Hex), Some("rgba(336699cc)"));
        assert_eq!(
            EditAction::SetColor(path.into(), picked).option_path(),
            Some(path)
        );
    }

    #[test]
    fn set_stop_color_updates_gradient_stop_and_syncs_draft() {
        let (mut l, schema) = loaded();
        let path = "general:col.active_border";
        let picked = Color::rgba(0x11, 0x22, 0x33, 0xff);
        l.apply(EditAction::SetStopColor(path.into(), 0, picked), schema);
        match l.config.get(path) {
            Some(Value::Gradient(g)) => assert_eq!(g.stops[0], picked),
            other => panic!("expected gradient, got {other:?}"),
        }
        assert_eq!(l.draft(path, Slot::Stop(0)), Some("rgba(112233ff)"));
    }

    #[test]
    fn vec2_edit_requires_both_components() {
        let (mut l, schema) = loaded();
        let path = "decoration:shadow:offset";
        l.apply(
            EditAction::EditText(path.into(), Slot::X, "3".into()),
            schema,
        );
        l.apply(
            EditAction::EditText(path.into(), Slot::Y, "4".into()),
            schema,
        );
        assert_eq!(l.config.get(path), Some(&Value::Vec2(Vec2::new(3.0, 4.0))));

        // breaking x flags x, keeps last good model
        l.apply(
            EditAction::EditText(path.into(), Slot::X, "x".into()),
            schema,
        );
        assert!(l.field_error(path, Slot::X).is_some());
        assert_eq!(l.config.get(path), Some(&Value::Vec2(Vec2::new(3.0, 4.0))));
    }

    #[test]
    fn gradient_stops_and_angle() {
        let (mut l, schema) = loaded();
        let path = "general:col.active_border"; // gradient default

        l.apply(EditAction::AddStop(path.into()), schema);
        let stops_after_add = match l.config.get(path) {
            Some(Value::Gradient(g)) => g.stops.len(),
            _ => 0,
        };
        assert!(stops_after_add >= 2);

        l.apply(
            EditAction::EditText(path.into(), Slot::Stop(0), "rgba(11223344)".into()),
            schema,
        );
        match l.config.get(path) {
            Some(Value::Gradient(g)) => assert_eq!(g.stops[0], Color::rgba(0x11, 0x22, 0x33, 0x44)),
            other => panic!("expected gradient, got {other:?}"),
        }

        l.apply(
            EditAction::EditText(path.into(), Slot::Angle, "90".into()),
            schema,
        );
        match l.config.get(path) {
            Some(Value::Gradient(g)) => assert_eq!(g.angle_deg, Some(90.0)),
            _ => panic!("expected gradient"),
        }
    }

    #[test]
    fn reset_restores_default_and_clears_dirty() {
        let (mut l, schema) = loaded();
        let path = "decoration:rounding";
        let default = schema.option(path).unwrap().default.clone();

        l.apply(EditAction::SetIntSlider(path.into(), 20), schema);
        assert!(l.is_dirty(path));
        assert!(l.draft(path, Slot::Main).is_some());

        l.apply(EditAction::Reset(path.into()), schema);
        assert_eq!(l.config.get(path), Some(&default));
        assert!(!l.is_dirty(path), "reset clears the dirty flag");
        assert!(
            l.draft(path, Slot::Main).is_none(),
            "reset clears the draft"
        );
        assert!(l.field_error(path, Slot::Main).is_none());
    }

    #[test]
    fn pending_diff_lists_changes() {
        let (mut l, schema) = loaded();
        l.apply(
            EditAction::SetIntSlider("decoration:rounding".into(), 9),
            schema,
        );
        l.apply(
            EditAction::SetBool("decoration:blur:enabled".into(), false),
            schema,
        );
        let diff = l.pending_diff();
        assert_eq!(diff.len(), 2);
        // sorted by path: blur:enabled before rounding
        assert_eq!(diff[0].0, "decoration:blur:enabled");
        assert_eq!(diff[0].2, "false");
    }
}
