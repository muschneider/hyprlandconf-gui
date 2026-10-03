// SPDX-License-Identifier: MIT OR Apache-2.0
//! A lossless, line-oriented model of a single `.conf` file.
//!
//! Round-trip fidelity is the whole point of this type: every physical line is
//! kept verbatim in [`Line::raw`] alongside its original terminator, so
//! re-serializing an unedited document reproduces the input **byte-for-byte**.
//! A parsed, structured view ([`LineKind`]) rides alongside the raw text and is
//! used for editing and for building the semantic [`crate::Config`]; editing a
//! value rewrites only that one line.

use std::path::PathBuf;

use crate::conf::serializer::value_to_conf;
use crate::value::Value;

/// The terminator that originally followed a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    CrLf,
    /// No terminator (only valid on the final line of a file without a trailing
    /// newline).
    None,
}

impl LineEnding {
    /// The literal bytes this terminator emits.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::None => "",
        }
    }
}

/// A scalar `key = value` assignment (also covers `$variable = value`).
///
/// The five text fields reconstruct the line exactly:
/// `raw == indent + key + sep + value + trailing`. Only [`Assignment::value`]
/// is mutated on edit, which is what keeps edits surgical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// Leading whitespace.
    pub indent: String,
    /// The key exactly as written (the local key within any enclosing section).
    pub key: String,
    /// The fully-resolved dotted path, e.g. `decoration:blur:size`. For
    /// variables this is the key itself (e.g. `$mainMod`).
    pub full_path: String,
    /// The text between key and value, e.g. `" = "`.
    pub sep: String,
    /// The value text (no surrounding whitespace, no inline comment).
    pub value: String,
    /// Everything after the value: spacing and any inline `# comment`.
    pub trailing: String,
    /// Whether this is a `$variable` definition.
    pub is_variable: bool,
}

impl Assignment {
    fn render(&self) -> String {
        format!(
            "{}{}{}{}{}",
            self.indent, self.key, self.sep, self.value, self.trailing
        )
    }

    /// The inline comment (including its leading `#`), if any.
    #[must_use]
    pub fn inline_comment(&self) -> Option<&str> {
        self.trailing.find('#').map(|i| &self.trailing[i..])
    }
}

/// A repeatable directive: `bind = ...`, `exec-once = ...`, `monitor = ...`, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// Leading whitespace.
    pub indent: String,
    /// The directive keyword, e.g. `bind`, `windowrulev2`, `exec-once`.
    pub keyword: String,
    /// The text between keyword and arguments, e.g. `" = "`.
    pub sep: String,
    /// The raw argument text (no inline comment).
    pub args: String,
    /// Spacing and any inline `# comment` after the arguments.
    pub trailing: String,
}

impl Directive {
    fn render(&self) -> String {
        format!(
            "{}{}{}{}{}",
            self.indent, self.keyword, self.sep, self.args, self.trailing
        )
    }

    /// The inline comment (including its leading `#`), if any.
    #[must_use]
    pub fn inline_comment(&self) -> Option<&str> {
        self.trailing.find('#').map(|i| &self.trailing[i..])
    }
}

/// A `source = path` include directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Leading whitespace.
    pub indent: String,
    /// The text between `source` and the path, e.g. `" = "`.
    pub sep: String,
    /// The include path/pattern exactly as written.
    pub path_text: String,
    /// Spacing and any inline `# comment`.
    pub trailing: String,
    /// Indices (into [`crate::ConfBundle::documents`]) of the file(s) this
    /// include resolved to. Empty when parsed without following includes.
    pub resolved: Vec<usize>,
}

impl Source {
    fn render(&self) -> String {
        format!(
            "{}source{}{}{}",
            self.indent, self.sep, self.path_text, self.trailing
        )
    }
}

/// The structured interpretation of a single line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// An empty or whitespace-only line.
    Blank,
    /// A whole-line `#` comment.
    Comment,
    /// A section header `name {`.
    SectionOpen {
        /// The section name written before `{`.
        name: String,
    },
    /// A section terminator `}`.
    SectionClose,
    /// A `key = value` assignment.
    Assignment(Assignment),
    /// A repeatable directive.
    Directive(Directive),
    /// A `source = path` include.
    Source(Source),
    /// A line we could not classify; preserved verbatim.
    Unknown,
}

/// A single physical line: its verbatim text, terminator and parsed view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The verbatim line content, **without** the terminator.
    pub raw: String,
    /// The original terminator.
    pub ending: LineEnding,
    /// The structured interpretation.
    pub kind: LineKind,
}

/// The outcome of [`ConfDocument::set_option`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOutcome {
    /// An existing assignment line was edited in place.
    Edited,
    /// A new assignment line was inserted into an existing section block.
    InsertedInSection,
    /// A new flat assignment line was appended at the end of the file.
    Appended,
}

/// A lossless, editable model of one `.conf` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfDocument {
    /// The file this document was read from, if any.
    pub path: Option<PathBuf>,
    /// The lines, in order.
    pub lines: Vec<Line>,
    /// The terminator used when inserting new lines.
    default_ending: LineEnding,
}

impl ConfDocument {
    /// Construct a document from already-parsed lines.
    #[must_use]
    pub fn new(path: Option<PathBuf>, lines: Vec<Line>) -> Self {
        let default_ending = lines
            .iter()
            .map(|l| l.ending)
            .find(|e| *e != LineEnding::None)
            .unwrap_or(LineEnding::Lf);
        Self {
            path,
            lines,
            default_ending,
        }
    }

    /// Serialize the document back to text.
    ///
    /// For an unedited document this reproduces the original input exactly.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&line.raw);
            out.push_str(line.ending.as_str());
        }
        out
    }

    /// Iterate over the scalar assignments in the document, in order.
    pub fn assignments(&self) -> impl Iterator<Item = &Assignment> {
        self.lines.iter().filter_map(|l| match &l.kind {
            LineKind::Assignment(a) => Some(a),
            _ => None,
        })
    }

    /// The current text value of the last assignment for `full_path`, if any.
    #[must_use]
    pub fn option_value(&self, full_path: &str) -> Option<&str> {
        self.find_assignment(full_path)
            .map(|i| match &self.lines[i].kind {
                LineKind::Assignment(a) => a.value.as_str(),
                _ => unreachable!("index came from find_assignment"),
            })
    }

    /// Set the text value of an option, editing in place if it exists or
    /// inserting it into the right place otherwise.
    ///
    /// Editing rewrites only the target line; indentation, the `=` spacing and
    /// any inline comment are preserved. `full_path` may be given in either
    /// spelling (`tap_to_click` / `tap-to-click`); an existing line is found
    /// under either, and a new one is always written in the `.conf` spelling
    /// Hyprland requires.
    pub fn set_option(&mut self, full_path: &str, value: &str) -> SetOutcome {
        if let Some(idx) = self.find_assignment(full_path) {
            if let LineKind::Assignment(a) = &mut self.lines[idx].kind {
                a.value = value.to_string();
                self.lines[idx].raw = a.render();
            }
            return SetOutcome::Edited;
        }
        let conf = crate::schema::conf_path(full_path).into_owned();
        self.insert_option(&conf, value)
    }

    /// Whether any assignment in this document sets `full_path` (in either
    /// spelling).
    #[must_use]
    pub fn defines(&self, full_path: &str) -> bool {
        self.find_assignment(full_path).is_some()
    }

    /// Like [`ConfDocument::set_option`] but renders a typed [`Value`].
    pub fn set_option_value(&mut self, full_path: &str, value: &Value) -> SetOutcome {
        let text = value_to_conf(value);
        self.set_option(full_path, &text)
    }

    /// The index of the last assignment line matching `full_path` (last write
    /// wins, mirroring hyprlang evaluation), comparing canonical spellings.
    fn find_assignment(&self, full_path: &str) -> Option<usize> {
        let wanted = crate::schema::canonical_path(full_path);
        self.lines
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, l)| match &l.kind {
                LineKind::Assignment(a)
                    if a.full_path == full_path
                        || crate::schema::canonical_path(&a.full_path) == wanted =>
                {
                    Some(i)
                }
                _ => None,
            })
    }

    fn insert_option(&mut self, full_path: &str, value: &str) -> SetOutcome {
        let top = full_path.split(':').next().unwrap_or(full_path);

        if let Some((_open, close, indent)) = self.find_block(top) {
            let remainder = full_path
                .strip_prefix(top)
                .and_then(|r| r.strip_prefix(':'))
                .unwrap_or(full_path);
            let line =
                make_assignment_line(&indent, remainder, full_path, value, self.default_ending);
            self.lines.insert(close, line);
            SetOutcome::InsertedInSection
        } else {
            self.ensure_final_newline();
            let line = make_assignment_line("", full_path, full_path, value, LineEnding::None);
            self.lines.push(line);
            SetOutcome::Appended
        }
    }

    /// Find a top-level section block by name, returning
    /// `(open_index, close_index, inner_indent)`.
    fn find_block(&self, name: &str) -> Option<(usize, usize, String)> {
        let open = self
            .lines
            .iter()
            .position(|l| matches!(&l.kind, LineKind::SectionOpen { name: n } if n == name))?;

        let mut depth = 1usize;
        for idx in (open + 1)..self.lines.len() {
            match &self.lines[idx].kind {
                LineKind::SectionOpen { .. } => depth += 1,
                LineKind::SectionClose => {
                    depth -= 1;
                    if depth == 0 {
                        let inner_indent = self.lines[(open + 1)..idx]
                            .iter()
                            .find(|l| !l.raw.trim().is_empty())
                            .map(|l| leading_whitespace(&l.raw).to_string())
                            .unwrap_or_else(|| {
                                format!("{}    ", leading_whitespace(&self.lines[open].raw))
                            });
                        return Some((open, idx, inner_indent));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Ensure the final line carries a terminator before appending.
    fn ensure_final_newline(&mut self) {
        if let Some(last) = self.lines.last_mut() {
            if last.ending == LineEnding::None {
                last.ending = self.default_ending;
            }
        }
    }
}

fn make_assignment_line(
    indent: &str,
    key: &str,
    full_path: &str,
    value: &str,
    ending: LineEnding,
) -> Line {
    let assignment = Assignment {
        indent: indent.to_string(),
        key: key.to_string(),
        full_path: full_path.to_string(),
        sep: " = ".to_string(),
        value: value.to_string(),
        trailing: String::new(),
        is_variable: key.starts_with('$'),
    };
    Line {
        raw: assignment.render(),
        ending,
        kind: LineKind::Assignment(assignment),
    }
}

/// The leading-whitespace prefix of `s`.
fn leading_whitespace(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

// Re-export `render` for the parser's debug assertions and the serializer.
impl LineKind {
    /// Re-render this line's structured form to text (used to keep `raw` in
    /// sync after edits and to assert parser fidelity in tests).
    #[must_use]
    pub fn render(&self, raw: &str) -> String {
        match self {
            LineKind::Assignment(a) => a.render(),
            LineKind::Directive(d) => d.render(),
            LineKind::Source(s) => s.render(),
            // Structural / opaque lines have no separately-stored pieces.
            LineKind::Blank
            | LineKind::Comment
            | LineKind::SectionOpen { .. }
            | LineKind::SectionClose
            | LineKind::Unknown => raw.to_string(),
        }
    }
}
