// SPDX-License-Identifier: MIT OR Apache-2.0
//! Validate a generated config with the **real** Hyprland binary, before it is
//! written anywhere the compositor will read it.
//!
//! `Hyprland --verify-config -c <file>` parses a config and prints diagnostics
//! without starting a session. That makes it the ground truth for "will this
//! config actually work?", which matters most during `.conf` -> Lua migration:
//! the Lua bindings are strict about table shapes and dispatcher objects, and
//! some mistakes (a rule in the wrong field) load *successfully* while doing
//! nothing at all.
//!
//! # Safety
//!
//! `--verify-config` **executes top-level side effects**: a bare
//! `hl.exec_cmd("waybar")` at file scope really does launch waybar. The
//! serializer already nests autostart inside `hl.on("hyprland.start", ...)`,
//! which is not fired during verification, but a *user-authored* file may not.
//! [`verify_text`] therefore neutralises top-level exec calls in the temporary
//! copy it checks — see [`neutralise_execs`].

use std::path::{Path, PathBuf};
use std::process::Command;

/// One diagnostic line reported by Hyprland.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyIssue {
    /// The 1-based line number in the verified file, when Hyprland reported one.
    pub line: Option<u32>,
    /// The message, with the file prefix stripped.
    pub message: String,
}

/// The outcome of a verification run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Hyprland parsed the config with no complaints.
    Ok,
    /// Hyprland reported problems.
    Problems(Vec<VerifyIssue>),
    /// Verification could not be performed (no binary, spawn failure, ...).
    Unavailable(String),
}

impl Verdict {
    /// Whether the config parsed cleanly.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }

    /// The reported issues, if any.
    #[must_use]
    pub fn issues(&self) -> &[VerifyIssue] {
        match self {
            Self::Problems(v) => v,
            _ => &[],
        }
    }
}

/// Whether a Hyprland binary capable of `--verify-config` is on `PATH`.
#[must_use]
pub fn available() -> bool {
    binary().is_some()
}

/// Locate the Hyprland binary.
fn binary() -> Option<&'static str> {
    for candidate in ["Hyprland", "hyprland"] {
        let ok = Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            return Some(candidate);
        }
    }
    None
}

/// Verify an on-disk config file.
///
/// This runs the file as-is, so only use it on files the user already owns.
/// Prefer [`verify_text`] for generated content.
#[must_use]
pub fn verify_file(path: &Path) -> Verdict {
    let Some(bin) = binary() else {
        return Verdict::Unavailable(
            "the Hyprland binary was not found on PATH, so the generated config \
             could not be machine-checked."
                .to_string(),
        );
    };

    let output = match Command::new(bin)
        .arg("--verify-config")
        .arg("-c")
        .arg(path)
        .output()
    {
        Ok(o) => o,
        Err(e) => return Verdict::Unavailable(format!("could not run {bin}: {e}")),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_output(&format!("{stdout}\n{stderr}"), path)
}

/// Verify generated config `text` by writing it to a temporary file.
///
/// `extension` should be `"lua"` or `"conf"` — Hyprland picks its parser from
/// the file extension.
#[must_use]
pub fn verify_text(text: &str, extension: &str) -> Verdict {
    let dir = std::env::temp_dir().join(format!("hyprconf-verify-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Verdict::Unavailable(format!("could not create a temporary directory: {e}"));
    }
    let path = dir.join(format!("check.{extension}"));

    let body = if extension == "lua" {
        neutralise_execs(text)
    } else {
        text.to_string()
    };
    if let Err(e) = std::fs::write(&path, body) {
        return Verdict::Unavailable(format!("could not write a temporary config: {e}"));
    }

    let verdict = verify_file(&path);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    verdict
}

/// Comment out **top-level** `hl.exec_cmd(...)` statements.
///
/// Hyprland runs these during `--verify-config`, which would spawn the user's
/// whole autostart just to answer "is this file valid?". Calls nested inside a
/// callback (`hl.on("hyprland.start", function() ... end)`) are indented and are
/// not fired during verification, so they are left alone — keeping them checked.
#[must_use]
pub fn neutralise_execs(text: &str) -> String {
    text.lines()
        .map(|line| {
            let is_top_level_exec = !line.starts_with(char::is_whitespace)
                && (line.starts_with("hl.exec_cmd") || line.starts_with("hl.exec_raw"));
            if is_top_level_exec {
                format!("-- [hyprconf: not run during verification] {line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse Hyprland's `======== Config parsing result:` block.
fn parse_output(text: &str, path: &Path) -> Verdict {
    const MARKER: &str = "Config parsing result:";
    let Some(idx) = text.find(MARKER) else {
        // No marker at all means the binary did not get far enough to report.
        return Verdict::Unavailable(
            "Hyprland did not produce a config parsing report.".to_string(),
        );
    };

    let body = &text[idx + MARKER.len()..];
    let prefix = format!("{}:", path.display());

    let mut issues = Vec::new();
    for raw in body.lines() {
        let line = strip_ansi(raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "config ok" {
            return Verdict::Ok;
        }
        // `/tmp/x/check.lua:38: hl.bind: failed to parse ...`
        let rest = line.strip_prefix(&prefix).unwrap_or(line);
        let (line_no, message) = match rest.split_once(':') {
            Some((n, m)) if n.trim().parse::<u32>().is_ok() => {
                (n.trim().parse::<u32>().ok(), m.trim().to_string())
            }
            _ => (None, rest.trim().to_string()),
        };
        if message.is_empty() {
            continue;
        }
        issues.push(VerifyIssue {
            line: line_no,
            message,
        });
    }

    if issues.is_empty() {
        Verdict::Ok
    } else {
        Verdict::Problems(issues)
    }
}

/// Remove ANSI SGR sequences (Hyprland colourises its log output).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Where a temporary verification file would be written (exposed for tests).
#[must_use]
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!("hyprconf-verify-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clean_report() {
        let text = "noise\n======== Config parsing result:\n\nconfig ok\n";
        assert_eq!(parse_output(text, Path::new("/tmp/x.lua")), Verdict::Ok);
    }

    #[test]
    fn parses_issues_with_line_numbers() {
        let text = "======== Config parsing result:\n\n\
                    /tmp/x.lua:38: hl.bind: failed to parse key string\n\
                    /tmp/x.lua:7: unknown config key 'general.nope'\n";
        let v = parse_output(text, Path::new("/tmp/x.lua"));
        let issues = v.issues();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].line, Some(38));
        assert_eq!(issues[0].message, "hl.bind: failed to parse key string");
        assert_eq!(issues[1].line, Some(7));
        assert!(!v.is_ok());
    }

    #[test]
    fn missing_report_is_unavailable_not_ok() {
        // A crash or a missing binary must never be mistaken for success.
        let v = parse_output("some unrelated output", Path::new("/tmp/x.lua"));
        assert!(matches!(v, Verdict::Unavailable(_)));
        assert!(!v.is_ok());
    }

    #[test]
    fn strips_ansi_colour_codes() {
        assert_eq!(strip_ansi("\u{1b}[1;31mERR \u{1b}[0m: boom"), "ERR : boom");
    }

    #[test]
    fn neutralises_only_top_level_execs() {
        let src = "hl.exec_cmd(\"waybar\")\n\
                   hl.on(\"hyprland.start\", function()\n\
                   \x20   hl.exec_cmd(\"nm-applet\")\n\
                   end)\n";
        let out = neutralise_execs(src);
        assert!(out.starts_with("-- [hyprconf: not run during verification] hl.exec_cmd"));
        // The nested call is still verified, so a typo in it is still caught.
        assert!(out.contains("    hl.exec_cmd(\"nm-applet\")"));
    }

    #[test]
    fn verdict_helpers_are_consistent() {
        assert!(Verdict::Ok.is_ok());
        assert!(Verdict::Ok.issues().is_empty());
        assert!(!Verdict::Problems(vec![VerifyIssue {
            line: None,
            message: "x".into()
        }])
        .is_ok());
    }
}
