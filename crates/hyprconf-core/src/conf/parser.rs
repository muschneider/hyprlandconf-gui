// SPDX-License-Identifier: MIT OR Apache-2.0
//! Parsing of `.conf` text into a lossless [`ConfDocument`], plus multi-file
//! include resolution (`source = ...`) with cycle and missing-file detection.

use std::path::{Path, PathBuf};

use crate::conf::document::{
    Assignment, ConfDocument, Directive, Line, LineEnding, LineKind, Source,
};

/// A typed error from reading or following `.conf` includes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfError {
    /// A file could not be read for a non-"not found" reason.
    #[error("failed to read {path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A file (root or `source`d) does not exist.
    #[error("file not found: {path}")]
    NotFound {
        /// The missing path.
        path: PathBuf,
    },

    /// A `source =` include forms a cycle.
    #[error("include cycle detected at {path} (chain: {})", format_chain(.chain))]
    IncludeCycle {
        /// The path that closed the cycle.
        path: PathBuf,
        /// The include chain leading up to it (root first).
        chain: Vec<PathBuf>,
    },

    /// A glob `source =` pattern was invalid.
    #[error("invalid source glob {pattern:?}: {message}")]
    Glob {
        /// The offending pattern.
        pattern: String,
        /// Why it was rejected.
        message: String,
    },
}

fn format_chain(chain: &[PathBuf]) -> String {
    chain
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// All documents loaded while following a root file's includes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfBundle {
    /// Every loaded file, root first, then includes in the order encountered.
    pub documents: Vec<ConfDocument>,
    /// The index of the root document in [`ConfBundle::documents`].
    pub root: usize,
}

impl ConfBundle {
    /// The root document.
    #[must_use]
    pub fn root(&self) -> &ConfDocument {
        &self.documents[self.root]
    }
}

/// Parses `.conf` text and follows includes.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConfParser;

impl ConfParser {
    /// Parse a single file's text into a [`ConfDocument`] **without** following
    /// `source` includes. This is pure (no I/O) and is the basis of the
    /// byte-for-byte round-trip guarantee.
    #[must_use]
    pub fn parse_str(text: &str, path: Option<PathBuf>) -> ConfDocument {
        let mut lines = Vec::new();
        let mut stack: Vec<String> = Vec::new();

        for (content, ending) in split_lines(text) {
            let kind = classify(&content, &mut stack);
            lines.push(Line {
                raw: content,
                ending,
                kind,
            });
        }

        ConfDocument::new(path, lines)
    }

    /// Read and parse a file, following every `source =` include.
    ///
    /// Relative includes resolve against the including file's directory;
    /// glob patterns expand to all matches (sorted); cycles and missing files
    /// are reported as [`ConfError`]s rather than panicking.
    ///
    /// # Errors
    ///
    /// Returns [`ConfError`] on I/O failure, a missing file, an invalid glob, or
    /// an include cycle.
    pub fn parse_file(path: impl AsRef<Path>) -> Result<ConfBundle, ConfError> {
        let mut bundle = ConfBundle {
            documents: Vec::new(),
            root: 0,
        };
        let mut chain: Vec<PathBuf> = Vec::new();
        let root = load(path.as_ref(), &mut bundle, &mut chain)?;
        bundle.root = root;
        Ok(bundle)
    }
}

/// Recursively load `path` and its includes into `bundle`, returning the index
/// of the loaded document. `chain` holds the canonical paths currently being
/// processed, for cycle detection.
fn load(
    path: &Path,
    bundle: &mut ConfBundle,
    chain: &mut Vec<PathBuf>,
) -> Result<usize, ConfError> {
    let canonical = canonicalize(path)?;
    let text = std::fs::read_to_string(&canonical).map_err(|source| map_io(&canonical, source))?;
    let document = ConfParser::parse_str(&text, Some(canonical.clone()));

    let my_index = bundle.documents.len();
    bundle.documents.push(document);
    chain.push(canonical.clone());

    let dir = canonical
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    // Collect (line index, raw path text) for each `source` line up front so we
    // don't hold a borrow of `bundle` across the recursive `load` call.
    let sources: Vec<(usize, String)> = bundle.documents[my_index]
        .lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| match &line.kind {
            LineKind::Source(s) => Some((i, s.path_text.clone())),
            _ => None,
        })
        .collect();

    for (line_idx, path_text) in sources {
        let candidates = resolve_include(&dir, &path_text)?;
        let mut resolved = Vec::new();
        for candidate in candidates {
            let candidate_canonical = canonicalize(&candidate)?;
            if chain.contains(&candidate_canonical) {
                return Err(ConfError::IncludeCycle {
                    path: candidate_canonical,
                    chain: chain.clone(),
                });
            }
            let child = load(&candidate, bundle, chain)?;
            resolved.push(child);
        }
        if let LineKind::Source(s) = &mut bundle.documents[my_index].lines[line_idx].kind {
            s.resolved = resolved;
        }
    }

    chain.pop();
    Ok(my_index)
}

fn canonicalize(path: &Path) -> Result<PathBuf, ConfError> {
    std::fs::canonicalize(path).map_err(|source| map_io(path, source))
}

fn map_io(path: &Path, source: std::io::Error) -> ConfError {
    if source.kind() == std::io::ErrorKind::NotFound {
        ConfError::NotFound {
            path: path.to_path_buf(),
        }
    } else {
        ConfError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Resolve a `source =` value to concrete candidate paths.
fn resolve_include(dir: &Path, path_text: &str) -> Result<Vec<PathBuf>, ConfError> {
    let raw = path_text.trim();
    let joined: PathBuf = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        dir.join(raw)
    };

    if raw.contains(['*', '?', '[']) {
        let pattern = joined.to_string_lossy().to_string();
        let paths = glob::glob(&pattern).map_err(|e| ConfError::Glob {
            pattern: pattern.clone(),
            message: e.to_string(),
        })?;
        let mut matches = Vec::new();
        for entry in paths {
            match entry {
                Ok(p) => matches.push(p),
                Err(e) => {
                    return Err(ConfError::Glob {
                        pattern: pattern.clone(),
                        message: e.to_string(),
                    })
                }
            }
        }
        matches.sort();
        Ok(matches)
    } else {
        Ok(vec![joined])
    }
}

/// Split text into `(content_without_terminator, ending)` pairs such that
/// concatenating `content + ending.as_str()` reproduces the input exactly.
pub(crate) fn split_lines(text: &str) -> Vec<(String, LineEnding)> {
    let mut lines = Vec::new();
    let mut rest = text;
    loop {
        match rest.find('\n') {
            Some(idx) => {
                let segment = &rest[..idx];
                if let Some(without_cr) = segment.strip_suffix('\r') {
                    lines.push((without_cr.to_string(), LineEnding::CrLf));
                } else {
                    lines.push((segment.to_string(), LineEnding::Lf));
                }
                rest = &rest[idx + 1..];
                if rest.is_empty() {
                    break;
                }
            }
            None => {
                if !rest.is_empty() {
                    lines.push((rest.to_string(), LineEnding::None));
                }
                break;
            }
        }
    }
    lines
}

/// Classify one line's content, updating the section `stack`.
fn classify(content: &str, stack: &mut Vec<String>) -> LineKind {
    let trimmed = content.trim();

    if trimmed.is_empty() {
        return LineKind::Blank;
    }
    if trimmed.starts_with('#') {
        return LineKind::Comment;
    }

    let body = content.trim_start();
    let code = match find_comment(body) {
        Some(i) => body[..i].trim_end(),
        None => body.trim_end(),
    };

    if code == "}" {
        stack.pop();
        return LineKind::SectionClose;
    }
    if code.ends_with('{') && !code.contains('=') {
        let name = code[..code.len() - 1].trim().to_string();
        stack.push(name.clone());
        return LineKind::SectionOpen { name };
    }

    if code.contains('=') {
        let pieces = split_pieces(content);
        if pieces.key == "source" {
            return LineKind::Source(Source {
                indent: pieces.indent,
                sep: pieces.sep,
                path_text: pieces.value,
                trailing: pieces.trailing,
                resolved: Vec::new(),
            });
        }
        if is_directive_keyword(&pieces.key) {
            return LineKind::Directive(Directive {
                indent: pieces.indent,
                keyword: pieces.key,
                sep: pieces.sep,
                args: pieces.value,
                trailing: pieces.trailing,
            });
        }
        let is_variable = pieces.key.starts_with('$');
        let full_path = if is_variable {
            pieces.key.clone()
        } else {
            resolve_path(stack, &pieces.key)
        };
        return LineKind::Assignment(Assignment {
            indent: pieces.indent,
            key: pieces.key,
            full_path,
            sep: pieces.sep,
            value: pieces.value,
            trailing: pieces.trailing,
            is_variable,
        });
    }

    LineKind::Unknown
}

/// Resolve a local key against the section stack into a `:`-joined path.
fn resolve_path(stack: &[String], key: &str) -> String {
    if stack.is_empty() {
        key.to_string()
    } else {
        format!("{}:{}", stack.join(":"), key)
    }
}

/// The split-out pieces of a `key = value [# comment]` line.
struct Pieces {
    indent: String,
    key: String,
    sep: String,
    value: String,
    trailing: String,
}

/// Split a line known to contain `=` into reconstructable pieces such that
/// `indent + key + sep + value + trailing == content`.
fn split_pieces(content: &str) -> Pieces {
    let indent_len = content.len() - content.trim_start().len();
    let (indent, body) = content.split_at(indent_len);

    let (code_ws, comment) = match find_comment(body) {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    let code = code_ws.trim_end();
    let mid_ws = &code_ws[code.len()..];
    let trailing = format!("{mid_ws}{comment}");

    // `code` is guaranteed to contain '=' by the caller.
    let eq = code.find('=').unwrap_or(code.len());
    let lhs = &code[..eq];
    let rhs = &code[eq + 1..];

    let key = lhs.trim();
    let value = rhs.trim();
    let key_end = lhs.trim_end().len();
    let value_start = eq + 1 + (rhs.len() - rhs.trim_start().len());
    let sep = &code[key_end..value_start];

    Pieces {
        indent: indent.to_string(),
        key: key.to_string(),
        sep: sep.to_string(),
        value: value.to_string(),
        trailing,
    }
}

/// Find the byte index where a `#` comment begins, honouring hyprlang's `##`
/// escape (which represents a literal `#`).
pub(crate) fn find_comment(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'#' {
            if bytes.get(idx + 1) == Some(&b'#') {
                idx += 2;
                continue;
            }
            return Some(idx);
        }
        idx += 1;
    }
    None
}

/// Whether `key` is a repeatable directive keyword (as opposed to a scalar
/// option). `source` is handled separately by the caller.
fn is_directive_keyword(key: &str) -> bool {
    const FIXED: &[&str] = &[
        "exec",
        "exec-once",
        "exec-shutdown",
        "execr",
        "exec-once-r",
        "env",
        "envd",
        "monitor",
        "workspace",
        "windowrule",
        "windowrulev2",
        "layerrule",
        "bezier",
        "animation",
        "submap",
        "plugin",
        "permission",
        // Repeatable in 0.56 (`gesture = 3, horizontal, workspace`); treating it
        // as a scalar option makes every gesture but the last vanish.
        "gesture",
        // `p` = fire even while an app inhibits shortcuts.
        "gesturep",
    ];
    if FIXED.contains(&key) {
        return true;
    }
    // Bind variants: `bind` plus zero or more flag letters, never namespaced.
    if key == "binds" || key.contains(':') {
        return false;
    }
    if let Some(flags) = key.strip_prefix("bind") {
        return flags.bytes().all(|b| b"lrenmtiopdcg".contains(&b));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_is_inverse_of_join() {
        for input in [
            "", "a", "a\n", "a\nb", "a\n\n", "\n", "a\r\nb\n", "x\r\n", "\r\n\n",
        ] {
            let joined: String = split_lines(input)
                .iter()
                .map(|(c, e)| format!("{c}{}", e.as_str()))
                .collect();
            assert_eq!(joined, *input, "round-trip failed for {input:?}");
        }
    }

    #[test]
    fn find_comment_respects_escape() {
        assert_eq!(find_comment("abc # x"), Some(4));
        assert_eq!(find_comment("abc"), None);
        assert_eq!(find_comment("a##b # c"), Some(5));
        assert_eq!(find_comment("##"), None);
    }

    #[test]
    fn pieces_reconstruct_line() {
        for line in [
            "    rounding = 10   # nice",
            "kb_variant =",
            "general:gaps_in=5",
            "$mod = SUPER # mainmod",
        ] {
            let p = split_pieces(line);
            let recon = format!("{}{}{}{}{}", p.indent, p.key, p.sep, p.value, p.trailing);
            assert_eq!(recon, line, "reconstruction mismatch");
        }
    }

    #[test]
    fn directive_keyword_detection() {
        assert!(is_directive_keyword("bind"));
        assert!(is_directive_keyword("bindel"));
        assert!(is_directive_keyword("exec-once"));
        assert!(is_directive_keyword("windowrulev2"));
        assert!(!is_directive_keyword("binds"));
        assert!(!is_directive_keyword("binds:drag_threshold"));
        assert!(!is_directive_keyword("rounding"));
    }

    #[test]
    fn classify_tracks_section_nesting() {
        let mut stack = Vec::new();
        assert!(matches!(
            classify("decoration {", &mut stack),
            LineKind::SectionOpen { .. }
        ));
        assert_eq!(stack, vec!["decoration".to_string()]);

        let kind = classify("    rounding = 10", &mut stack);
        match kind {
            LineKind::Assignment(a) => assert_eq!(a.full_path, "decoration:rounding"),
            other => panic!("expected assignment, got {other:?}"),
        }

        assert!(matches!(classify("}", &mut stack), LineKind::SectionClose));
        assert!(stack.is_empty());
    }
}
