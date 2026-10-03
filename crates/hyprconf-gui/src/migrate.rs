// SPDX-License-Identifier: MIT OR Apache-2.0
//! The guided `.conf` -> Lua migration flow.
//!
//! # Why this is a wizard and not a button
//!
//! Hyprland 0.56 prints:
//!
//! > You are using the .conf config format, support for which will be removed
//! > in Hyprland 0.57.
//!
//! so every `.conf` user has to migrate, and most will do it exactly once, under
//! mild time pressure, to the single file that determines whether their desktop
//! still works after a reboot. The risky part is not the file write — it is not
//! knowing whether the result is *correct*.
//!
//! The flow is therefore built around answering that question before anything is
//! written:
//!
//! 1. **Review** — what was found, and what could not be translated exactly.
//! 2. **Preview** — the actual Lua, in full.
//! 3. **Check** — hand the generated file to the real `Hyprland --verify-config`
//!    and show its verdict verbatim.
//! 4. **Apply** — back up the `.conf`, write `hyprland.lua`, and say what to do
//!    next.
//!
//! Step 3 is the reason to trust the result: it is the compositor itself
//! confirming the file loads, not this program marking its own homework.

use std::path::{Path, PathBuf};

use hyprconf_core::lua::{LuaNote, LuaSerializer};
use hyprconf_core::verify::Verdict;
use hyprconf_core::{Config, ConfigFormat};

use crate::load::Loaded;

/// Which step of the flow is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Summary of what will be converted, plus anything needing attention.
    Review,
    /// The generated Lua in full.
    Preview,
    /// The verdict from `Hyprland --verify-config`.
    Check,
    /// Post-write confirmation and next steps.
    Done,
}

impl Step {
    /// The steps in order, for the progress indicator.
    pub const ALL: [Step; 4] = [Step::Review, Step::Preview, Step::Check, Step::Done];

    /// Short title.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Step::Review => "Review",
            Step::Preview => "Preview",
            Step::Check => "Check",
            Step::Done => "Apply",
        }
    }

    /// The 1-based position, for "step 2 of 4".
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0) + 1
    }
}

/// What the config contains, for the review summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Scalar settings.
    pub options: usize,
    /// Keybinds.
    pub keybinds: usize,
    /// Window rules.
    pub window_rules: usize,
    /// Layer rules.
    pub layer_rules: usize,
    /// Monitors.
    pub monitors: usize,
    /// Workspace rules.
    pub workspaces: usize,
    /// Environment variables.
    pub env: usize,
    /// Autostart commands.
    pub execs: usize,
    /// Bezier curves.
    pub beziers: usize,
    /// Animations.
    pub animations: usize,
}

impl Summary {
    fn of(config: &Config) -> Self {
        Self {
            options: config.options.len(),
            keybinds: config.keybinds.len(),
            window_rules: config.window_rules.len(),
            layer_rules: config.layer_rules.len(),
            monitors: config.monitors.len(),
            workspaces: config.workspaces.len(),
            env: config.env.len(),
            execs: config.execs.len(),
            beziers: config.beziers.len(),
            animations: config.animations.len(),
        }
    }

    /// Non-zero entries as `(label, count)`, for rendering.
    #[must_use]
    pub fn rows(&self) -> Vec<(&'static str, usize)> {
        [
            ("Settings", self.options),
            ("Keybinds", self.keybinds),
            ("Window rules", self.window_rules),
            ("Layer rules", self.layer_rules),
            ("Monitors", self.monitors),
            ("Workspace rules", self.workspaces),
            ("Environment variables", self.env),
            ("Autostart commands", self.execs),
            ("Bezier curves", self.beziers),
            ("Animations", self.animations),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .collect()
    }
}

/// An in-progress migration.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Current step.
    pub step: Step,
    /// The `.conf` being migrated.
    pub source: PathBuf,
    /// Where the Lua will be written.
    pub target: PathBuf,
    /// The generated Lua.
    pub lua: String,
    /// Constructs the Lua API could not express exactly.
    pub notes: Vec<LuaNote>,
    /// What the config contains.
    pub summary: Summary,
    /// Dynamic Lua regions that would be lost (only when migrating *from* Lua).
    pub drops_dynamic: usize,
    /// The verdict from Hyprland, once checked.
    pub verdict: Option<Verdict>,
    /// Whether a check is currently running.
    pub checking: bool,
    /// The result of writing, once applied.
    pub outcome: Option<Result<Applied, String>>,
    /// Whether the user chose to proceed despite a failed check.
    pub override_check: bool,
}

/// What a successful migration produced.
#[derive(Debug, Clone)]
pub struct Applied {
    /// The written Lua file.
    pub written: PathBuf,
    /// The backup of the original `.conf`, if one was made.
    pub backup: Option<PathBuf>,
}

impl Migration {
    /// Start a migration from the currently loaded config.
    #[must_use]
    pub fn start(loaded: &Loaded) -> Self {
        let output = LuaSerializer::generate(&loaded.config);
        let source = loaded.source.clone();
        Self {
            step: Step::Review,
            target: lua_target(&source),
            source,
            lua: output.text,
            notes: output.notes,
            summary: Summary::of(&loaded.config),
            drops_dynamic: if loaded.format == ConfigFormat::Lua {
                loaded.dynamic_regions
            } else {
                0
            },
            verdict: None,
            checking: false,
            outcome: None,
            override_check: false,
        }
    }

    /// Notes that may change behaviour (as opposed to cosmetic ones).
    #[must_use]
    pub fn blocking_notes(&self) -> Vec<&LuaNote> {
        self.notes.iter().filter(|n| !n.lossless).collect()
    }

    /// Whether the flow may advance past the check step.
    #[must_use]
    pub fn can_apply(&self) -> bool {
        match &self.verdict {
            Some(v) if v.is_ok() => true,
            // A missing Hyprland binary must not block migration on, say, a
            // machine where the user is preparing a config for another host.
            Some(Verdict::Unavailable(_)) | None => self.override_check,
            Some(_) => self.override_check,
        }
    }

    /// A one-line description of the check result.
    #[must_use]
    pub fn check_summary(&self) -> Option<String> {
        Some(match self.verdict.as_ref()? {
            Verdict::Ok => "Hyprland loaded the generated config with no errors.".to_string(),
            Verdict::Problems(issues) => format!(
                "Hyprland reported {} problem{}.",
                issues.len(),
                if issues.len() == 1 { "" } else { "s" }
            ),
            Verdict::Unavailable(reason) => reason.clone(),
        })
    }
}

/// The Lua file a `.conf` should migrate to: same directory, `.lua` extension.
///
/// `hyprland.conf` -> `hyprland.lua`, which is what Hyprland looks for.
#[must_use]
pub fn lua_target(source: &Path) -> PathBuf {
    source.with_extension("lua")
}

/// Write the migration: back up whatever is at `target`, then write `lua`
/// atomically.
///
/// The source `.conf` is deliberately **left in place**. Hyprland prefers
/// `hyprland.lua` when both exist, so keeping the original costs nothing and
/// means undoing the migration is deleting one file.
///
/// Takes owned arguments so the GUI can run it off the UI thread in a `Task`.
///
/// # Errors
///
/// Returns a human-readable message if the backup or the write fails.
pub fn apply(target: PathBuf, lua: String) -> Result<Applied, String> {
    let backup = hyprconf_core::fs::backup_existing(&target)
        .map_err(|e| format!("could not back up {}: {e}", target.display()))?;

    hyprconf_core::fs::atomic_write(&target, &lua)
        .map_err(|e| format!("could not write {}: {e}", target.display()))?;

    Ok(Applied {
        written: target,
        backup,
    })
}

/// Whether a deprecation warning should be shown for `format`.
///
/// Hyprland 0.56 warns that `.conf` support goes away in 0.57; the banner is
/// shown for any `.conf` config so the message is not missed on older versions
/// either.
#[must_use]
pub fn should_warn(format: ConfigFormat) -> bool {
    format == ConfigFormat::Conf
}

/// The release that drops `.conf` support, as announced by Hyprland 0.56.
pub const CONF_REMOVED_IN: &str = "0.57";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_target_swaps_the_extension() {
        assert_eq!(
            lua_target(Path::new("/home/u/.config/hypr/hyprland.conf")),
            PathBuf::from("/home/u/.config/hypr/hyprland.lua")
        );
    }

    #[test]
    fn steps_report_their_position() {
        assert_eq!(Step::Review.index(), 1);
        assert_eq!(Step::Done.index(), 4);
        assert_eq!(Step::Check.title(), "Check");
    }

    #[test]
    fn summary_lists_only_non_empty_rows() {
        let s = Summary {
            options: 3,
            keybinds: 2,
            ..Summary::default()
        };
        assert_eq!(s.rows(), vec![("Settings", 3), ("Keybinds", 2)]);
    }

    #[test]
    fn only_conf_configs_get_the_deprecation_banner() {
        assert!(should_warn(ConfigFormat::Conf));
        assert!(!should_warn(ConfigFormat::Lua));
    }
}
