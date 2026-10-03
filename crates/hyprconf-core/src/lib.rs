// SPDX-License-Identifier: MIT OR Apache-2.0
//! `hyprconf-core` is the UI-free heart of hyprconf.
//!
//! It holds the format-agnostic configuration model and the data-driven
//! Hyprland option [`schema`]. Future steps will add the Lua and `conf`
//! parsers/serializers and validation on top of these foundations. Keeping this
//! crate free of any GUI dependency is what makes the interesting logic
//! unit-testable in isolation.
//!
//! ## Layout
//!
//! - [`value`] — scalar values ([`Color`], [`Gradient`], [`Vec2`], [`Value`])
//!   and their Hyprland text representations.
//! - [`structured`] — the repeatable, ordered config constructs (keybinds,
//!   window rules, monitors, ...).
//! - [`schema`] — the data-driven description of Hyprland's option surface,
//!   verified against the vendored stub in `meta/`.
//! - [`model`] — the in-memory [`Config`], independent of any on-disk format,
//!   carrying provenance for comment-preserving round-trips.

pub mod conf;
pub mod fs;
pub mod hyprctl;
pub mod lua;
pub mod model;
pub mod outputs;
pub mod schema;
pub mod structured;
pub mod validate;
pub mod value;
pub mod verify;

pub use conf::{ConfBundle, ConfDocument, ConfError, ConfParser, ConfSerializer, ConfWarning};
pub use fs::{atomic_write, backup_existing, save_atomically, FsError, SaveReport};
pub use hyprctl::{HyprctlError, HyprlandInfo};
pub use lua::{
    LuaBundle, LuaDocument, LuaError, LuaNote, LuaOutput, LuaParser, LuaSerializer, LuaWarning,
};
pub use model::{Config, ConfigFormat, Provenance, Span, Tracked};
pub use outputs::{DetectedMonitor, Mode};
pub use schema::{
    CollectionId, CollectionSpec, EnumVariant, NumericRange, OptionSpec, Schema, Section, ValueType,
};
pub use structured::{
    Animation, Bezier, EnvVar, Exec, ExecKind, Keybind, KeybindFlags, LayerRule, MonitorRule,
    StructuredValue, Submap, Variable, WindowRule, WorkspaceRule,
};
pub use validate::{has_errors, unsupported_options, validate_config, ConfigProblem, Severity};
pub use value::{Color, Gradient, Value, ValueParseError, Vec2};
pub use verify::{verify_file, verify_text, Verdict, VerifyIssue};

/// The error type returned by fallible `hyprconf-core` operations.
///
/// It is marked `#[non_exhaustive]` because future steps will add variants
/// (parse errors for whole files, atomic-write failures, ...) and downstream
/// code must not assume the set is closed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// An underlying filesystem or I/O error.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A feature that is declared but not yet implemented in this step.
    #[error("not yet implemented: {0}")]
    Unimplemented(&'static str),

    /// The embedded schema is internally inconsistent (a build-time invariant
    /// guarded by this crate's unit tests).
    #[error("schema error: {0}")]
    Schema(String),

    /// A value did not satisfy the constraints of its [`OptionSpec`].
    #[error("validation error at `{path}`: {reason}")]
    Validation {
        /// The dotted option path that failed validation.
        path: String,
        /// A human-readable explanation of the failure.
        reason: String,
    },

    /// A scalar value could not be parsed from its Hyprland textual form.
    #[error(transparent)]
    Value(#[from] value::ValueParseError),

    /// An error while reading, parsing or following includes in a `.conf` file.
    #[error(transparent)]
    Conf(#[from] conf::ConfError),

    /// An error while reading, parsing or following includes in a `.lua` file.
    #[error(transparent)]
    Lua(#[from] lua::LuaError),

    /// A filesystem error while writing a config atomically.
    #[error(transparent)]
    Fs(#[from] fs::FsError),
}

/// A convenience [`Result`] alias whose error type is [`CoreError`].
pub type Result<T> = std::result::Result<T, CoreError>;

/// Returns the semantic version of `hyprconf-core`.
///
/// This mirrors the workspace version and is primarily useful for diagnostics,
/// `--version` output, and compatibility checks between the core and any GUI/CLI
/// front-end built on top of it.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty(), "version() must not be empty");
    }

    #[test]
    fn version_matches_cargo_pkg_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn unimplemented_error_displays_context() {
        let err = CoreError::Unimplemented("lua parser");
        assert_eq!(err.to_string(), "not yet implemented: lua parser");
    }

    #[test]
    fn io_error_is_convertible() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: CoreError = io.into();
        assert!(matches!(err, CoreError::Io(_)));
        assert!(err.to_string().starts_with("i/o error"));
    }

    #[test]
    fn value_parse_error_is_convertible() {
        let pe = ValueParseError::Vec2 {
            input: "nope".to_string(),
            reason: "expected two numbers",
        };
        let err: CoreError = pe.into();
        assert!(matches!(err, CoreError::Value(_)));
    }
}
