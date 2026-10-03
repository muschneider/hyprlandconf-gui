// SPDX-License-Identifier: MIT OR Apache-2.0
//! Locating and loading the user's Hyprland configuration.
//!
//! Detection order (when no explicit path is given): `hyprland.lua` then
//! `hyprland.conf` under `$XDG_CONFIG_HOME/hypr` (falling back to
//! `~/.config/hypr`). Parsing goes through `hyprconf-core` and follows includes.
//! This runs off the UI thread inside an `iced::Task`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hyprconf_core::{
    conf, lua, ConfBundle, ConfWarning, Config, ConfigFormat, LuaWarning, Schema, Severity, Value,
};

use crate::edit::FieldId;

/// The originally-parsed source, retained so a same-format save can preserve
/// comments/structure and write back only the files that changed.
#[derive(Debug, Clone)]
pub enum Origin {
    /// Parsed from `.conf` file(s); the bundle backs preserve-mode saves.
    Conf(ConfBundle),
    /// Parsed from `.lua` file(s). Lua always regenerates on save, so the bundle
    /// is not retained.
    Lua,
}

/// A human-friendly issue surfaced while reading the configuration.
///
/// Parsing never fails on these — the offending text is always preserved on
/// disk — but the user should know about them, so each carries a plain-language
/// summary, where it occurred, and a hint on what (if anything) to do.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// How serious it is (load-time issues are always [`Severity::Warning`]).
    pub severity: Severity,
    /// A one-line, plain-language description of what happened.
    pub message: String,
    /// Where in the source it occurred (`file:line`), when known.
    pub location: Option<String>,
    /// A short suggestion for how to resolve or interpret it.
    pub hint: Option<String>,
}

/// The result of attempting to load a configuration.
#[derive(Debug, Clone)]
pub enum LoadState {
    /// A load is in progress.
    Loading,
    /// A configuration was loaded successfully.
    Loaded(Box<Loaded>),
    /// No configuration file could be found.
    NotFound {
        /// The paths that were probed.
        searched: Vec<PathBuf>,
    },
    /// A configuration file was found but failed to load.
    Error {
        /// The path that failed.
        path: PathBuf,
        /// The error message.
        message: String,
    },
}

impl LoadState {
    /// The loaded configuration, if the load succeeded.
    #[must_use]
    pub fn loaded(&self) -> Option<&Loaded> {
        match self {
            LoadState::Loaded(loaded) => Some(loaded),
            _ => None,
        }
    }
}

/// A successfully loaded configuration plus its provenance and live edit state.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The detected on-disk format.
    pub format: ConfigFormat,
    /// The (canonical) root file that was loaded.
    pub source: PathBuf,
    /// How many additional files were pulled in via includes.
    pub included_files: usize,
    /// Issues found while mapping the file(s) onto the schema (unknown options,
    /// unparseable values, preserved dynamic regions, …). Each is non-fatal and
    /// retained in full so the UI can explain it and suggest a fix.
    pub diagnostics: Vec<Diagnostic>,
    /// The current (possibly edited) configuration.
    pub config: Config,
    /// The effective value of each schema option at load time, used to decide
    /// whether a field has unsaved changes. Shared (`Arc`) so undo snapshots can
    /// reference it cheaply instead of deep-copying it on every edit.
    pub baseline: Arc<HashMap<String, Value>>,
    /// Paths whose current value differs from [`Loaded::baseline`].
    pub dirty: HashSet<String>,
    /// In-progress raw text for text-based editors, keyed by field.
    pub drafts: HashMap<FieldId, String>,
    /// Per-field validation errors (invalid/out-of-range input).
    pub errors: HashMap<FieldId, String>,
    /// Structured collections that have been edited (add/remove/reorder/field).
    pub touched: HashSet<hyprconf_core::schema::CollectionId>,
    /// The originally-parsed source (for preserve-mode/multi-file saves).
    pub origin: Origin,
    /// Number of dynamic Lua regions that fresh serialization would drop.
    pub dynamic_regions: usize,
}

impl Loaded {
    /// Build a [`Loaded`], snapshotting the baseline value of every schema
    /// option (its file value, or its schema default if absent).
    #[allow(clippy::too_many_arguments)]
    fn new(
        format: ConfigFormat,
        source: PathBuf,
        included_files: usize,
        config: Config,
        diagnostics: Vec<Diagnostic>,
        dynamic_regions: usize,
        origin: Origin,
        schema: &Schema,
    ) -> Self {
        let baseline: HashMap<String, Value> = schema
            .options()
            .map(|opt| {
                let value = config
                    .get(&opt.path)
                    .cloned()
                    .unwrap_or_else(|| opt.default.clone());
                (opt.path.clone(), value)
            })
            .collect();

        Self {
            format,
            source,
            included_files,
            diagnostics,
            config,
            baseline: Arc::new(baseline),
            dirty: HashSet::new(),
            drafts: HashMap::new(),
            errors: HashMap::new(),
            touched: HashSet::new(),
            origin,
            dynamic_regions,
        }
    }

    /// Whether the config was loaded from more than one file.
    #[must_use]
    pub fn is_multi_file(&self) -> bool {
        self.included_files > 0
    }
}

/// Load a configuration, optionally from an explicit path.
///
/// This is synchronous; the GUI runs it inside a [`iced::Task`] so the window
/// stays responsive.
#[must_use]
pub fn load_config(explicit: Option<PathBuf>) -> LoadState {
    let schema = Schema::shared();

    if let Some(path) = explicit {
        if !path.exists() {
            return LoadState::Error {
                path: path.clone(),
                message: "file does not exist".to_string(),
            };
        }
        return load_path(&path, format_from_ext(&path), schema);
    }

    let dir = hypr_config_dir();
    let lua = dir.join("hyprland.lua");
    let conf = dir.join("hyprland.conf");

    if lua.exists() {
        load_path(&lua, ConfigFormat::Lua, schema)
    } else if conf.exists() {
        load_path(&conf, ConfigFormat::Conf, schema)
    } else {
        LoadState::NotFound {
            searched: vec![lua, conf],
        }
    }
}

fn load_path(path: &Path, format: ConfigFormat, schema: &Schema) -> LoadState {
    match format {
        ConfigFormat::Lua => match lua::LuaParser::parse_file(path) {
            Ok(bundle) => {
                let (config, warnings) = lua::bundle_to_config(&bundle, schema);
                let source = bundle
                    .root()
                    .path
                    .clone()
                    .unwrap_or_else(|| path.to_path_buf());
                let included = bundle.documents.len().saturating_sub(1);
                let dynamic = warnings
                    .iter()
                    .filter(|w| matches!(w, LuaWarning::DynamicRegion { .. }))
                    .count();
                let diagnostics = warnings.iter().map(lua_diagnostic).collect();
                LoadState::Loaded(Box::new(Loaded::new(
                    format,
                    source,
                    included,
                    config,
                    diagnostics,
                    dynamic,
                    Origin::Lua,
                    schema,
                )))
            }
            Err(e) => LoadState::Error {
                path: path.to_path_buf(),
                message: e.to_string(),
            },
        },
        ConfigFormat::Conf => match conf::ConfParser::parse_file(path) {
            Ok(bundle) => {
                let (config, warnings) = conf::bundle_to_config(&bundle, schema);
                let source = bundle
                    .root()
                    .path
                    .clone()
                    .unwrap_or_else(|| path.to_path_buf());
                let included = bundle.documents.len().saturating_sub(1);
                let diagnostics = warnings.iter().map(conf_diagnostic).collect();
                LoadState::Loaded(Box::new(Loaded::new(
                    format,
                    source,
                    included,
                    config,
                    diagnostics,
                    0,
                    Origin::Conf(bundle),
                    schema,
                )))
            }
            Err(e) => LoadState::Error {
                path: path.to_path_buf(),
                message: e.to_string(),
            },
        },
    }
}

/// Turn a `.conf` mapping warning into a user-facing [`Diagnostic`].
fn conf_diagnostic(warning: &ConfWarning) -> Diagnostic {
    match warning {
        ConfWarning::UnknownOption { path, file, line } => Diagnostic {
            severity: Severity::Warning,
            message: format!("“{path}” isn’t a recognized Hyprland option"),
            location: location(file.as_deref(), *line),
            hint: Some(
                "Kept exactly as written. Check for a typo, or it may belong to a plugin."
                    .to_string(),
            ),
        },
        ConfWarning::UnparsableValue {
            path,
            value,
            reason,
            file,
            line,
        } => Diagnostic {
            severity: Severity::Warning,
            message: format!("Couldn’t read the value for “{path}”: “{value}”"),
            location: location(file.as_deref(), *line),
            hint: Some(format!("{reason}. The original text is preserved as-is.")),
        },
        ConfWarning::UnparsableDirective {
            keyword,
            args,
            reason,
            file,
            line,
        } => Diagnostic {
            severity: Severity::Warning,
            message: format!("Couldn’t parse this {keyword} entry: “{args}”"),
            location: location(file.as_deref(), *line),
            hint: Some(format!("{reason}. It’s preserved untouched.")),
        },
        // `ConfWarning` is `#[non_exhaustive]`; fall back to its own message.
        other => Diagnostic {
            severity: Severity::Warning,
            message: other.to_string(),
            location: None,
            hint: None,
        },
    }
}

/// Turn a Lua mapping warning into a user-facing [`Diagnostic`].
fn lua_diagnostic(warning: &LuaWarning) -> Diagnostic {
    match warning {
        LuaWarning::DynamicRegion { snippet } => Diagnostic {
            severity: Severity::Warning,
            message: format!("Dynamic Lua kept read-only: {}", truncate(snippet)),
            location: None,
            hint: Some(
                "This is code (a loop, function, …) rather than a setting, so it can’t be \
                 edited here — it’s preserved untouched. Converting to conf would drop it."
                    .to_string(),
            ),
        },
        LuaWarning::UnknownOption { path } => Diagnostic {
            severity: Severity::Warning,
            message: format!("“{path}” isn’t a recognized Hyprland option"),
            location: None,
            hint: Some("Kept as-is. Check for a typo, or it may belong to a plugin.".to_string()),
        },
        LuaWarning::UnparsableValue {
            path,
            value,
            reason,
        } => Diagnostic {
            severity: Severity::Warning,
            message: format!("Couldn’t read the value for “{path}”: “{value}”"),
            location: None,
            hint: Some(format!("{reason}. The original is preserved.")),
        },
        LuaWarning::UnparsableCall { callee, reason } => Diagnostic {
            severity: Severity::Warning,
            message: format!("Couldn’t interpret a {callee}(…) call"),
            location: None,
            hint: Some(format!("{reason}. It’s preserved untouched.")),
        },
        LuaWarning::UnresolvedRequire { module } => Diagnostic {
            severity: Severity::Warning,
            message: format!("Couldn’t resolve require(\"{module}\")"),
            location: None,
            hint: Some(
                "The file wasn’t found or couldn’t be loaded, so its settings aren’t shown."
                    .to_string(),
            ),
        },
        // `LuaWarning` is `#[non_exhaustive]`; fall back to its own message.
        other => Diagnostic {
            severity: Severity::Warning,
            message: other.to_string(),
            location: None,
            hint: None,
        },
    }
}

/// Format an optional `file:line` source location.
fn location(file: Option<&Path>, line: Option<u32>) -> Option<String> {
    match (file, line) {
        (Some(p), Some(l)) => Some(format!("{}:{l}", p.display())),
        (Some(p), None) => Some(p.display().to_string()),
        (None, Some(l)) => Some(format!("line {l}")),
        (None, None) => None,
    }
}

/// Collapse whitespace and clip a snippet so it fits on one tidy line.
fn truncate(snippet: &str) -> String {
    let one_line = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 80 {
        let mut clipped: String = one_line.chars().take(79).collect();
        clipped.push('…');
        clipped
    } else {
        one_line
    }
}

/// Guess the format from a path's extension, defaulting to `conf`.
fn format_from_ext(path: &Path) -> ConfigFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some("lua") => ConfigFormat::Lua,
        _ => ConfigFormat::Conf,
    }
}

/// `$XDG_CONFIG_HOME/hypr` (or `~/.config/hypr`).
fn hypr_config_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.config_dir().join("hypr"))
        .unwrap_or_else(|| PathBuf::from("~/.config/hypr"))
}

/// A short human label for a format, for the status bar.
#[must_use]
pub fn format_label(format: ConfigFormat) -> &'static str {
    match format {
        ConfigFormat::Lua => "Lua",
        ConfigFormat::Conf => "conf",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyprconf_core::Value;

    /// Create a unique temp directory for a test, returning its path.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hyprconf-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_explicit_conf() {
        let dir = temp_dir("conf");
        let path = dir.join("hyprland.conf");
        std::fs::write(&path, "general {\n    gaps_in = 7\n}\n").unwrap();

        match load_config(Some(path)) {
            LoadState::Loaded(loaded) => {
                assert_eq!(loaded.format, ConfigFormat::Conf);
                assert_eq!(
                    loaded.config.get("general:gaps_in"),
                    Some(&Value::CssGap(hyprconf_core::value::CssGap::uniform(7)))
                );
            }
            other => panic!("expected Loaded, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_explicit_lua() {
        let dir = temp_dir("lua");
        let path = dir.join("hyprland.lua");
        std::fs::write(&path, "hl.config({ general = { gaps_in = 9 } })\n").unwrap();

        match load_config(Some(path)) {
            LoadState::Loaded(loaded) => {
                assert_eq!(loaded.format, ConfigFormat::Lua);
                assert_eq!(
                    loaded.config.get("general:gaps_in"),
                    Some(&Value::CssGap(hyprconf_core::value::CssGap::uniform(9)))
                );
            }
            other => panic!("expected Loaded, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_explicit_path_is_error() {
        match load_config(Some(PathBuf::from("/nonexistent/hyprconf-xyz.conf"))) {
            LoadState::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn format_from_ext_defaults_to_conf() {
        assert_eq!(format_from_ext(Path::new("a.lua")), ConfigFormat::Lua);
        assert_eq!(format_from_ext(Path::new("a.conf")), ConfigFormat::Conf);
        assert_eq!(format_from_ext(Path::new("a")), ConfigFormat::Conf);
    }
}
