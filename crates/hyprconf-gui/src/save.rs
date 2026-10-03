// SPDX-License-Identifier: MIT OR Apache-2.0
//! Save planning: turn the edited model into the exact bytes to write, choosing
//! between *preserving* the original document (same-format conf, scalar-only
//! edits — keeps comments and writes back only changed files) and *regenerating*
//! a fresh file (format conversion, collection edits, or Lua).
//!
//! The plan is computed up-front so the GUI can show an accurate preview/diff;
//! [`perform_save`] then writes exactly those bytes atomically with backups.

use std::path::PathBuf;

use hyprconf_core::conf::{config_to_conf, value_to_conf};
use hyprconf_core::{
    fs as core_fs, validate_config, ConfigFormat, LuaParser, LuaSerializer, SaveReport, Schema,
    Severity,
};

use crate::diff::{self, DiffLine};
use crate::edit::{
    device_issue, env_issue, exec_issue, gesture_issue, gesture_shadowed_by, keybind_issue,
    layer_rule_issue, monitor_issue, permission_issue, plugin_issue, variable_issue,
    window_rule_issue, workspace_issue,
};
use crate::load::{Loaded, Origin};
use crate::Selection;

/// How a save will be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// Edit the original document(s) in place (comments/structure preserved).
    Preserve,
    /// Regenerate a fresh file from the model.
    Regenerate,
}

/// One prospective file write.
#[derive(Debug, Clone)]
pub struct FileWrite {
    /// Destination path.
    pub path: PathBuf,
    /// Current on-disk content (empty if the file does not exist yet).
    pub before: String,
    /// Content that will be written.
    pub after: String,
}

impl FileWrite {
    /// Whether this write actually changes the file.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.before != self.after
    }
}

/// A complete, reviewable save plan.
#[derive(Debug, Clone)]
pub struct SavePlan {
    /// Whether the original is preserved or regenerated.
    pub mode: SaveMode,
    /// The target on-disk format.
    pub format: ConfigFormat,
    /// Every prospective file write (changed and unchanged).
    pub files: Vec<FileWrite>,
    /// Dynamic Lua regions that will be lost (regenerating a Lua-origin config).
    pub drops_dynamic: usize,
    /// The root file to (re)load after saving.
    pub root: PathBuf,
}

/// A validation finding for the save review, with a jump target.
#[derive(Debug, Clone)]
pub struct Problem {
    /// Short label (option path or "keybind #2").
    pub label: String,
    /// The message.
    pub message: String,
    /// Severity (errors block, warnings are overridable).
    pub severity: Severity,
    /// Where to jump to fix it.
    pub jump: Option<Selection>,
}

/// Validate the whole model: scalar options against the schema, plus structured
/// collection sanity. Returns every problem found.
#[must_use]
pub fn review(loaded: &Loaded, schema: &Schema) -> Vec<Problem> {
    let mut out = Vec::new();

    for p in validate_config(schema, &loaded.config) {
        let section = p.path.split(':').next().unwrap_or_default().to_string();
        out.push(Problem {
            label: p.path,
            message: p.message,
            severity: p.severity,
            jump: Some(Selection::Section(section)),
        });
    }

    use hyprconf_core::schema::CollectionId;
    let c = &loaded.config;
    for (i, t) in c.keybinds.iter().enumerate() {
        if let Some(m) = keybind_issue(&t.value) {
            out.push(collection_problem("keybind", i, m, CollectionId::Keybinds));
        }
    }
    for (i, t) in c.window_rules.iter().enumerate() {
        if let Some(m) = window_rule_issue(&t.value) {
            out.push(collection_problem(
                "window rule",
                i,
                m,
                CollectionId::WindowRules,
            ));
        }
    }
    for (i, t) in c.layer_rules.iter().enumerate() {
        if let Some(m) = layer_rule_issue(&t.value) {
            out.push(collection_problem(
                "layer rule",
                i,
                m,
                CollectionId::LayerRules,
            ));
        }
    }
    for (i, t) in c.monitors.iter().enumerate() {
        if let Some(m) = monitor_issue(&t.value) {
            out.push(collection_problem("monitor", i, m, CollectionId::Monitors));
        }
    }
    for (i, t) in c.env.iter().enumerate() {
        if let Some(m) = env_issue(&t.value) {
            out.push(collection_problem("env", i, m, CollectionId::Env));
        }
    }
    for (i, t) in c.execs.iter().enumerate() {
        if let Some(m) = exec_issue(&t.value) {
            out.push(collection_problem("exec", i, m, CollectionId::Execs));
        }
    }
    for (i, t) in c.workspaces.iter().enumerate() {
        if let Some(m) = workspace_issue(&t.value) {
            out.push(collection_problem(
                "workspace rule",
                i,
                m,
                CollectionId::Workspaces,
            ));
        }
    }
    for (i, t) in c.variables.iter().enumerate() {
        if let Some(m) = variable_issue(&t.value) {
            out.push(collection_problem(
                "variable",
                i,
                m,
                CollectionId::Variables,
            ));
        }
    }
    for (i, t) in c.gestures.iter().enumerate() {
        if let Some(m) = gesture_issue(&t.value) {
            out.push(collection_problem("gesture", i, m, CollectionId::Gestures));
        } else if let Some(j) = gesture_shadowed_by(&c.gestures, i) {
            // Hyprland reports it and ignores the gesture, but loads the rest.
            out.push(Problem {
                severity: Severity::Warning,
                ..collection_problem(
                    "gesture",
                    i,
                    format!("overshadowed by gesture #{}", j + 1),
                    CollectionId::Gestures,
                )
            });
        }
    }
    for (i, t) in c.devices.iter().enumerate() {
        if let Some(m) = device_issue(&t.value) {
            out.push(collection_problem("device", i, m, CollectionId::Devices));
        }
    }
    for (i, t) in c.permissions.iter().enumerate() {
        if let Some(m) = permission_issue(&t.value) {
            out.push(collection_problem(
                "permission",
                i,
                m,
                CollectionId::Permissions,
            ));
        }
    }
    for (i, t) in c.plugins.iter().enumerate() {
        if let Some(m) = plugin_issue(&t.value) {
            // A relative path passes `--verify-config` but never loads.
            out.push(Problem {
                severity: Severity::Warning,
                ..collection_problem("plugin", i, m, CollectionId::Plugins)
            });
        }
    }

    out
}

fn collection_problem(
    kind: &str,
    index: usize,
    message: String,
    id: hyprconf_core::schema::CollectionId,
) -> Problem {
    Problem {
        label: format!("{kind} #{}", index + 1),
        message,
        severity: Severity::Error,
        jump: Some(Selection::Collection(id)),
    }
}

/// Why a save is blocked, if it is.
#[must_use]
pub fn blocked(problems: &[Problem], override_warnings: bool) -> Option<String> {
    let errors = problems
        .iter()
        .filter(|p| p.severity == Severity::Error)
        .count();
    let warnings = problems
        .iter()
        .filter(|p| p.severity == Severity::Warning)
        .count();
    if errors > 0 {
        Some(format!("{errors} error(s) must be fixed before saving"))
    } else if warnings > 0 && !override_warnings {
        Some(format!(
            "{warnings} warning(s) — enable “save anyway” to proceed"
        ))
    } else {
        None
    }
}

impl SavePlan {
    /// The files that actually change.
    #[must_use]
    pub fn changed_files(&self) -> Vec<&FileWrite> {
        self.files.iter().filter(|f| f.changed()).collect()
    }

    /// Whether there is anything to write.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.files.iter().any(FileWrite::changed)
    }
}

/// A precomputed before/after line diff for one changed file.
///
/// Computing this is `O(n·m)` (LCS), so it is built once in [`build_preview`]
/// rather than on every frame inside `view`.
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// The file the diff is for.
    pub path: PathBuf,
    /// Number of added lines.
    pub added: usize,
    /// Number of removed lines.
    pub removed: usize,
    /// The diffed lines.
    pub lines: Vec<DiffLine>,
}

/// Everything the save panel needs to render, computed up front (off the render
/// path): the plan, validation results, and per-file diffs.
///
/// This is built in `update` (when the panel opens, the format changes, or the
/// model is undone/redone) and cached on the app, so `view` stays cheap and does
/// no filesystem I/O, serialization, or diffing.
#[derive(Debug, Clone)]
pub struct SavePreview {
    /// The computed write plan.
    pub plan: SavePlan,
    /// Validation findings (errors block, warnings overridable).
    pub problems: Vec<Problem>,
    /// One entry per changed file, already diffed.
    pub diffs: Vec<FileDiff>,
}

/// Compute the full [`SavePreview`] for the current model and target format.
#[must_use]
pub fn build_preview(loaded: &Loaded, target: ConfigFormat, schema: &Schema) -> SavePreview {
    let plan = plan_save(loaded, target);
    let mut problems = review(loaded, schema);
    problems.extend(plan_problems(&plan));
    let diffs = plan
        .changed_files()
        .into_iter()
        .map(|file| {
            let lines = diff::diff_lines(&file.before, &file.after);
            let (added, removed) = diff::summary(&lines);
            FileDiff {
                path: file.path.clone(),
                added,
                removed,
                lines,
            }
        })
        .collect();
    SavePreview {
        plan,
        problems,
        diffs,
    }
}

/// Serialize a config to a given format from scratch.
#[must_use]
pub fn serialize(config: &hyprconf_core::Config, format: ConfigFormat) -> String {
    match format {
        ConfigFormat::Lua => LuaSerializer::serialize(config),
        ConfigFormat::Conf => config_to_conf(config),
    }
}

/// Compute the save plan for the current model and chosen output format.
#[must_use]
pub fn plan_save(loaded: &Loaded, target: ConfigFormat) -> SavePlan {
    let same_format = target == loaded.format;
    let scalar_only = loaded.touched.is_empty();

    match &loaded.origin {
        // Preserve path: same-format conf with no structured-collection edits.
        Origin::Conf(bundle) if same_format && scalar_only => preserve_conf(loaded, bundle),
        // Lua with only settings changed: never regenerate a hand-written file
        // (that drops every loop, function and comment in it).
        Origin::Lua if same_format && scalar_only => preserve_lua(loaded),
        _ => regenerate(loaded, target),
    }
}

/// First line of the block hyprconf owns inside a hand-written `hyprland.lua`.
pub(crate) const MANAGED_BEGIN: &str =
    "-- >>> hyprconf: settings changed in hyprconf (delete this block to undo them) >>>";
/// Last line of the managed block.
pub(crate) const MANAGED_END: &str = "-- <<< hyprconf <<<";

/// Split `text` around an existing managed block: `(before, block, after)`.
fn split_managed(text: &str) -> (&str, Option<&str>, &str) {
    let Some(start) = text.find(MANAGED_BEGIN) else {
        return (text, None, "");
    };
    let body_start = start + MANAGED_BEGIN.len();
    let Some(end_rel) = text[body_start..].find(MANAGED_END) else {
        return (text, None, "");
    };
    let end = body_start + end_rel + MANAGED_END.len();
    let after = text[end..].strip_prefix('\n').unwrap_or(&text[end..]);
    (
        &text[..start],
        Some(&text[body_start..body_start + end_rel]),
        after,
    )
}

/// Save settings edits to a Lua config **without regenerating it**.
///
/// Hyprland applies `hl.config` calls in order, so a block appended at the end
/// of the root file overrides whatever the hand-written part (or a `require`d
/// file) set, while every other byte — loops, functions, comments, binds built
/// in code — stays exactly as the user wrote it. The block is fenced by
/// [`MANAGED_BEGIN`]/[`MANAGED_END`]; later saves update it in place, carrying
/// over the options it already held so earlier edits are never dropped.
fn preserve_lua(loaded: &Loaded) -> SavePlan {
    let path = loaded.source.clone();
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    let (head, block, tail) = split_managed(&before);

    // Options the block already manages keep being managed…
    let mut managed: Vec<String> = block
        .and_then(|b| LuaParser::parse_str(b, None).ok())
        .map(|doc| {
            hyprconf_core::lua::document_to_config(&doc, Schema::shared())
                .0
                .options
                .into_keys()
                .collect()
        })
        .unwrap_or_default();
    // …plus everything edited in this session.
    let mut dirty: Vec<&String> = loaded.dirty.iter().collect();
    dirty.sort();
    for path in dirty {
        if !managed.contains(path) {
            managed.push(path.clone());
        }
    }

    let entries: Vec<(&str, &hyprconf_core::Value)> = managed
        .iter()
        .filter_map(|p| loaded.config.get(p).map(|v| (p.as_str(), v)))
        .collect();

    let mut after = head.to_string();
    if !entries.is_empty() {
        if !after.is_empty() && !after.ends_with('\n') {
            after.push('\n');
        }
        if block.is_none() && !after.is_empty() && !after.ends_with("\n\n") {
            after.push('\n');
        }
        after.push_str(MANAGED_BEGIN);
        after.push('\n');
        after.push_str(&hyprconf_core::lua::config_call(entries));
        after.push_str(MANAGED_END);
        after.push('\n');
    }
    after.push_str(tail);

    SavePlan {
        mode: SaveMode::Preserve,
        format: ConfigFormat::Lua,
        files: vec![FileWrite {
            path: path.clone(),
            before,
            after,
        }],
        drops_dynamic: 0,
        root: path,
    }
}

/// Plan-level problems on top of [`review`]: today, the one irreversible case —
/// regenerating a Lua config whose dynamic code cannot be carried over.
#[must_use]
pub fn plan_problems(plan: &SavePlan) -> Vec<Problem> {
    let mut out = Vec::new();
    if plan.drops_dynamic > 0 {
        out.push(Problem {
            label: "Lua code".to_string(),
            message: format!(
                "{} block(s) of Lua code (loops, functions, binds built in code) can't be \
                 regenerated and will be removed. Keybind/rule edits on a Lua config rewrite \
                 the file; a backup is kept.",
                plan.drops_dynamic
            ),
            severity: Severity::Warning,
            jump: None,
        });
    }
    out
}

fn preserve_conf(loaded: &Loaded, bundle: &hyprconf_core::ConfBundle) -> SavePlan {
    let originals: Vec<String> = bundle.documents.iter().map(|d| d.to_text()).collect();
    let mut docs = bundle.documents.clone();

    for path in &loaded.dirty {
        let Some(value) = loaded.config.get(path) else {
            continue;
        };
        let text = value_to_conf(value);
        let idx = owning_doc(bundle, path).unwrap_or(bundle.root);
        docs[idx].set_option(path, &text);
    }

    let files = docs
        .iter()
        .zip(originals)
        .map(|(doc, before)| FileWrite {
            path: doc.path.clone().unwrap_or_else(|| loaded.source.clone()),
            before,
            after: doc.to_text(),
        })
        .collect();

    SavePlan {
        mode: SaveMode::Preserve,
        format: loaded.format,
        files,
        drops_dynamic: 0,
        root: loaded.source.clone(),
    }
}

fn regenerate(loaded: &Loaded, target: ConfigFormat) -> SavePlan {
    let after = serialize(&loaded.config, target);
    let path = target_path(&loaded.source, target);
    let before = std::fs::read_to_string(&path).unwrap_or_default();

    let drops_dynamic = if matches!(loaded.origin, Origin::Lua) {
        loaded.dynamic_regions
    } else {
        0
    };

    SavePlan {
        mode: SaveMode::Regenerate,
        format: target,
        files: vec![FileWrite {
            path: path.clone(),
            before,
            after,
        }],
        drops_dynamic,
        root: path,
    }
}

/// Map the root source path to the target path for `format`, swapping the
/// extension when converting.
fn target_path(source: &std::path::Path, format: ConfigFormat) -> PathBuf {
    let ext = match format {
        ConfigFormat::Lua => "lua",
        ConfigFormat::Conf => "conf",
    };
    if source.extension().and_then(|e| e.to_str()) == Some(ext) {
        source.to_path_buf()
    } else {
        source.with_extension(ext)
    }
}

/// The document (highest index) that defines `path`, if any.
fn owning_doc(bundle: &hyprconf_core::ConfBundle, path: &str) -> Option<usize> {
    // `defines` matches either spelling (`tap_to_click` / `tap-to-click`).
    bundle.documents.iter().rposition(|doc| doc.defines(path))
}

/// Write every changed file in the plan atomically, backing each up first.
///
/// # Errors
///
/// Returns the first filesystem error as a string.
pub fn perform_save(plan: &SavePlan) -> Result<Vec<SaveReport>, String> {
    let mut reports = Vec::new();
    for file in plan.changed_files() {
        let report =
            core_fs::save_atomically(&file.path, &file.after).map_err(|e| e.to_string())?;
        reports.push(report);
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{CollectionAction, EditAction};
    use crate::load::{load_config, LoadState};
    use hyprconf_core::schema::CollectionId;
    use hyprconf_core::{conf, lua, ConfParser, LuaParser, Schema, Value};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hyprconf-save-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn load_from(path: &std::path::Path) -> Box<crate::load::Loaded> {
        match load_config(Some(path.to_path_buf())) {
            LoadState::Loaded(l) => l,
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn preserve_conf_writes_only_the_edited_scalar_line() {
        let dir = temp_dir("preserve");
        let path = dir.join("hyprland.conf");
        let original = "# my config\ndecoration {\n    rounding = 5   # nice corners\n    blur {\n        enabled = true\n    }\n}\n";
        std::fs::write(&path, original).unwrap();

        let mut loaded = load_from(&path);
        loaded.apply(
            EditAction::EditText(
                "decoration:rounding".into(),
                crate::edit::Slot::Main,
                "12".into(),
            ),
            Schema::shared(),
        );

        let plan = plan_save(&loaded, ConfigFormat::Conf);
        assert_eq!(plan.mode, SaveMode::Preserve);
        let changed = plan.changed_files();
        assert_eq!(changed.len(), 1);
        let after = &changed[0].after;
        // Comment preserved, only the value changed.
        assert!(after.contains("rounding = 12   # nice corners"), "{after}");
        assert!(after.contains("# my config"));
        assert!(after.contains("enabled = true"));

        // Write and confirm preview == on-disk.
        perform_save(&plan).unwrap();
        assert_eq!(std::fs::read_to_string(&changed[0].path).unwrap(), *after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn convert_conf_to_lua_is_semantically_equivalent_and_preview_matches() {
        let dir = temp_dir("convert");
        let path = dir.join("hyprland.conf");
        std::fs::write(
            &path,
            "general {\n    gaps_in = 7\n    layout = master\n}\nbind = SUPER, Q, killactive\n",
        )
        .unwrap();

        let loaded = load_from(&path);
        let plan = plan_save(&loaded, ConfigFormat::Lua);
        assert_eq!(plan.mode, SaveMode::Regenerate);
        assert_eq!(plan.format, ConfigFormat::Lua);
        let file = &plan.files[0];
        assert_eq!(file.path.extension().unwrap(), "lua");

        // Round-trip equivalence: parse the generated lua back, compare values.
        let schema = Schema::shared();
        let doc = LuaParser::parse_str(&file.after, None).unwrap();
        let (lua_cfg, _) = lua::document_to_config(&doc, schema);
        assert_eq!(
            lua_cfg.get("general:gaps_in"),
            Some(&Value::CssGap(hyprconf_core::value::CssGap::uniform(7)))
        );
        assert_eq!(
            lua_cfg.get("general:layout"),
            Some(&Value::String("master".into()))
        );
        assert_eq!(lua_cfg.keybinds.len(), 1);
        assert_eq!(lua_cfg.keybinds[0].value.dispatcher, "killactive");

        // Preview == on-disk after writing.
        perform_save(&plan).unwrap();
        assert_eq!(std::fs::read_to_string(&file.path).unwrap(), file.after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collection_edits_force_regenerate_even_same_format() {
        let dir = temp_dir("coll");
        let path = dir.join("hyprland.conf");
        std::fs::write(&path, "decoration {\n    rounding = 5\n}\n").unwrap();

        let mut loaded = load_from(&path);
        loaded.apply_collection(CollectionAction::Add(CollectionId::Keybinds));
        let plan = plan_save(&loaded, ConfigFormat::Conf);
        assert_eq!(plan.mode, SaveMode::Regenerate);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A settings edit on a hand-written Lua config must not regenerate it:
    /// the code, comments and order all survive, and the change lands in a
    /// fenced block at the end that a later save updates in place.
    #[test]
    fn lua_settings_edits_preserve_the_hand_written_file() {
        let dir = temp_dir("lua-preserve");
        let path = dir.join("hyprland.lua");
        let original = "-- my config\nhl.config({ decoration = { rounding = 5 } })\n\
                        for i = 1, 9 do hl.bind(\"SUPER + \" .. i, hl.dsp.focus({ workspace = i })) end\n";
        std::fs::write(&path, original).unwrap();

        let mut loaded = load_from(&path);
        let schema = Schema::shared();
        loaded.apply(
            EditAction::EditText(
                "decoration:rounding".into(),
                crate::edit::Slot::Main,
                "12".into(),
            ),
            schema,
        );
        let plan = plan_save(&loaded, ConfigFormat::Lua);
        assert_eq!(plan.mode, SaveMode::Preserve);
        assert_eq!(plan.drops_dynamic, 0);
        let after = plan.files[0].after.clone();
        assert!(
            after.starts_with(original),
            "hand-written part untouched:\n{after}"
        );
        assert!(
            after.contains(MANAGED_BEGIN) && after.contains("rounding = 12"),
            "{after}"
        );
        perform_save(&plan).unwrap();

        // The written file loads to the edited value, dynamic code intact.
        let mut reloaded = load_from(&path);
        assert_eq!(
            reloaded.config.get("decoration:rounding"),
            Some(&Value::Int(12))
        );
        assert!(reloaded.dynamic_regions >= 1);

        // A second save updates the block in place and keeps earlier edits.
        reloaded.apply(
            EditAction::SetBool("decoration:blur:enabled".into(), false),
            schema,
        );
        let second = plan_save(&reloaded, ConfigFormat::Lua).files[0]
            .after
            .clone();
        assert_eq!(second.matches(MANAGED_BEGIN).count(), 1, "{second}");
        assert!(second.contains("rounding = 12"), "{second}");
        assert!(second.contains("enabled = false"), "{second}");
        assert!(second.starts_with(original));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lua_regeneration_that_drops_code_needs_explicit_consent() {
        let dir = temp_dir("lua-consent");
        let path = dir.join("hyprland.lua");
        std::fs::write(
            &path,
            "for i = 1, 9 do hl.bind(\"SUPER + \" .. i, hl.dsp.focus({ workspace = i })) end\n",
        )
        .unwrap();
        let mut loaded = load_from(&path);
        loaded.apply_collection(CollectionAction::Add(CollectionId::Env));
        let preview = build_preview(&loaded, ConfigFormat::Lua, Schema::shared());
        assert_eq!(preview.plan.mode, SaveMode::Regenerate);
        assert!(
            preview
                .problems
                .iter()
                .any(|p| p.label == "Lua code" && p.severity == Severity::Warning),
            "{:?}",
            preview.problems
        );
        assert!(blocked(&preview.problems, false).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lua_to_conf_warns_about_dropped_dynamic_regions() {
        let dir = temp_dir("dynamic");
        let path = dir.join("hyprland.lua");
        std::fs::write(
            &path,
            "hl.config({ general = { gaps_in = 5 } })\nfor i = 1, 9 do hl.bind(\"SUPER, \" .. i, \"workspace \" .. i) end\n",
        )
        .unwrap();

        let loaded = load_from(&path);
        assert!(
            loaded.dynamic_regions >= 1,
            "fixture should have a dynamic region"
        );

        let plan = plan_save(&loaded, ConfigFormat::Conf);
        assert!(
            plan.drops_dynamic >= 1,
            "converting away from Lua should warn about dynamic loss"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preserve_multi_file_writes_only_the_changed_file() {
        let dir = temp_dir("multi");
        let main = dir.join("hyprland.conf");
        std::fs::write(
            &main,
            "$mod = SUPER\nsource = colors.conf\ndecoration {\n    rounding = 5\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("colors.conf"), "general {\n    gaps_in = 8\n}\n").unwrap();

        let mut loaded = load_from(&main);
        // Edit a scalar that lives in the included file.
        loaded.apply(
            EditAction::EditText(
                "general:gaps_in".into(),
                crate::edit::Slot::Main,
                "10".into(),
            ),
            Schema::shared(),
        );

        let plan = plan_save(&loaded, ConfigFormat::Conf);
        assert_eq!(plan.mode, SaveMode::Preserve);
        let changed = plan.changed_files();
        assert_eq!(changed.len(), 1, "only the included file changed");
        assert!(
            changed[0].path.ends_with("colors.conf"),
            "{:?}",
            changed[0].path
        );
        assert!(changed[0].after.contains("gaps_in = 10"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_conf_parse_of_preserved_output() {
        // The preserved output is itself valid and re-parses.
        let dir = temp_dir("revalidate");
        let path = dir.join("hyprland.conf");
        std::fs::write(&path, "general {\n    gaps_in = 5\n}\n").unwrap();
        let mut loaded = load_from(&path);
        loaded.apply(
            EditAction::EditText(
                "general:gaps_in".into(),
                crate::edit::Slot::Main,
                "9".into(),
            ),
            Schema::shared(),
        );
        let plan = plan_save(&loaded, ConfigFormat::Conf);
        let after = &plan.changed_files()[0].after;
        let doc = ConfParser::parse_str(after, None);
        let (cfg, _) = conf::document_to_config(&doc, Schema::shared());
        assert_eq!(
            cfg.get("general:gaps_in"),
            Some(&Value::CssGap(hyprconf_core::value::CssGap::uniform(9)))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
