// SPDX-License-Identifier: MIT OR Apache-2.0
//! The editing engine: applies user edits to the in-memory [`Config`], tracks
//! per-field drafts, validation errors and dirty state, and supports
//! reset-to-default. This is deliberately UI-free so it can be unit-tested.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hyprconf_core::schema::{CollectionId, NumericRange, OptionSpec, Schema, ValueType};
use hyprconf_core::structured::{
    Animation, Bezier, Device, EnvVar, Exec, ExecKind, Gesture, Keybind, KeybindFlags, LayerRule,
    MonitorRule, Permission, Plugin, Submap, Variable, WindowRule, WorkspaceRule, GESTURE_ACTIONS,
    GESTURE_DIRECTIONS, PERMISSION_MODES, PERMISSION_TYPES,
};
use hyprconf_core::value::{Color, CssGap, Gradient, Vec2};
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
    /// One side of a gap, in CSS order (0 top, 1 right, 2 bottom, 3 left).
    Side(usize),
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
    /// Set a whole per-side gap value (see [`Loaded::set_gap`]).
    SetGap(String, CssGap),
    /// Undo this session's change to an option: back to the value it had when
    /// the file was loaded (unlike [`EditAction::Reset`], which goes to
    /// Hyprland's default).
    Revert(String),
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
            EditAction::SetGap(path, gap) => self.set_gap(&path, gap),
            EditAction::Revert(path) => {
                if let Some(base) = self.baseline.get(&path).cloned() {
                    self.drafts.retain(|id, _| id.path != path);
                    self.errors.retain(|id, _| id.path != path);
                    self.commit(&path, base);
                }
            }
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

    /// Reset to the schema default — as an ordinary edit.
    ///
    /// This used to re-baseline the option as "clean", which meant resetting a
    /// value the *file* sets (say `rounding = 10`) was never written back by a
    /// preserve-mode save: the reset silently evaporated. Now it is dirty
    /// exactly when it differs from what was loaded, like any other edit.
    fn reset(&mut self, path: &str, schema: &Schema) {
        let Some(opt) = schema.option(path) else {
            return;
        };
        self.drafts.retain(|id, _| id.path != path);
        self.errors.retain(|id, _| id.path != path);
        self.commit(path, opt.default.clone());
    }

    /// Whether an option's current value is its schema default.
    #[must_use]
    pub fn is_default(&self, opt: &OptionSpec) -> bool {
        self.config.get(&opt.path).is_none_or(|v| *v == opt.default)
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
            ValueType::CssGap => self.commit_gap_text(path, slot, &text, opt),
            _ => {}
        }
    }

    /// A typed gap field: the single "all sides" field (`Slot::Main`, which
    /// also accepts CSS shorthand like `5 10`) or one side (`Slot::Side`).
    fn commit_gap_text(&mut self, path: &str, slot: Slot, text: &str, opt: &OptionSpec) {
        let current = self.current_gap(path, opt);
        let parsed = match slot {
            Slot::Side(i) => text
                .trim()
                .parse::<i64>()
                .map(|px| current.with_side(i, px))
                .map_err(|_| "whole number expected".to_string()),
            _ => CssGap::from_hyprland_str(text).map_err(|e| e.to_string()),
        };
        let checked = parsed.and_then(|gap| {
            gap.sides()
                .iter()
                .try_for_each(|&px| check_range(px as f64, opt.range.as_ref()))
                .map(|()| gap)
        });
        match checked {
            Ok(gap) => {
                self.clear_error(path, slot);
                self.commit(path, Value::CssGap(gap));
            }
            Err(e) => self.set_error(path, slot, e),
        }
    }

    /// Set a whole gap at once (the "all sides" slider, or linking the sides),
    /// discarding any per-side drafts so the fields re-derive from the model.
    pub fn set_gap(&mut self, path: &str, gap: CssGap) {
        self.drafts.retain(|id, _| id.path != path);
        self.errors.retain(|id, _| id.path != path);
        self.commit(path, Value::CssGap(gap));
    }

    /// The current gap (set value, else default, else zero).
    #[must_use]
    pub fn current_gap(&self, path: &str, opt: &OptionSpec) -> CssGap {
        match self.config.get(path).unwrap_or(&opt.default) {
            Value::CssGap(g) => *g,
            Value::Int(i) => CssGap::uniform(*i),
            _ => CssGap::uniform(0),
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
    /// `o` — long press.
    LongPress,
    /// `c` — click.
    Click,
    /// `g` — drag.
    Drag,
    /// `p` — bypass shortcut inhibitors.
    DontInhibit,
}

impl BindFlag {
    /// Every flag with a short label and an explanation, in display order.
    pub const ALL: [(BindFlag, &'static str, &'static str); 11] = [
        (
            BindFlag::Repeat,
            "repeat",
            "Repeat while the key is held (e)",
        ),
        (
            BindFlag::Release,
            "on release",
            "Fire when the key is released (r)",
        ),
        (
            BindFlag::LongPress,
            "long press",
            "Fire on a long press (o)",
        ),
        (
            BindFlag::Click,
            "click",
            "Fire on a click without movement (c)",
        ),
        (BindFlag::Drag, "drag", "Fire on a drag (g)"),
        (
            BindFlag::Locked,
            "when locked",
            "Also works on the lock screen (l)",
        ),
        (
            BindFlag::Mouse,
            "mouse",
            "Mouse bind: key is mouse:272 / mouse:273 (m)",
        ),
        (
            BindFlag::NonConsuming,
            "pass through",
            "The key also reaches the focused app (n)",
        ),
        (
            BindFlag::Transparent,
            "transparent",
            "Doesn't block other binds on the same key (t)",
        ),
        (
            BindFlag::IgnoreMods,
            "ignore mods",
            "Fires regardless of held modifiers (i)",
        ),
        (
            BindFlag::DontInhibit,
            "bypass inhibit",
            "Works even when an app inhibits shortcuts (p)",
        ),
    ];

    /// Whether this flag is set on `flags`.
    #[must_use]
    pub fn is_set(self, flags: &KeybindFlags) -> bool {
        match self {
            BindFlag::Locked => flags.locked,
            BindFlag::Release => flags.release,
            BindFlag::Repeat => flags.repeat,
            BindFlag::NonConsuming => flags.non_consuming,
            BindFlag::Mouse => flags.mouse,
            BindFlag::Transparent => flags.transparent,
            BindFlag::IgnoreMods => flags.ignore_mods,
            BindFlag::LongPress => flags.long_press,
            BindFlag::Click => flags.click,
            BindFlag::Drag => flags.drag,
            BindFlag::DontInhibit => flags.dont_inhibit,
        }
    }
}

/// The modifier keys offered as a multi-select.
pub const MODS: &[&str] = &["SUPER", "SHIFT", "CTRL", "ALT"];

/// Every dispatcher offered in the keybind editor, with what it does and the
/// arguments it takes (`[…]` = optional). These are exactly the dispatchers
/// `hyprconf_core::lua::dispatch` maps onto native `hl.dsp.*` calls, so any
/// bind built from this list also converts cleanly to Lua.
pub const DISPATCHERS: &[(&str, &str)] = &[
    ("exec", "Run a command · args: the command"),
    (
        "execr",
        "Run a command without window rules · args: the command",
    ),
    ("killactive", "Close the active window"),
    ("forcekillactive", "Kill the active window's process"),
    (
        "closewindow",
        "Close a window · args: window (class:…, title:…)",
    ),
    ("killwindow", "Kill a window's process · args: window"),
    (
        "signalwindow",
        "Send a signal to a window · args: window, signal",
    ),
    (
        "workspace",
        "Switch workspace · args: 1, +1, e+1, name:web, previous…",
    ),
    (
        "movetoworkspace",
        "Move the window to a workspace and follow · args: workspace",
    ),
    (
        "movetoworkspacesilent",
        "Move the window to a workspace, stay here · args: workspace",
    ),
    ("togglefloating", "Toggle floating for the active window"),
    ("setfloating", "Make the active window float"),
    ("settiled", "Make the active window tiled"),
    (
        "fullscreen",
        "Toggle fullscreen · args: [0 fullscreen | 1 maximize]",
    ),
    (
        "fullscreenstate",
        "Set the fullscreen state · args: internal client (-1…3)",
    ),
    (
        "dpms",
        "Turn displays on or off · args: on | off | toggle [monitor]",
    ),
    ("forceidle", "Act as if the user went idle · args: seconds"),
    ("pin", "Pin a floating window to every workspace"),
    ("movefocus", "Move focus · args: l | r | u | d"),
    (
        "movewindow",
        "Move the window · args: l | r | u | d, or mon:NAME",
    ),
    (
        "resizewindow",
        "Mouse binds (bindm): resize the window by dragging",
    ),
    (
        "swapwindow",
        "Swap the window with a neighbour · args: l | r | u | d",
    ),
    ("centerwindow", "Center the floating window"),
    (
        "resizeactive",
        "Resize the active window · args: x y (e.g. 20 0, -20 0)",
    ),
    ("moveactive", "Move the floating window · args: x y"),
    (
        "resizewindowpixel",
        "Resize a given window · args: x y, window",
    ),
    ("movewindowpixel", "Move a given window · args: x y, window"),
    (
        "cyclenext",
        "Focus the next window · args: [prev] [tiled | floating]",
    ),
    ("tagwindow", "Add/remove a window tag · args: [+-]tag"),
    (
        "focuswindow",
        "Focus a window · args: class:…, title:… or address:…",
    ),
    (
        "focusmonitor",
        "Focus a monitor · args: name, l/r/u/d or +1/-1",
    ),
    (
        "movecursortocorner",
        "Move the cursor to a window corner · args: 0–3",
    ),
    ("movecursor", "Move the cursor · args: x y"),
    ("renameworkspace", "Rename a workspace · args: id new-name"),
    ("exit", "Exit Hyprland"),
    (
        "forcerendererreload",
        "Reload the renderer (shaders, resources)",
    ),
    (
        "movecurrentworkspacetomonitor",
        "Move this workspace to a monitor · args: monitor",
    ),
    (
        "moveworkspacetomonitor",
        "Move a workspace to a monitor · args: workspace monitor",
    ),
    (
        "swapactiveworkspaces",
        "Swap two monitors' workspaces · args: monitor monitor",
    ),
    ("bringactivetotop", "Raise the floating window above others"),
    ("alterzorder", "Change stacking order · args: top | bottom"),
    (
        "togglespecialworkspace",
        "Show/hide a special workspace · args: [name]",
    ),
    (
        "focusurgentorlast",
        "Focus the urgent window, else the previous one",
    ),
    (
        "focuscurrentorlast",
        "Jump between the current and previous window",
    ),
    ("togglegroup", "Group the window (tabs) or ungroup it"),
    (
        "changegroupactive",
        "Switch tab in a group · args: f | b | index",
    ),
    (
        "lockgroups",
        "Lock every group · args: lock | unlock | toggle",
    ),
    (
        "lockactivegroup",
        "Lock the active group · args: lock | unlock | toggle",
    ),
    (
        "movegroupwindow",
        "Move the tab within its group · args: f | b",
    ),
    (
        "denywindowfromgroup",
        "Keep the window out of groups · args: on | off | toggle",
    ),
    (
        "layoutmsg",
        "Send the layout a command · args: e.g. togglesplit, swapwithmaster",
    ),
    ("togglesplit", "Dwindle: flip the split direction"),
    ("swapsplit", "Dwindle: swap the two halves of the split"),
    (
        "preselect",
        "Dwindle: choose where the next window opens · args: l | r | u | d",
    ),
    ("pseudo", "Toggle pseudo-tiling for the window"),
    ("pass", "Pass the key to a window · args: window"),
    (
        "sendshortcut",
        "Send a key combo to a window · args: mods, key[, window]",
    ),
    (
        "global",
        "Trigger an app's global shortcut · args: app:name",
    ),
    ("submap", "Enter a key mode (submap) · args: name | reset"),
    ("event", "Emit a custom socket2 event · args: data"),
    (
        "setprop",
        "Set a window property · args: window property value",
    ),
    ("toggleswallow", "Toggle window swallowing"),
];

/// The help line for a dispatcher, if it is a known one.
#[must_use]
pub fn dispatcher_help(dispatcher: &str) -> Option<&'static str> {
    DISPATCHERS
        .iter()
        .find(|(name, _)| *name == dispatcher)
        .map(|(_, help)| *help)
}

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
    /// Set the human description (empty = none).
    Description(String),
}

/// Field edits for a workspace-rule row.
#[derive(Debug, Clone)]
pub enum WorkspaceEdit {
    /// The workspace selector (`1`, `name:web`, `special:magic`, `r[1-5]`…).
    Selector(String),
    /// The raw `key:value, …` rule list.
    Rules(String),
    /// Append a rule.
    AddRule,
    /// Remove the rule at an index.
    RemoveRule(usize),
    /// Set a rule's key.
    RuleKey(usize, String),
    /// Set a rule's value.
    RuleValue(usize, String),
}

/// Field edits for a variable row.
#[derive(Debug, Clone)]
pub enum VariableEdit {
    /// The name (without `$`).
    Name(String),
    /// The replacement text.
    Value(String),
}

/// Field edits for a bezier-curve row.
#[derive(Debug, Clone)]
pub enum BezierEdit {
    /// The curve name.
    Name(String),
    /// One control coordinate: 0 = x0, 1 = y0, 2 = x1, 3 = y1.
    Coord(usize, f64),
}

/// Field edits for an animation row.
#[derive(Debug, Clone)]
pub enum AnimationEdit {
    /// The animation target (`windows`, `workspaces`, …).
    Name(String),
    /// On/off.
    Enabled(bool),
    /// Speed in deciseconds.
    Speed(f64),
    /// The curve name.
    Curve(String),
    /// The style (empty = none).
    Style(String),
}

/// Field edits for a gesture row.
#[derive(Debug, Clone)]
pub enum GestureEdit {
    /// Finger count.
    Fingers(u32),
    /// Direction.
    Direction(String),
    /// Toggle a modifier.
    ToggleMod(String, bool),
    /// Action.
    Action(String),
    /// Action arguments.
    Args(String),
    /// Bypass shortcut inhibitors.
    Bypass(bool),
}

/// Field edits for a device row.
#[derive(Debug, Clone)]
pub enum DeviceEdit {
    /// The device name.
    Name(String),
    /// Append an option.
    AddOption,
    /// Remove the option at an index.
    RemoveOption(usize),
    /// Set an option's key.
    OptionKey(usize, String),
    /// Set an option's value.
    OptionValue(usize, String),
}

/// Field edits for a permission row.
#[derive(Debug, Clone)]
pub enum PermissionEdit {
    /// The binary (or device) regex.
    Binary(String),
    /// The permission type.
    Kind(String),
    /// allow / ask / deny.
    Mode(String),
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
    /// Colour depth (`8` or `10`).
    Bitdepth(String),
    /// Enable or disable the output. Disabling writes `disable` as the mode;
    /// enabling restores the supplied mode (the display's current one, or
    /// `preferred` when nothing is known).
    Enabled(bool, String),
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
    /// Edit a workspace-rule field.
    Workspace(usize, WorkspaceEdit),
    /// Edit a variable field.
    Variable(usize, VariableEdit),
    /// Edit a bezier-curve field.
    Bezier(usize, BezierEdit),
    /// Edit an animation field.
    Animation(usize, AnimationEdit),
    /// Edit a gesture field.
    Gesture(usize, GestureEdit),
    /// Edit a device field.
    Device(usize, DeviceEdit),
    /// Edit a permission field.
    Permission(usize, PermissionEdit),
    /// Set a plugin path.
    Plugin(usize, String),
}

impl CollectionAction {
    /// The collection this action touches.
    #[must_use]
    pub fn collection(&self) -> CollectionId {
        match self {
            CollectionAction::Add(id)
            | CollectionAction::Remove(id, _)
            | CollectionAction::Duplicate(id, _)
            | CollectionAction::Move(id, _, _) => *id,
            CollectionAction::Keybind(..) => CollectionId::Keybinds,
            CollectionAction::WindowRule(..) => CollectionId::WindowRules,
            CollectionAction::LayerRule(..) => CollectionId::LayerRules,
            CollectionAction::Monitor(..) => CollectionId::Monitors,
            CollectionAction::Submap(..) => CollectionId::Submaps,
            CollectionAction::Env(..) => CollectionId::Env,
            CollectionAction::Exec(..) => CollectionId::Execs,
            CollectionAction::Workspace(..) => CollectionId::Workspaces,
            CollectionAction::Variable(..) => CollectionId::Variables,
            CollectionAction::Bezier(..) => CollectionId::Beziers,
            CollectionAction::Animation(..) => CollectionId::Animations,
            CollectionAction::Gesture(..) => CollectionId::Gestures,
            CollectionAction::Device(..) => CollectionId::Devices,
            CollectionAction::Permission(..) => CollectionId::Permissions,
            CollectionAction::Plugin(..) => CollectionId::Plugins,
        }
    }

    /// Whether this action adds, removes or reorders entries (as opposed to
    /// editing a field of one), which invalidates per-row UI state.
    #[must_use]
    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            CollectionAction::Add(_)
                | CollectionAction::Remove(..)
                | CollectionAction::Duplicate(..)
                | CollectionAction::Move(..)
        )
    }
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
            CollectionAction::Workspace(i, edit) => {
                self.touch(CollectionId::Workspaces);
                if let Some(t) = self.config.workspaces.get_mut(i) {
                    apply_workspace(&mut t.value, edit);
                }
            }
            CollectionAction::Variable(i, edit) => {
                self.touch(CollectionId::Variables);
                if let Some(t) = self.config.variables.get_mut(i) {
                    match edit {
                        VariableEdit::Name(s) => {
                            t.value.name = s.trim_start_matches('$').to_string();
                        }
                        VariableEdit::Value(s) => t.value.value = s,
                    }
                }
            }
            CollectionAction::Bezier(i, edit) => {
                self.touch(CollectionId::Beziers);
                if let Some(t) = self.config.beziers.get_mut(i) {
                    let b = &mut t.value;
                    match edit {
                        BezierEdit::Name(s) => b.name = s,
                        BezierEdit::Coord(0, v) => b.p0.x = v,
                        BezierEdit::Coord(1, v) => b.p0.y = v,
                        BezierEdit::Coord(2, v) => b.p1.x = v,
                        BezierEdit::Coord(_, v) => b.p1.y = v,
                    }
                }
            }
            CollectionAction::Animation(i, edit) => {
                self.touch(CollectionId::Animations);
                if let Some(t) = self.config.animations.get_mut(i) {
                    let a = &mut t.value;
                    match edit {
                        AnimationEdit::Name(s) => a.name = s,
                        AnimationEdit::Enabled(on) => a.enabled = on,
                        AnimationEdit::Speed(v) => a.speed = (v * 10.0).round() / 10.0,
                        AnimationEdit::Curve(s) => a.curve = s,
                        AnimationEdit::Style(s) => {
                            a.style = (!s.trim().is_empty()).then_some(s);
                        }
                    }
                }
            }
            CollectionAction::Gesture(i, edit) => {
                self.touch(CollectionId::Gestures);
                if let Some(t) = self.config.gestures.get_mut(i) {
                    let g = &mut t.value;
                    match edit {
                        GestureEdit::Fingers(n) => g.fingers = n.clamp(2, 9),
                        GestureEdit::Direction(s) => g.direction = s,
                        GestureEdit::ToggleMod(name, on) => g.mods = toggle_mod(&g.mods, &name, on),
                        GestureEdit::Action(s) => {
                            // Arguments mean different things per action.
                            if g.action != s {
                                g.args.clear();
                            }
                            g.action = s;
                        }
                        GestureEdit::Args(s) => g.args = s,
                        GestureEdit::Bypass(on) => g.bypass_inhibit = on,
                    }
                }
            }
            CollectionAction::Device(i, edit) => {
                self.touch(CollectionId::Devices);
                if let Some(t) = self.config.devices.get_mut(i) {
                    let d = &mut t.value;
                    match edit {
                        DeviceEdit::Name(s) => d.name = s,
                        DeviceEdit::AddOption => {
                            // Offer the most commonly overridden option first.
                            let next = hyprconf_core::structured::DEVICE_OPTIONS
                                .iter()
                                .map(|(k, _)| *k)
                                .find(|k| !d.options.iter().any(|(o, _)| o == k))
                                .unwrap_or("sensitivity");
                            d.options.push((next.to_string(), String::new()));
                        }
                        DeviceEdit::RemoveOption(j) => {
                            if j < d.options.len() {
                                d.options.remove(j);
                            }
                        }
                        DeviceEdit::OptionKey(j, k) => {
                            if let Some(o) = d.options.get_mut(j) {
                                o.0 = k;
                            }
                        }
                        DeviceEdit::OptionValue(j, v) => {
                            if let Some(o) = d.options.get_mut(j) {
                                o.1 = v;
                            }
                        }
                    }
                }
            }
            CollectionAction::Permission(i, edit) => {
                self.touch(CollectionId::Permissions);
                if let Some(t) = self.config.permissions.get_mut(i) {
                    match edit {
                        PermissionEdit::Binary(s) => t.value.binary = s,
                        PermissionEdit::Kind(s) => t.value.kind = s,
                        PermissionEdit::Mode(s) => t.value.mode = s,
                    }
                }
            }
            CollectionAction::Plugin(i, path) => {
                self.touch(CollectionId::Plugins);
                if let Some(t) = self.config.plugins.get_mut(i) {
                    t.value.path = path;
                }
            }
        }
    }

    fn touch(&mut self, id: CollectionId) {
        self.touched.insert(id);
    }

    /// Edit the rule that governs a physically attached output, creating one if
    /// the config is silent about it.
    ///
    /// The Monitors screen is driven by detected hardware, but the file is a
    /// list of rules — this is the bridge. `seed` is the display's current
    /// state, used both to author a faithful new rule (so adopting a monitor
    /// changes nothing about how it currently runs) and to restore a mode when
    /// re-enabling.
    ///
    /// Returns the index of the rule that was edited.
    pub fn edit_monitor(&mut self, seed: &MonitorRule, edit: MonitorEdit) -> usize {
        self.touch(CollectionId::Monitors);
        let index = match monitor_rule_index(&self.config.monitors, &seed.name) {
            Some(i) => i,
            None => {
                self.config.monitors.push(Tracked::new(seed.clone()));
                self.config.monitors.len() - 1
            }
        };
        if let Some(t) = self.config.monitors.get_mut(index) {
            apply_monitor(&mut t.value, edit);
        }
        index
    }
}

/// The index of the rule that governs `connector`, if any.
///
/// Later rules win in Hyprland, so the *last* match is the effective one — the
/// same rule the compositor would apply.
#[must_use]
pub fn monitor_rule_index(monitors: &[Tracked<MonitorRule>], connector: &str) -> Option<usize> {
    monitors
        .iter()
        .rposition(|t| monitor_rule_targets(&t.value, connector))
}

/// Whether a rule's selector names one specific connector.
///
/// Deliberately narrow: only an exact connector match counts. Wildcards (`*`,
/// empty) and `desc:` selectors *do* affect the output, but they are catch-alls
/// or EDID matches — silently rewriting one because the user tweaked the scale
/// of a single screen would change every other display too. Those stay visible
/// and editable as raw rules instead.
#[must_use]
pub fn monitor_rule_targets(rule: &MonitorRule, connector: &str) -> bool {
    !connector.is_empty() && rule.name.trim().eq_ignore_ascii_case(connector)
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
            description: None,
        })),
        CollectionId::Gestures => {
            // Start from a combination nothing already claims, so a new gesture
            // is never born overshadowed.
            const CANDIDATES: [(u32, &str); 7] = [
                (3, "horizontal"),
                (3, "vertical"),
                (4, "horizontal"),
                (4, "vertical"),
                (3, "pinch"),
                (4, "pinch"),
                (2, "pinch"),
            ];
            let taken = |fingers: u32, direction: &str| {
                c.gestures.iter().any(|t| {
                    let e = &t.value;
                    e.action != "unset"
                        && e.fingers == fingers
                        && e.mods.trim().is_empty()
                        && gesture_covers(&e.direction, direction)
                })
            };
            let (fingers, direction) = CANDIDATES
                .into_iter()
                .find(|(f, d)| !taken(*f, d))
                .unwrap_or((3, "horizontal"));
            c.gestures.push(Tracked::new(Gesture {
                fingers,
                direction: direction.into(),
                mods: String::new(),
                scale: None,
                action: "workspace".into(),
                args: String::new(),
                bypass_inhibit: false,
            }));
        }
        CollectionId::Devices => c.devices.push(Tracked::new(Device {
            name: String::new(),
            options: vec![("sensitivity".into(), "0".into())],
        })),
        CollectionId::Permissions => c.permissions.push(Tracked::new(Permission {
            binary: String::new(),
            kind: "screencopy".into(),
            mode: "ask".into(),
        })),
        CollectionId::Plugins => c.plugins.push(Tracked::new(Plugin {
            path: String::new(),
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
        CollectionId::Gestures => go!(c.gestures),
        CollectionId::Devices => go!(c.devices),
        CollectionId::Permissions => go!(c.permissions),
        CollectionId::Plugins => go!(c.plugins),
    }
}

fn apply_workspace(w: &mut WorkspaceRule, edit: WorkspaceEdit) {
    // Workspace rules share the window-rule matcher syntax (`key:value, …`).
    let mut rules = parse_matchers(&w.rules);
    match edit {
        WorkspaceEdit::Selector(s) => {
            w.selector = s;
            return;
        }
        WorkspaceEdit::Rules(s) => {
            w.rules = s;
            return;
        }
        WorkspaceEdit::AddRule => {
            let next = hyprconf_core::lua::workspace_rule_keys()
                .into_iter()
                .find(|k| !rules.iter().any(|(r, _)| r == k))
                .unwrap_or("monitor");
            rules.push((next.to_string(), String::new()));
        }
        WorkspaceEdit::RemoveRule(i) => {
            if i < rules.len() {
                rules.remove(i);
            }
        }
        WorkspaceEdit::RuleKey(i, k) => {
            if let Some(r) = rules.get_mut(i) {
                r.0 = k;
            }
        }
        WorkspaceEdit::RuleValue(i, v) => {
            if let Some(r) = rules.get_mut(i) {
                r.1 = v;
            }
        }
    }
    w.rules = build_rules(&rules);
}

/// Like [`build_matchers`], but keeps a valueless key as `key:` so a freshly
/// added rule row survives until the user fills it in.
fn build_rules(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(", ")
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
            BindFlag::LongPress => kb.flags.long_press = on,
            BindFlag::Click => kb.flags.click = on,
            BindFlag::Drag => kb.flags.drag = on,
            BindFlag::DontInhibit => kb.flags.dont_inhibit = on,
        },
        KeybindEdit::Description(s) => {
            kb.description = (!s.trim().is_empty()).then_some(s);
        }
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
        MonitorEdit::Transform(s) => set_extra(&mut m.extra, "transform", &s),
        MonitorEdit::Vrr(s) => set_extra(&mut m.extra, "vrr", &s),
        MonitorEdit::Mirror(s) => set_extra(&mut m.extra, "mirror", &s),
        MonitorEdit::Bitdepth(s) => set_extra(&mut m.extra, "bitdepth", &s),
        MonitorEdit::Enabled(on, restore) => {
            m.mode = if on {
                // Re-enabling a monitor whose stored mode is literally `disable`
                // needs a real mode; anything else the user typed is theirs to
                // keep.
                if m.is_disabled() {
                    let restore = restore.trim();
                    if restore.is_empty() {
                        "preferred".to_string()
                    } else {
                        restore.to_string()
                    }
                } else {
                    std::mem::take(&mut m.mode)
                }
            } else {
                "disable".to_string()
            };
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

/// Set (or, with an empty `value`, clear) one `keyword value` pair in a
/// monitor's trailing tokens, **leaving every other token untouched**.
///
/// This matters: `extra` is an open-ended token list and Hyprland keeps adding
/// modifiers (`cm`, `sdrbrightness`, `icc`, …). Rebuilding it from the handful
/// of fields the UI knows about would silently delete anything else the user
/// had written.
pub fn set_extra(extra: &mut Vec<String>, keyword: &str, value: &str) {
    let value = value.trim();
    match extra.iter().position(|t| t == keyword) {
        Some(i) => {
            if value.is_empty() {
                // Remove the keyword and its value (if the pair is complete).
                let end = (i + 2).min(extra.len());
                extra.drain(i..end);
            } else if i + 1 < extra.len() {
                extra[i + 1] = value.to_string();
            } else {
                extra.push(value.to_string());
            }
        }
        None if !value.is_empty() => {
            extra.push(keyword.to_string());
            extra.push(value.to_string());
        }
        None => {}
    }
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
    // Mouse binds (`bindm`) use the argument-less drag forms of `movewindow` /
    // `resizewindow`.
    if !kb.flags.mouse && dispatcher_needs_args(&kb.dispatcher) && kb.args.trim().is_empty() {
        return Some(format!("`{}` needs arguments", kb.dispatcher));
    }
    None
}

/// Whether a dispatcher requires arguments: its help names arguments that are
/// not marked optional (`[…]`).
fn dispatcher_needs_args(dispatcher: &str) -> bool {
    dispatcher_help(dispatcher)
        .and_then(|help| help.split_once("args: "))
        .is_some_and(|(_, args)| !args.starts_with('['))
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

/// Returns a problem with a workspace rule, if any.
#[must_use]
pub fn workspace_issue(w: &WorkspaceRule) -> Option<String> {
    if w.selector.trim().is_empty() {
        return Some("a workspace is required".into());
    }
    None
}

/// Returns a problem with a variable, if any.
#[must_use]
pub fn variable_issue(v: &Variable) -> Option<String> {
    if v.name.trim().is_empty() || v.name.contains(char::is_whitespace) {
        return Some("a name without spaces is required".into());
    }
    None
}

/// Returns a problem with a gesture, if any — mirroring what
/// `Hyprland --verify-config` rejects.
#[must_use]
pub fn gesture_issue(g: &Gesture) -> Option<String> {
    if !(2..=9).contains(&g.fingers) {
        return Some("2 to 9 fingers".into());
    }
    if !GESTURE_DIRECTIONS
        .iter()
        .any(|(d, _)| d.eq_ignore_ascii_case(&g.direction))
    {
        return Some(format!("`{}` is not a direction", g.direction));
    }
    if !GESTURE_ACTIONS.iter().any(|(a, _)| *a == g.action) {
        return Some(format!("`{}` is not a gesture action", g.action));
    }
    if g.action == "special" && g.args.trim().is_empty() {
        return Some("special needs a workspace name".into());
    }
    if g.action == "dispatcher" && g.args.trim().is_empty() {
        return Some("dispatcher needs a dispatcher name".into());
    }
    None
}

/// Whether a gesture bound to `earlier` swallows one bound to `later` (same
/// fingers and modifiers assumed): equal directions, or a broader one first.
#[must_use]
pub fn gesture_covers(earlier: &str, later: &str) -> bool {
    let (e, l) = (earlier.to_ascii_lowercase(), later.to_ascii_lowercase());
    e == l
        || match e.as_str() {
            "swipe" => matches!(
                l.as_str(),
                "horizontal" | "vertical" | "left" | "right" | "up" | "down"
            ),
            "horizontal" => matches!(l.as_str(), "left" | "right"),
            "vertical" => matches!(l.as_str(), "up" | "down"),
            "pinch" => matches!(l.as_str(), "pinchin" | "pinchout"),
            _ => false,
        }
}

/// Modifiers as a comparable set (`SUPER SHIFT` = `SHIFT_SUPER` = `super+shift`).
fn mod_set(mods: &str) -> Vec<String> {
    let mut set: Vec<String> = mods
        .split(|c: char| c.is_whitespace() || c == '_' || c == '+')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    set.sort();
    set.dedup();
    set
}

/// The earlier gesture that overshadows gesture `i`, if any — Hyprland rejects
/// the later one (*"Gesture will be overshadowed by a previous gesture"*).
/// `unset` entries are honoured: they remove the gesture they match.
#[must_use]
pub fn gesture_shadowed_by(gestures: &[Tracked<Gesture>], i: usize) -> Option<usize> {
    let g = &gestures.get(i)?.value;
    if g.action == "unset" {
        return None;
    }
    let mut live: Vec<usize> = Vec::new();
    for (j, earlier) in gestures[..i].iter().enumerate() {
        let e = &earlier.value;
        if e.action == "unset" {
            live.retain(|&k| {
                let k = &gestures[k].value;
                !(k.fingers == e.fingers
                    && k.direction.eq_ignore_ascii_case(&e.direction)
                    && mod_set(&k.mods) == mod_set(&e.mods)
                    && k.scale == e.scale)
            });
        } else {
            live.push(j);
        }
    }
    live.into_iter().find(|&j| {
        let e = &gestures[j].value;
        e.fingers == g.fingers
            && mod_set(&e.mods) == mod_set(&g.mods)
            && gesture_covers(&e.direction, &g.direction)
    })
}

/// Returns a problem with a device block, if any.
#[must_use]
pub fn device_issue(d: &Device) -> Option<String> {
    if d.name.trim().is_empty() {
        return Some("a device name is required (see hyprctl devices)".into());
    }
    if d.options.iter().any(|(k, _)| k.trim().is_empty()) {
        return Some("an option has no name".into());
    }
    None
}

/// Returns a problem with a permission rule, if any.
#[must_use]
pub fn permission_issue(p: &Permission) -> Option<String> {
    if p.binary.trim().is_empty() {
        return Some("a binary regex is required".into());
    }
    if !PERMISSION_TYPES.iter().any(|(t, _)| *t == p.kind) {
        return Some(format!("`{}` is not a permission type", p.kind));
    }
    if !PERMISSION_MODES.iter().any(|(m, _)| *m == p.mode) {
        return Some(format!("`{}` is not allow/ask/deny", p.mode));
    }
    None
}

/// Returns a problem with a plugin entry, if any.
#[must_use]
pub fn plugin_issue(p: &Plugin) -> Option<String> {
    if !p.path.trim().starts_with('/') {
        return Some("an absolute path to the .so is required".into());
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
            EditAction::SetGap(path, _) => Some(format!("gap:{path}")),
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
            | EditAction::RemoveStop(p, _)
            | EditAction::SetGap(p, _)
            | EditAction::Revert(p) => Some(p),
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
            // Flipping a display off and straight back on must be two undo
            // steps, so the toggle deliberately opts out of coalescing.
            CollectionAction::Monitor(_, MonitorEdit::Enabled(..)) => return None,
            CollectionAction::Monitor(i, edit) => format!("mon:{i}:{}", monitor_field_tag(edit)),
            CollectionAction::Submap(i, _) => format!("sm:{i}"),
            CollectionAction::Env(i, EnvEdit::Name(_)) => format!("env:{i}:name"),
            CollectionAction::Env(i, EnvEdit::Value(_)) => format!("env:{i}:value"),
            CollectionAction::Exec(i, ExecEdit::Command(_)) => format!("exec:{i}:cmd"),
            CollectionAction::Keybind(i, KeybindEdit::Description(_)) => format!("kb:{i}:desc"),
            CollectionAction::Workspace(i, WorkspaceEdit::Selector(_)) => format!("ws:{i}:sel"),
            CollectionAction::Workspace(i, WorkspaceEdit::Rules(_)) => format!("ws:{i}:raw"),
            CollectionAction::Workspace(i, WorkspaceEdit::RuleValue(j, _)) => {
                format!("ws:{i}:rv:{j}")
            }
            CollectionAction::Variable(i, VariableEdit::Name(_)) => format!("var:{i}:name"),
            CollectionAction::Variable(i, VariableEdit::Value(_)) => format!("var:{i}:value"),
            CollectionAction::Bezier(i, BezierEdit::Name(_)) => format!("bz:{i}:name"),
            CollectionAction::Bezier(i, BezierEdit::Coord(j, _)) => format!("bz:{i}:{j}"),
            CollectionAction::Animation(i, AnimationEdit::Speed(_)) => format!("an:{i}:speed"),
            CollectionAction::Animation(i, AnimationEdit::Style(_)) => format!("an:{i}:style"),
            CollectionAction::Gesture(i, GestureEdit::Args(_)) => format!("ge:{i}:args"),
            CollectionAction::Device(i, DeviceEdit::Name(_)) => format!("dev:{i}:name"),
            CollectionAction::Device(i, DeviceEdit::OptionValue(j, _)) => format!("dev:{i}:v:{j}"),
            CollectionAction::Permission(i, PermissionEdit::Binary(_)) => format!("perm:{i}:bin"),
            CollectionAction::Plugin(i, _) => format!("plugin:{i}"),
            _ => return None,
        };
        Some(text)
    }
}

/// A stable per-field tag, so consecutive edits to the *same* field of the same
/// monitor coalesce into one undo step (typing a scale, dragging a monitor).
#[must_use]
pub fn monitor_field_tag(edit: &MonitorEdit) -> &'static str {
    match edit {
        MonitorEdit::Name(_) => "name",
        MonitorEdit::Mode(_) => "mode",
        MonitorEdit::Position(_) => "position",
        MonitorEdit::Scale(_) => "scale",
        MonitorEdit::Transform(_) => "transform",
        MonitorEdit::Vrr(_) => "vrr",
        MonitorEdit::Mirror(_) => "mirror",
        MonitorEdit::Bitdepth(_) => "bitdepth",
        // Not coalesced: flipping a display off and on again must be two steps.
        MonitorEdit::Enabled(..) => "enabled",
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
            diagnostics: Vec::new(),
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

    #[test]
    fn editing_one_modifier_preserves_the_others() {
        // `extra` is open-ended and Hyprland keeps adding modifiers; touching
        // one must never drop tokens the UI doesn't model.
        let mut extra: Vec<String> = ["cm", "hdr", "sdrbrightness", "1.2", "transform", "1"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        set_extra(&mut extra, "transform", "3");
        assert_eq!(extra_field(&extra, "transform"), "3");
        assert_eq!(extra_field(&extra, "cm"), "hdr");
        assert_eq!(extra_field(&extra, "sdrbrightness"), "1.2");

        // A new keyword is appended…
        set_extra(&mut extra, "vrr", "2");
        assert_eq!(extra_field(&extra, "vrr"), "2");
        // …and an emptied one is removed outright, pair and all.
        set_extra(&mut extra, "transform", "");
        assert_eq!(extra_field(&extra, "transform"), "");
        assert!(!extra.iter().any(|t| t == "transform"));
        assert_eq!(extra_field(&extra, "cm"), "hdr", "neighbours are intact");
        assert_eq!(extra.len(), 6, "only the pair was removed: {extra:?}");
    }

    /// A rule describing a detected display's current state.
    fn seed(name: &str) -> MonitorRule {
        MonitorRule {
            name: name.into(),
            mode: "1920x1080@60.00".into(),
            position: "0x0".into(),
            scale: "1".into(),
            extra: Vec::new(),
        }
    }

    #[test]
    fn editing_an_unconfigured_display_adopts_it_faithfully() {
        let (mut l, _schema) = loaded();
        assert!(l.config.monitors.is_empty());

        // The first edit to a display the config never mentioned creates a rule
        // seeded from how it is *currently* running, so nothing else moves.
        let i = l.edit_monitor(&seed("DP-1"), MonitorEdit::Scale("1.5".into()));
        assert_eq!(i, 0);
        assert_eq!(l.config.monitors.len(), 1);
        let m = &l.config.monitors[0].value;
        assert_eq!(m.name, "DP-1");
        assert_eq!(m.mode, "1920x1080@60.00", "current mode is preserved");
        assert_eq!(m.position, "0x0", "current position is preserved");
        assert_eq!(m.scale, "1.5", "…and the edit applied");

        // A second edit reuses that rule rather than stacking another.
        l.edit_monitor(&seed("DP-1"), MonitorEdit::Transform("1".into()));
        assert_eq!(l.config.monitors.len(), 1);
        assert_eq!(
            extra_field(&l.config.monitors[0].value.extra, "transform"),
            "1"
        );
    }

    #[test]
    fn the_last_matching_rule_wins_as_hyprland_does() {
        let (mut l, _schema) = loaded();
        for _ in 0..2 {
            l.apply_collection(CollectionAction::Add(CollectionId::Monitors));
        }
        l.config.monitors[0].value.name = "DP-1".into();
        l.config.monitors[1].value.name = "DP-1".into();

        assert_eq!(monitor_rule_index(&l.config.monitors, "DP-1"), Some(1));
        assert_eq!(
            l.edit_monitor(&seed("DP-1"), MonitorEdit::Scale("2".into())),
            1
        );
        assert_eq!(l.config.monitors[1].value.scale, "2");
    }

    #[test]
    fn wildcard_and_desc_rules_are_never_hijacked() {
        // A catch-all governs the output, but rewriting it would move every
        // other display too — so it is left alone and a specific rule is added.
        let (mut l, _schema) = loaded();
        l.apply_collection(CollectionAction::Add(CollectionId::Monitors));
        l.config.monitors[0].value.name = ",".into();
        l.config.monitors[0].value.name = "*".into();

        assert_eq!(monitor_rule_index(&l.config.monitors, "DP-1"), None);
        l.edit_monitor(&seed("DP-1"), MonitorEdit::Scale("2".into()));
        assert_eq!(l.config.monitors.len(), 2);
        assert_eq!(l.config.monitors[0].value.name, "*", "untouched");
        assert_eq!(l.config.monitors[1].value.name, "DP-1");
    }

    #[test]
    fn disabling_and_re_enabling_round_trips_the_mode() {
        let (mut l, _schema) = loaded();
        let s = seed("DP-1");

        l.edit_monitor(&s, MonitorEdit::Enabled(false, s.mode.clone()));
        assert_eq!(l.config.monitors[0].value.mode, "disable");

        l.edit_monitor(&s, MonitorEdit::Enabled(true, s.mode.clone()));
        assert_eq!(l.config.monitors[0].value.mode, "1920x1080@60.00");

        // With nothing to restore, `preferred` lets Hyprland decide.
        l.edit_monitor(&s, MonitorEdit::Enabled(false, String::new()));
        l.edit_monitor(&s, MonitorEdit::Enabled(true, String::new()));
        assert_eq!(l.config.monitors[0].value.mode, "preferred");
    }

    #[test]
    fn toggling_a_display_is_never_coalesced_into_one_undo_step() {
        let off = CollectionAction::Monitor(0, MonitorEdit::Enabled(false, String::new()));
        assert_eq!(off.coalesce_key(), None);
        // …while continuous edits still are.
        let drag = CollectionAction::Monitor(0, MonitorEdit::Position("10x0".into()));
        assert_eq!(drag.coalesce_key().as_deref(), Some("mon:0:position"));
    }

    fn empty_loaded() -> Loaded {
        Loaded {
            format: ConfigFormat::Conf,
            source: PathBuf::from("test"),
            included_files: 0,
            diagnostics: Vec::new(),
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
        let path = "misc:background_color";
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
        let path = "misc:background_color";
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

    fn gesture(fingers: u32, direction: &str, mods: &str, action: &str) -> Tracked<Gesture> {
        Tracked::new(Gesture {
            fingers,
            direction: direction.into(),
            mods: mods.into(),
            scale: None,
            action: action.into(),
            args: String::new(),
            bypass_inhibit: false,
        })
    }

    /// Mirrors what `Hyprland --verify-config` reports as "overshadowed".
    #[test]
    fn gesture_shadowing_matches_hyprland() {
        let g = |list: Vec<Tracked<Gesture>>, i| gesture_shadowed_by(&list, i);
        // A broader direction first swallows a narrower one…
        assert_eq!(
            g(
                vec![
                    gesture(3, "horizontal", "", "workspace"),
                    gesture(3, "left", "", "close")
                ],
                1
            ),
            Some(0)
        );
        assert_eq!(
            g(
                vec![
                    gesture(3, "swipe", "", "move"),
                    gesture(3, "up", "", "close")
                ],
                1
            ),
            Some(0)
        );
        // …but specific-first is fine, as are different fingers or modifiers.
        assert_eq!(
            g(
                vec![
                    gesture(3, "left", "", "close"),
                    gesture(3, "horizontal", "", "workspace")
                ],
                1
            ),
            None
        );
        assert_eq!(
            g(
                vec![
                    gesture(3, "horizontal", "", "workspace"),
                    gesture(4, "left", "", "close")
                ],
                1
            ),
            None
        );
        assert_eq!(
            g(
                vec![
                    gesture(3, "horizontal", "", "workspace"),
                    gesture(3, "left", "SUPER", "close")
                ],
                1
            ),
            None
        );
        // Modifier spelling doesn't matter.
        assert_eq!(
            g(
                vec![
                    gesture(3, "down", "SUPER SHIFT", "close"),
                    gesture(3, "down", "shift_super", "move")
                ],
                1
            ),
            Some(0)
        );
        // An `unset` frees the slot again.
        assert_eq!(
            g(
                vec![
                    gesture(3, "horizontal", "", "workspace"),
                    gesture(3, "horizontal", "", "unset"),
                    gesture(3, "left", "", "close"),
                ],
                2
            ),
            None
        );
    }

    #[test]
    fn new_gestures_start_unshadowed() {
        let (mut l, _schema) = loaded();
        for _ in 0..4 {
            l.apply_collection(CollectionAction::Add(CollectionId::Gestures));
        }
        for i in 0..4 {
            assert_eq!(
                gesture_shadowed_by(&l.config.gestures, i),
                None,
                "gesture {i}"
            );
            assert_eq!(gesture_issue(&l.config.gestures[i].value), None);
        }
    }

    /// Resetting a value the file sets must be *saved*: it used to re-baseline
    /// the option as clean, so preserve-mode saves silently skipped it.
    #[test]
    fn reset_is_a_saveable_edit_and_revert_returns_to_the_file_value() {
        let (mut l, schema) = loaded();
        let path = "decoration:rounding";
        // Pretend the file set rounding = 10.
        Arc::make_mut(&mut l.baseline).insert(path.into(), Value::Int(10));
        l.config.set(path, Value::Int(10));

        l.apply(EditAction::Reset(path.into()), schema);
        assert_eq!(l.config.get(path), Some(&Value::Int(0)));
        assert!(
            l.is_dirty(path),
            "a reset away from the file value is a change"
        );

        l.apply(EditAction::Revert(path.into()), schema);
        assert_eq!(l.config.get(path), Some(&Value::Int(10)));
        assert!(!l.is_dirty(path));
    }

    #[test]
    fn gap_edits_link_split_and_validate() {
        let (mut l, schema) = loaded();
        let path = "general:gaps_out";
        l.apply(
            EditAction::EditText(path.into(), Slot::Main, "5 10".into()),
            schema,
        );
        assert_eq!(
            l.config.get(path),
            Some(&Value::CssGap(CssGap::from_sides([5, 10, 5, 10])))
        );
        l.apply(
            EditAction::EditText(path.into(), Slot::Side(3), "7".into()),
            schema,
        );
        assert_eq!(
            l.config.get(path),
            Some(&Value::CssGap(CssGap::from_sides([5, 10, 5, 7])))
        );
        l.apply(
            EditAction::EditText(path.into(), Slot::Side(0), "-1".into()),
            schema,
        );
        assert!(
            l.field_error(path, Slot::Side(0)).is_some(),
            "gaps can't be negative"
        );
        l.apply(EditAction::SetGap(path.into(), CssGap::uniform(4)), schema);
        assert_eq!(l.config.get(path), Some(&Value::CssGap(CssGap::uniform(4))));
        assert!(l.field_error(path, Slot::Side(0)).is_none());
    }

    #[test]
    fn new_collections_are_editable() {
        let (mut l, _schema) = loaded();
        l.apply_collection(CollectionAction::Add(CollectionId::Devices));
        l.apply_collection(CollectionAction::Device(
            0,
            DeviceEdit::Name("epic-mouse".into()),
        ));
        l.apply_collection(CollectionAction::Device(0, DeviceEdit::AddOption));
        assert_eq!(l.config.devices[0].value.options.len(), 2);
        assert_eq!(device_issue(&l.config.devices[0].value), None);

        l.apply_collection(CollectionAction::Add(CollectionId::Permissions));
        assert!(permission_issue(&l.config.permissions[0].value).is_some());
        l.apply_collection(CollectionAction::Permission(
            0,
            PermissionEdit::Binary("/usr/bin/grim".into()),
        ));
        assert_eq!(permission_issue(&l.config.permissions[0].value), None);

        l.apply_collection(CollectionAction::Add(CollectionId::Workspaces));
        l.apply_collection(CollectionAction::Workspace(
            0,
            WorkspaceEdit::Selector("1".into()),
        ));
        l.apply_collection(CollectionAction::Workspace(0, WorkspaceEdit::AddRule));
        l.apply_collection(CollectionAction::Workspace(
            0,
            WorkspaceEdit::RuleValue(0, "DP-1".into()),
        ));
        assert_eq!(l.config.workspaces[0].value.rules, "monitor:DP-1");

        l.apply_collection(CollectionAction::Add(CollectionId::Animations));
        l.apply_collection(CollectionAction::Animation(0, AnimationEdit::Speed(4.26)));
        assert_eq!(l.config.animations[0].value.speed, 4.3);

        l.apply_collection(CollectionAction::Add(CollectionId::Keybinds));
        l.apply_collection(CollectionAction::Keybind(
            0,
            KeybindEdit::Flag(BindFlag::LongPress, true),
        ));
        l.apply_collection(CollectionAction::Keybind(
            0,
            KeybindEdit::Description("Close".into()),
        ));
        let kb = &l.config.keybinds[0].value;
        assert!(kb.flags.long_press);
        assert_eq!(kb.keyword(), "bindod");
    }

    #[test]
    fn mouse_binds_need_no_dispatcher_arguments() {
        let kb = Keybind {
            flags: KeybindFlags {
                mouse: true,
                ..KeybindFlags::default()
            },
            mods: "SUPER".into(),
            key: "mouse:272".into(),
            dispatcher: "movewindow".into(),
            args: String::new(),
            submap: None,
            description: None,
        };
        assert_eq!(keybind_issue(&kb), None);
        let keyboard = Keybind {
            flags: KeybindFlags::default(),
            ..kb
        };
        assert!(keybind_issue(&keyboard).is_some());
    }
}
