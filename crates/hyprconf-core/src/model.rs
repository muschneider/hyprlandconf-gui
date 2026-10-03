// SPDX-License-Identifier: MIT OR Apache-2.0
//! The in-memory, format-agnostic configuration model.
//!
//! [`Config`] is the central abstraction of hyprconf: it holds a config's
//! meaning independently of whether it came from (or will be written to) a
//! `.lua` or `.conf` file. Scalar options are keyed by their dotted path; the
//! structured constructs live in ordered `Vec`s because their order is
//! semantically significant.
//!
//! Every stored item is wrapped in [`Tracked`], which carries [`Provenance`]
//! (origin file, byte span, surrounding comments). That metadata is unused in
//! this step but is the hook that lets later steps round-trip a file while
//! preserving comments, blank lines and ordering.

use std::path::PathBuf;

use indexmap::IndexMap;

use crate::schema::Schema;
use crate::structured::{
    Animation, Bezier, Device, EnvVar, Exec, Gesture, Keybind, LayerRule, MonitorRule, Permission,
    Plugin, Submap, Variable, WindowRule, WorkspaceRule,
};
use crate::value::Value;

/// Which on-disk format a [`Config`] was read from / should be written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// Hyprland's Lua format (`hyprland.lua`, the `hl` table).
    Lua,
    /// Legacy hyprlang (`hyprland.conf`, `key = value`).
    Conf,
}

/// A half-open byte range `[start, end)` into an original source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Start byte offset (inclusive).
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

impl Span {
    /// Construct a span.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// Where a value came from in its original file, for round-trip fidelity.
///
/// All fields are optional: a synthetic value (e.g. a schema default) has an
/// empty provenance, whereas a parsed value records as much origin detail as
/// the parser can recover.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Provenance {
    /// The file this item was read from (relevant once `source=`/`require()`
    /// pull in multiple files).
    pub source: Option<PathBuf>,
    /// The byte range of the item in its source file.
    pub span: Option<Span>,
    /// The 1-based line number of the item.
    pub line: Option<u32>,
    /// Comment lines that immediately precede the item (without delimiters).
    pub leading_comments: Vec<String>,
    /// An inline comment trailing the item on the same line.
    pub trailing_comment: Option<String>,
}

impl Provenance {
    /// `true` if no origin information is recorded (i.e. a synthetic value).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.span.is_none()
            && self.line.is_none()
            && self.leading_comments.is_empty()
            && self.trailing_comment.is_none()
    }
}

/// A value paired with its [`Provenance`].
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked<T> {
    /// The wrapped value.
    pub value: T,
    /// Origin metadata for round-tripping.
    pub provenance: Provenance,
}

impl<T> Tracked<T> {
    /// Wrap a synthetic value with empty provenance.
    pub fn new(value: T) -> Self {
        Self {
            value,
            provenance: Provenance::default(),
        }
    }

    /// Wrap a value with explicit provenance.
    pub fn with_provenance(value: T, provenance: Provenance) -> Self {
        Self { value, provenance }
    }
}

/// The complete in-memory configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The format this config was read from (`None` for a synthetic config).
    pub format: Option<ConfigFormat>,
    /// The primary source file, if any.
    pub source: Option<PathBuf>,

    /// Scalar options, keyed by dotted path and kept in insertion order.
    pub options: IndexMap<String, Tracked<Value>>,

    /// `monitor = ...` directives, in order.
    pub monitors: Vec<Tracked<MonitorRule>>,
    /// `workspace = ...` directives, in order.
    pub workspaces: Vec<Tracked<WorkspaceRule>>,
    /// `windowrule`/`windowrulev2` directives, in order.
    pub window_rules: Vec<Tracked<WindowRule>>,
    /// `layerrule` directives, in order.
    pub layer_rules: Vec<Tracked<LayerRule>>,
    /// `bind*` directives, in order.
    pub keybinds: Vec<Tracked<Keybind>>,
    /// `submap` markers, in order.
    pub submaps: Vec<Tracked<Submap>>,
    /// `env`/`envd` directives, in order.
    pub env: Vec<Tracked<EnvVar>>,
    /// `exec*` directives, in order.
    pub execs: Vec<Tracked<Exec>>,
    /// `$variable` definitions, in order.
    pub variables: Vec<Tracked<Variable>>,
    /// `bezier` definitions, in order.
    pub beziers: Vec<Tracked<Bezier>>,
    /// `animation` directives, in order.
    pub animations: Vec<Tracked<Animation>>,
    /// `gesture` bindings, in order (later ones may be shadowed by earlier).
    pub gestures: Vec<Tracked<Gesture>>,
    /// `device { … }` blocks, in order.
    pub devices: Vec<Tracked<Device>>,
    /// `permission` rules, in order (first match wins).
    pub permissions: Vec<Tracked<Permission>>,
    /// `plugin` loads, in order.
    pub plugins: Vec<Tracked<Plugin>>,
}

impl Config {
    /// An empty config with no options, no collections and no format.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a config pre-populated with every option's schema default.
    ///
    /// Options are inserted in schema order; all structured collections start
    /// empty (Hyprland ships no monitors/binds/etc. by default). The result has
    /// no `format` or `source` since it is synthetic.
    #[must_use]
    pub fn default_from_schema(schema: &Schema) -> Self {
        let mut options = IndexMap::new();
        for section in schema.sections() {
            for opt in &section.options {
                options.insert(opt.path.clone(), Tracked::new(opt.default.clone()));
            }
        }

        Self {
            options,
            ..Self::default()
        }
    }

    /// Get a scalar option's current value by path.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&Value> {
        self.options.get(path).map(|tracked| &tracked.value)
    }

    /// Set a scalar option's value, preserving any existing provenance.
    ///
    /// If the path is new, it is appended with empty provenance. Returns the
    /// previous value, if any.
    pub fn set(&mut self, path: impl Into<String>, value: Value) -> Option<Value> {
        let path = path.into();
        match self.options.get_mut(&path) {
            Some(tracked) => Some(std::mem::replace(&mut tracked.value, value)),
            None => {
                self.options.insert(path, Tracked::new(value));
                None
            }
        }
    }

    /// The number of scalar options currently stored.
    #[must_use]
    pub fn option_count(&self) -> usize {
        self.options.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{Color, Gradient};

    #[test]
    fn empty_config_is_empty() {
        let config = Config::empty();
        assert_eq!(config.option_count(), 0);
        assert!(config.monitors.is_empty());
        assert!(config.keybinds.is_empty());
        assert!(config.format.is_none());
    }

    #[test]
    fn default_from_schema_populates_every_option() {
        let schema = Schema::load();
        let config = Config::default_from_schema(&schema);

        assert_eq!(config.option_count(), schema.option_count());
        for opt in schema.options() {
            let value = config
                .get(&opt.path)
                .unwrap_or_else(|| panic!("missing default for `{}`", opt.path));
            assert_eq!(value, &opt.default, "default mismatch for `{}`", opt.path);
        }
    }

    #[test]
    fn default_from_schema_has_empty_collections_and_provenance() {
        let schema = Schema::load();
        let config = Config::default_from_schema(&schema);

        assert!(config.monitors.is_empty());
        assert!(config.window_rules.is_empty());
        assert!(config.variables.is_empty());

        let rounding = config.options.get("decoration:rounding").unwrap();
        assert!(
            rounding.provenance.is_empty(),
            "synthetic values have empty provenance"
        );
    }

    #[test]
    fn known_defaults_are_correct() {
        let schema = Schema::load();
        let config = Config::default_from_schema(&schema);

        assert_eq!(config.get("decoration:rounding"), Some(&Value::Int(0)));
        // An open choice (custom `lua:<name>` layouts are valid), so a string.
        assert_eq!(
            config.get("general:layout"),
            Some(&Value::String("dwindle".into()))
        );
        assert_eq!(
            config.get("general:col.active_border"),
            Some(&Value::Gradient(Gradient::solid(Color::rgba(
                0xff, 0xff, 0xff, 0xff
            ))))
        );
    }

    #[test]
    fn set_updates_value_and_preserves_provenance() {
        let schema = Schema::load();
        let mut config = Config::default_from_schema(&schema);

        // Give an option some provenance, then mutate via `set`.
        let prov = Provenance {
            line: Some(42),
            ..Provenance::default()
        };
        config.options.insert(
            "decoration:rounding".into(),
            Tracked::with_provenance(Value::Int(0), prov.clone()),
        );

        let previous = config.set("decoration:rounding", Value::Int(12));
        assert_eq!(previous, Some(Value::Int(0)));
        assert_eq!(config.get("decoration:rounding"), Some(&Value::Int(12)));
        assert_eq!(
            config
                .options
                .get("decoration:rounding")
                .unwrap()
                .provenance,
            prov,
            "provenance must survive a value update"
        );
    }

    #[test]
    fn set_appends_unknown_path() {
        let mut config = Config::empty();
        assert_eq!(config.set("custom:thing", Value::Bool(true)), None);
        assert_eq!(config.get("custom:thing"), Some(&Value::Bool(true)));
    }
}
