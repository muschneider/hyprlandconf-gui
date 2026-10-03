// SPDX-License-Identifier: MIT OR Apache-2.0
//! Thin, UI-free wrappers around the `hyprctl` CLI.
//!
//! These run the external binary with [`std::process::Command`]; the GUI invokes
//! them off the UI thread inside an `iced::Task`. Everything degrades gracefully:
//! if `hyprctl` is missing or Hyprland isn't running, calls return an error
//! instead of panicking, and [`detect`] returns `None`.

use std::process::Command;

use crate::outputs::{parse_monitors, DetectedMonitor};

/// A typed error from a `hyprctl` invocation.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum HyprctlError {
    /// `hyprctl` could not be executed (not installed / not in `PATH`).
    #[error("hyprctl is not available: {0}")]
    Unavailable(String),
    /// `hyprctl` ran but reported failure.
    #[error("hyprctl {command} failed: {message}")]
    Failed {
        /// The sub-command attempted.
        command: String,
        /// `hyprctl`'s error output.
        message: String,
    },
}

/// Information about a running Hyprland instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyprlandInfo {
    /// The semantic version string, e.g. `0.55.2`.
    pub version: String,
    /// The git tag, if reported (e.g. `v0.55.2`).
    pub tag: Option<String>,
}

/// Detect a running Hyprland by parsing `hyprctl version`.
///
/// Returns `None` if `hyprctl` cannot be run or its output is unrecognised.
#[must_use]
pub fn detect() -> Option<HyprlandInfo> {
    let output = Command::new("hyprctl").arg("version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the first line of `hyprctl version` output.
fn parse_version(text: &str) -> Option<HyprlandInfo> {
    let first = text.lines().next()?;
    // "Hyprland 0.55.2 built from branch ..."
    let version = first.split_whitespace().nth(1)?.trim().to_string();
    if version.is_empty() {
        return None;
    }
    let tag = text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Tag:")
            .map(|rest| rest.split(',').next().unwrap_or("").trim().to_string())
            .filter(|t| !t.is_empty())
    });
    Some(HyprlandInfo { version, tag })
}

/// Apply a single keyword live: `hyprctl keyword <name> <value>`.
///
/// Only works when the running Hyprland was configured with hyprlang
/// (`.conf`); prefer [`apply_option`] / [`apply_monitor`], which also handle
/// Lua sessions.
///
/// # Errors
///
/// Returns [`HyprctlError`] if `hyprctl` cannot run or reports failure —
/// including the exit-0 refusal a Lua session gives.
pub fn apply_keyword(name: &str, value: &str) -> Result<String, HyprctlError> {
    let command = format!("keyword {name} {value}");
    checked(&command, run(&["keyword", name, value]))
}

/// Evaluate Lua in the running compositor: `hyprctl eval <code>`.
///
/// # Errors
///
/// Returns [`HyprctlError`] if `hyprctl` cannot run or Hyprland reports an
/// error evaluating the code.
pub fn eval(lua: &str) -> Result<String, HyprctlError> {
    checked("eval", run(&["eval", lua]))
}

/// Apply one option live, whichever config format the session runs.
///
/// A hyprlang session takes `hyprctl keyword` (with the `.conf` spelling of
/// the key). A Lua session — the default since 0.55 — refuses `keyword`
/// outright (*"keyword can't work with non-legacy parsers. Use eval."*, with
/// exit status 0) and needs an `hl.config({ … })` call through `eval`.
///
/// # Errors
///
/// Returns [`HyprctlError`] if neither form is accepted.
pub fn apply_option(path: &str, value: &crate::Value) -> Result<String, HyprctlError> {
    let keyword = crate::schema::conf_path(path);
    let text = crate::conf::value_to_conf(value);
    match apply_keyword(&keyword, &text) {
        Err(e) if wants_eval(&e) => eval(&crate::lua::config_call([(path, value)])),
        other => other,
    }
}

/// Apply a monitor rule live, whichever config format the session runs (see
/// [`apply_option`]).
///
/// # Errors
///
/// Returns [`HyprctlError`] if neither form is accepted.
pub fn apply_monitor(rule: &crate::structured::MonitorRule) -> Result<String, HyprctlError> {
    match apply_keyword("monitor", &rule.to_keyword_value()) {
        Err(e) if wants_eval(&e) => eval(&crate::lua::monitor_call(rule)),
        other => other,
    }
}

/// Whether Hyprland refused `keyword` because the session runs a Lua config.
fn wants_eval(error: &HyprctlError) -> bool {
    matches!(error, HyprctlError::Failed { message, .. }
        if message.contains("non-legacy parser") || message.contains("Use eval"))
}

/// Turn `hyprctl`'s *textual* verdict into a result. Several commands exit 0
/// while refusing the request, so the reply text is the real signal: `ok` (or
/// nothing) is success, anything else is the error message.
fn checked(command: &str, result: Result<String, HyprctlError>) -> Result<String, HyprctlError> {
    let reply = result?;
    let trimmed = reply.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("ok") {
        Ok(reply)
    } else {
        Err(HyprctlError::Failed {
            command: command.to_string(),
            message: trimmed.to_string(),
        })
    }
}

/// Trigger a config reload: `hyprctl reload`.
///
/// # Errors
///
/// Returns [`HyprctlError`] if `hyprctl` cannot run or reports failure.
pub fn reload() -> Result<String, HyprctlError> {
    run(&["reload"])
}

/// List the attached displays: `hyprctl monitors all -j`.
///
/// `all` matters — it includes outputs the compositor has disabled, which are
/// exactly the ones a user comes to this screen to turn back on.
///
/// # Errors
///
/// Returns [`HyprctlError`] if `hyprctl` cannot run, reports failure, or prints
/// something that isn't the expected JSON array.
pub fn monitors() -> Result<Vec<DetectedMonitor>, HyprctlError> {
    let json = run(&["monitors", "all", "-j"])?;
    parse_monitors(&json).map_err(|message| HyprctlError::Failed {
        command: "monitors all -j".to_string(),
        message,
    })
}

fn run(args: &[&str]) -> Result<String, HyprctlError> {
    let output = Command::new("hyprctl")
        .args(args)
        .output()
        .map_err(|e| HyprctlError::Unavailable(e.to_string()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        // `hyprctl` prints most errors on stdout (`eval` exits 7 with
        // "error: …" there and nothing on stderr).
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(HyprctlError::Failed {
            command: args.first().copied().unwrap_or_default().to_string(),
            message: if stderr.is_empty() { stdout } else { stderr },
        })
    }
}

/// Parse a Hyprland version string `[v]MAJOR.MINOR.PATCH` into a tuple.
#[must_use]
pub fn parse_semver(version: &str) -> Option<(u32, u32, u32)> {
    let core = version.trim().trim_start_matches('v');
    // Stop at the first non-version character (e.g. `-dirty`).
    let core: String = core
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Whether `required` is strictly newer than `running` (both `MAJOR.MINOR.PATCH`).
///
/// Returns `None` if either string is unparseable.
#[must_use]
pub fn is_newer(required: &str, running: &str) -> Option<bool> {
    Some(parse_semver(required)? > parse_semver(running)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_version_output() {
        let sample = "Hyprland 0.55.2 built from branch v0.55.2 at commit 39d7e2 clean (version: bump).\nDate: ...\nTag: v0.55.2, commits: 7319\n";
        let info = parse_version(sample).unwrap();
        assert_eq!(info.version, "0.55.2");
        assert_eq!(info.tag.as_deref(), Some("v0.55.2"));
    }

    #[test]
    fn unrecognised_output_is_none() {
        assert!(parse_version("").is_none());
        assert!(parse_version("garbage").is_none());
    }

    #[test]
    fn exit_zero_refusals_are_errors_and_trigger_the_lua_fallback() {
        // Verbatim reply of Hyprland 0.56 running a Lua config.
        let refusal = "keyword can't work with non-legacy parsers. Use eval.";
        let err = checked("keyword x 1", Ok(refusal.to_string())).unwrap_err();
        assert!(wants_eval(&err));
        assert!(checked("keyword x 1", Ok("ok".into())).is_ok());
        assert!(checked("reload", Ok(String::new())).is_ok());

        let eval_err = checked(
            "eval",
            Ok("error: [string \"hl.config({\"]:1: unexpected symbol".into()),
        )
        .unwrap_err();
        assert!(!wants_eval(&eval_err));
    }

    #[test]
    fn semver_parse_and_compare() {
        assert_eq!(parse_semver("0.55.2"), Some((0, 55, 2)));
        assert_eq!(parse_semver("v0.42.0"), Some((0, 42, 0)));
        assert_eq!(parse_semver("0.50.0-dirty"), Some((0, 50, 0)));

        assert_eq!(is_newer("0.56.0", "0.55.2"), Some(true));
        assert_eq!(is_newer("0.42.0", "0.55.2"), Some(false));
        assert_eq!(is_newer("0.55.2", "0.55.2"), Some(false));
    }
}
