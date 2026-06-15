// SPDX-License-Identifier: MIT OR Apache-2.0
//! The data-driven description of Hyprland's configuration surface.
//!
//! The [`Schema`] is the single source of truth for *what options exist*, their
//! value types, defaults, ranges and documentation. Nothing else in the
//! codebase hardcodes option knowledge ad hoc — UI panels, validation and
//! serialization all consult the schema.
//!
//! The concrete option data lives in the [`mod@data`] submodule as compact
//! `const` builders (chosen over an embedded RON/TOML blob so every default is
//! type-checked by the compiler and `Schema::load` cannot fail at runtime). The
//! option *set* is verified against the vendored upstream stub
//! (`meta/hyprland-config-keys.txt`) by a unit test in this module — see that
//! file's `README.md` for provenance and the regeneration procedure.

mod data;

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::value::Value;
use crate::CoreError;

/// The kind of value an option (or structured collection element) holds.
///
/// Scalar kinds back a single keyed [`Value`]; structured kinds back an ordered
/// collection of [`crate::structured`] items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueType {
    // ---- scalar kinds ----
    /// A boolean.
    Bool,
    /// A signed integer.
    Int,
    /// A floating-point number.
    Float,
    /// A single color.
    Color,
    /// A color gradient.
    Gradient,
    /// Free-form text.
    String,
    /// An enumeration with a fixed, non-empty set of variants.
    Enum(Vec<EnumVariant>),
    /// A 2D vector.
    Vec2,

    // ---- structured kinds (back ordered collections) ----
    /// A key/mouse binding.
    Keybind,
    /// A window rule.
    WindowRule,
    /// A layer-surface rule.
    LayerRule,
    /// A monitor directive.
    MonitorRule,
    /// A workspace rule.
    Workspace,
    /// An environment-variable directive.
    EnvVar,
    /// An `exec*` directive.
    Exec,
    /// A bezier curve definition.
    Bezier,
    /// An animation directive.
    Animation,
    /// A submap marker.
    Submap,
    /// A hyprlang `$variable`.
    Variable,
}

impl ValueType {
    /// Whether this type backs a single keyed scalar [`Value`].
    #[must_use]
    pub fn is_scalar(&self) -> bool {
        matches!(
            self,
            ValueType::Bool
                | ValueType::Int
                | ValueType::Float
                | ValueType::Color
                | ValueType::Gradient
                | ValueType::String
                | ValueType::Enum(_)
                | ValueType::Vec2
        )
    }

    /// Whether this type backs an ordered structured collection.
    #[must_use]
    pub fn is_structured(&self) -> bool {
        !self.is_scalar()
    }

    /// The enum variants, if this is [`ValueType::Enum`].
    #[must_use]
    pub fn enum_variants(&self) -> Option<&[EnumVariant]> {
        match self {
            ValueType::Enum(variants) => Some(variants),
            _ => None,
        }
    }
}

/// One choice of an enumerated option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    /// The literal value written to disk (e.g. `dwindle`, `0`, `slave`).
    pub name: String,
    /// Optional human-friendly explanation of what the variant does.
    pub description: Option<String>,
}

impl EnumVariant {
    /// A variant with no description.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
        }
    }

    /// A variant with a description.
    #[must_use]
    pub fn described(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: Some(description.into()),
        }
    }
}

/// An optional numeric constraint for [`ValueType::Int`] / [`ValueType::Float`].
///
/// `step` is advisory metadata for UI spinners and is **not** enforced during
/// validation (enforcing exact float divisibility is brittle).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericRange {
    /// Inclusive lower bound, if any.
    pub min: Option<f64>,
    /// Inclusive upper bound, if any.
    pub max: Option<f64>,
    /// Suggested increment, if any.
    pub step: Option<f64>,
}

impl NumericRange {
    /// A range with all bounds explicitly specified.
    #[must_use]
    pub const fn new(min: Option<f64>, max: Option<f64>, step: Option<f64>) -> Self {
        Self { min, max, step }
    }

    /// `value >= min`, no upper bound.
    #[must_use]
    pub const fn at_least(min: f64) -> Self {
        Self {
            min: Some(min),
            max: None,
            step: None,
        }
    }

    /// `min <= value <= max`.
    #[must_use]
    pub const fn bounded(min: f64, max: f64) -> Self {
        Self {
            min: Some(min),
            max: Some(max),
            step: None,
        }
    }
}

/// A complete description of a single scalar configuration option.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionSpec {
    /// The dotted option path in `conf` notation, e.g. `decoration:blur:size`.
    pub path: String,
    /// A short human label for UIs.
    pub label: String,
    /// A longer human description.
    pub description: String,
    /// The option's value type (always a scalar kind).
    pub value_type: ValueType,
    /// The Hyprland default value.
    pub default: Value,
    /// An optional numeric range (for [`ValueType::Int`]/[`ValueType::Float`]).
    pub range: Option<NumericRange>,
    /// The Hyprland version that introduced the option, if known.
    pub since: Option<String>,
}

impl OptionSpec {
    /// Builder helper: attach a `since` version hint.
    #[must_use]
    pub fn since(mut self, version: impl Into<String>) -> Self {
        self.since = Some(version.into());
        self
    }

    /// The enum variants, if this option is enumerated.
    #[must_use]
    pub fn enum_variants(&self) -> Option<&[EnumVariant]> {
        self.value_type.enum_variants()
    }

    /// Validate `value` against this option's type and range.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Validation`] if the value's kind does not match the
    /// declared [`ValueType`], if a numeric value falls outside its
    /// [`NumericRange`], or if an enum value is not one of the variants.
    pub fn validate(&self, value: &Value) -> Result<(), CoreError> {
        let mismatch = || {
            Err(CoreError::Validation {
                path: self.path.clone(),
                reason: format!("expected {:?}, got {}", self.value_type, value.type_name()),
            })
        };

        match (&self.value_type, value) {
            (ValueType::Bool, Value::Bool(_)) => Ok(()),
            (ValueType::Int, Value::Int(i)) => self.check_range(*i as f64),
            (ValueType::Float, Value::Float(x)) => self.check_range(*x),
            (ValueType::Color, Value::Color(_)) => Ok(()),
            (ValueType::Gradient, Value::Gradient(_)) => Ok(()),
            (ValueType::String, Value::String(_)) => Ok(()),
            (ValueType::Vec2, Value::Vec2(_)) => Ok(()),
            (ValueType::Enum(variants), Value::Enum(name)) => {
                if variants.iter().any(|v| &v.name == name) {
                    Ok(())
                } else {
                    Err(CoreError::Validation {
                        path: self.path.clone(),
                        reason: format!("`{name}` is not a valid variant"),
                    })
                }
            }
            _ => mismatch(),
        }
    }

    /// Validate this option's own default value.
    ///
    /// # Errors
    ///
    /// See [`OptionSpec::validate`].
    pub fn validate_default(&self) -> Result<(), CoreError> {
        self.validate(&self.default)
    }

    fn check_range(&self, x: f64) -> Result<(), CoreError> {
        if let Some(range) = &self.range {
            if let Some(min) = range.min {
                if x < min {
                    return Err(CoreError::Validation {
                        path: self.path.clone(),
                        reason: format!("{x} is below the minimum {min}"),
                    });
                }
            }
            if let Some(max) = range.max {
                if x > max {
                    return Err(CoreError::Validation {
                        path: self.path.clone(),
                        reason: format!("{x} is above the maximum {max}"),
                    });
                }
            }
        }
        Ok(())
    }
}

/// A named group of related scalar options (e.g. `general`, `decoration`).
///
/// Sub-sections such as `blur` or `groupbar` are *not* separate [`Section`]s;
/// they are reflected as `:`-nested prefixes in member [`OptionSpec::path`]s.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// The top-level section identifier, e.g. `decoration`.
    pub id: String,
    /// A short human label.
    pub label: String,
    /// A longer human description.
    pub description: String,
    /// The options in this section, in display order.
    pub options: Vec<OptionSpec>,
}

/// Identifies one of the structured, ordered collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CollectionId {
    /// `monitor = ...`
    Monitors,
    /// `workspace = ...`
    Workspaces,
    /// `windowrule` / `windowrulev2 = ...`
    WindowRules,
    /// `layerrule = ...`
    LayerRules,
    /// `bind* = ...`
    Keybinds,
    /// `submap = ...`
    Submaps,
    /// `env = ...`
    Env,
    /// `exec*` = ...`
    Execs,
    /// `$variable = ...`
    Variables,
    /// `bezier = ...`
    Beziers,
    /// `animation = ...`
    Animations,
}

/// Describes one structured collection (its element type and source keywords).
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionSpec {
    /// Stable identifier for the collection.
    pub id: CollectionId,
    /// A short human label.
    pub label: String,
    /// A longer human description.
    pub description: String,
    /// The structured [`ValueType`] of each element.
    pub element_type: ValueType,
    /// The on-disk directive keyword(s) that populate this collection.
    pub keywords: Vec<String>,
    /// The Hyprland version that introduced the collection, if relevant.
    pub since: Option<String>,
}

/// The complete Hyprland configuration schema.
///
/// `index` maps each option's dotted path to its `(section, option)` position
/// so [`Schema::option`] is an O(1) lookup rather than a linear scan — it is
/// derived from `sections` and is therefore not part of the public API.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    sections: Vec<Section>,
    collections: Vec<CollectionSpec>,
    /// Path → `(section_index, option_index)` for O(1) [`Schema::option`].
    index: HashMap<String, (usize, usize)>,
}

impl Schema {
    /// Build the full embedded schema.
    ///
    /// This is infallible: the data is `const`-built and type-checked at compile
    /// time. Structural invariants (no duplicate paths, valid defaults, ...) are
    /// asserted by [`Schema::validate`] and this crate's tests.
    #[must_use]
    pub fn load() -> Schema {
        data::build()
    }

    /// Assemble a schema from its parts, building the path lookup index. Used by
    /// the `data` builder so the index is always in sync with `sections`.
    pub(super) fn from_parts(sections: Vec<Section>, collections: Vec<CollectionSpec>) -> Schema {
        let index = sections
            .iter()
            .enumerate()
            .flat_map(|(si, section)| {
                section
                    .options
                    .iter()
                    .enumerate()
                    .map(move |(oi, opt)| (opt.path.clone(), (si, oi)))
            })
            .collect();
        Schema {
            sections,
            collections,
            index,
        }
    }

    /// A process-wide cached schema, parsed once on first use.
    #[must_use]
    pub fn shared() -> &'static Schema {
        static SCHEMA: OnceLock<Schema> = OnceLock::new();
        SCHEMA.get_or_init(Schema::load)
    }

    /// All sections, in display order.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// All structured collections, in display order.
    #[must_use]
    pub fn collections(&self) -> &[CollectionSpec] {
        &self.collections
    }

    /// Look up a section by its identifier.
    #[must_use]
    pub fn section(&self, id: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.id == id)
    }

    /// Look up a collection by its identifier.
    #[must_use]
    pub fn collection(&self, id: CollectionId) -> Option<&CollectionSpec> {
        self.collections.iter().find(|c| c.id == id)
    }

    /// Iterate over every option in every section, in order.
    pub fn options(&self) -> impl Iterator<Item = &OptionSpec> {
        self.sections.iter().flat_map(|s| s.options.iter())
    }

    /// Look up an option by its dotted path. O(1) via the prebuilt index.
    #[must_use]
    pub fn option(&self, path: &str) -> Option<&OptionSpec> {
        let &(si, oi) = self.index.get(path)?;
        self.sections.get(si)?.options.get(oi)
    }

    /// The total number of scalar options across all sections.
    #[must_use]
    pub fn option_count(&self) -> usize {
        self.sections.iter().map(|s| s.options.len()).sum()
    }

    /// Check the schema's internal consistency.
    ///
    /// Verifies that option paths are unique, every option has a scalar value
    /// type whose default validates, every enum has at least one variant, and
    /// every collection has a structured element type.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Schema`] or [`CoreError::Validation`] describing the
    /// first inconsistency found.
    pub fn validate(&self) -> Result<(), CoreError> {
        let mut seen = std::collections::HashSet::new();
        for section in &self.sections {
            for opt in &section.options {
                if !seen.insert(opt.path.as_str()) {
                    return Err(CoreError::Schema(format!(
                        "duplicate option path `{}`",
                        opt.path
                    )));
                }
                if !opt.value_type.is_scalar() {
                    return Err(CoreError::Schema(format!(
                        "option `{}` has a non-scalar value type",
                        opt.path
                    )));
                }
                if let ValueType::Enum(variants) = &opt.value_type {
                    if variants.is_empty() {
                        return Err(CoreError::Schema(format!(
                            "enum option `{}` has no variants",
                            opt.path
                        )));
                    }
                }
                opt.validate_default()?;
            }
        }

        for collection in &self.collections {
            if !collection.element_type.is_structured() {
                return Err(CoreError::Schema(format!(
                    "collection {:?} has a non-structured element type",
                    collection.id
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sections the schema must contain at minimum (per the project spec).
    const REQUIRED_SECTIONS: &[&str] = &[
        "general",
        "decoration",
        "animations",
        "input",
        "gestures",
        "group",
        "misc",
        "binds",
        "dwindle",
        "master",
        "xwayland",
        "cursor",
        "render",
        "debug",
    ];

    const REQUIRED_COLLECTIONS: &[CollectionId] = &[
        CollectionId::Monitors,
        CollectionId::Workspaces,
        CollectionId::WindowRules,
        CollectionId::LayerRules,
        CollectionId::Keybinds,
        CollectionId::Submaps,
        CollectionId::Env,
        CollectionId::Execs,
        CollectionId::Variables,
        CollectionId::Beziers,
        CollectionId::Animations,
    ];

    #[test]
    fn load_returns_a_non_trivial_schema() {
        let schema = Schema::load();
        assert!(
            schema.option_count() >= 100,
            "expected a substantial option set"
        );
        assert!(!schema.collections().is_empty());
    }

    #[test]
    fn all_required_sections_present() {
        let schema = Schema::load();
        for id in REQUIRED_SECTIONS {
            let section = schema.section(id);
            assert!(section.is_some(), "missing section `{id}`");
            assert!(
                !section.unwrap().options.is_empty(),
                "section `{id}` has no options"
            );
        }
    }

    #[test]
    fn all_required_collections_present() {
        let schema = Schema::load();
        for id in REQUIRED_COLLECTIONS {
            assert!(
                schema.collection(*id).is_some(),
                "missing collection {id:?}"
            );
        }
    }

    #[test]
    fn schema_is_internally_consistent() {
        // Covers: no duplicate paths, scalar option types, defaults valid,
        // enums non-empty, structured collection element types.
        Schema::load()
            .validate()
            .expect("embedded schema must be valid");
    }

    #[test]
    fn no_duplicate_option_paths() {
        let schema = Schema::load();
        let mut seen = std::collections::HashSet::new();
        for opt in schema.options() {
            assert!(
                seen.insert(opt.path.clone()),
                "duplicate path `{}`",
                opt.path
            );
        }
    }

    #[test]
    fn every_default_validates_against_its_spec() {
        let schema = Schema::load();
        for opt in schema.options() {
            opt.validate_default()
                .unwrap_or_else(|e| panic!("default for `{}` is invalid: {e}", opt.path));
        }
    }

    #[test]
    fn every_enum_has_at_least_one_variant() {
        let schema = Schema::load();
        for opt in schema.options() {
            if let ValueType::Enum(variants) = &opt.value_type {
                assert!(!variants.is_empty(), "enum `{}` has no variants", opt.path);
            }
        }
    }

    #[test]
    fn option_lookup_works() {
        let schema = Schema::load();
        assert!(schema.option("decoration:rounding").is_some());
        assert!(schema.option("general:col.active_border").is_some());
        assert!(schema.option("does:not:exist").is_none());
    }

    /// The data-driven guarantee: every option we expose must be a real
    /// Hyprland key, as enumerated by the vendored upstream stub. The mapping is
    /// purely `conf` `:` -> stub `.` (see `meta/README.md`).
    #[test]
    fn every_option_path_exists_in_vendored_stub() {
        const KEYS: &str = include_str!("../../../meta/hyprland-config-keys.txt");
        let known: std::collections::HashSet<&str> = KEYS.lines().map(str::trim).collect();

        let schema = Schema::load();
        for opt in schema.options() {
            let stub_key = opt.path.replace(':', ".");
            assert!(
                known.contains(stub_key.as_str()),
                "option `{}` (stub key `{stub_key}`) is not in meta/hyprland-config-keys.txt",
                opt.path
            );
        }
    }

    /// Choice-typed options must expose their full list of variants (so the GUI
    /// renders dropdowns, not free text). Variant sets are pinned against the
    /// Hyprland 0.55.2 `src/config/values/ConfigValues.cpp` option data.
    #[test]
    fn choice_options_are_enums_with_expected_variants() {
        let schema = Schema::load();
        let variants = |path: &str| -> Vec<String> {
            schema
                .option(path)
                .unwrap_or_else(|| panic!("missing option `{path}`"))
                .enum_variants()
                .unwrap_or_else(|| panic!("`{path}` is not an enum"))
                .iter()
                .map(|v| v.name.clone())
                .collect()
        };

        // Integer "mode" options expose their integer literals as variants.
        assert_eq!(
            variants("input:follow_mouse"),
            ["0", "1", "2", "3"].map(str::to_string)
        );
        assert_eq!(
            variants("misc:vrr"),
            ["0", "1", "2", "3"].map(str::to_string)
        );
        assert_eq!(
            variants("misc:force_default_wallpaper"),
            ["-1", "0", "1", "2"].map(str::to_string)
        );
        assert_eq!(
            variants("dwindle:force_split"),
            ["0", "1", "2"].map(str::to_string)
        );
        assert_eq!(
            variants("cursor:no_hardware_cursors"),
            ["0", "1", "2"].map(str::to_string)
        );
        assert_eq!(
            variants("general:resize_corner"),
            ["0", "1", "2", "3", "4"].map(str::to_string)
        );

        // String-choice options expose their textual variants (incl. unset "").
        assert_eq!(
            variants("input:accel_profile"),
            ["", "adaptive", "flat", "custom"].map(str::to_string)
        );
        assert_eq!(
            variants("input:scroll_method"),
            ["", "2fg", "edge", "on_button_down", "no_scroll"].map(str::to_string)
        );

        // Every enum variant carries a description (the "meaning" of the value),
        // and every enum's default is one of its own variants.
        for opt in schema.options() {
            if let ValueType::Enum(vs) = &opt.value_type {
                assert!(
                    vs.iter().all(|v| v.description.is_some()),
                    "enum `{}` has a variant without a description",
                    opt.path
                );
                opt.validate_default()
                    .unwrap_or_else(|e| panic!("enum default invalid for `{}`: {e}", opt.path));
            }
        }
    }
}
