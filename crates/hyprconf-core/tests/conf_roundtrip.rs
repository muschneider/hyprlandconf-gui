// SPDX-License-Identifier: MIT OR Apache-2.0
//! Integration tests for the `.conf` parser/serializer/mapper.
//!
//! Covers byte-for-byte round-trips, surgical single-value edits, multi-file
//! `source =` includes, semantic mapping (variables, submaps, windowrulev2,
//! unknown keys), and typed errors for cyclic/missing includes.

use std::path::PathBuf;

use hyprconf_core::conf::{bundle_to_config, document_to_config, ConfWarning, SetOutcome};
use hyprconf_core::{ConfError, ConfParser, Schema, Value};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conf")
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name))
        .unwrap_or_else(|e| panic!("reading fixture {name}: {e}"))
}

/// Indices of lines that differ between two texts (assumes equal line counts).
fn changed_line_indices(a: &str, b: &str) -> Vec<usize> {
    let a: Vec<&str> = a.split('\n').collect();
    let b: Vec<&str> = b.split('\n').collect();
    assert_eq!(a.len(), b.len(), "edit must not change the number of lines");
    a.iter()
        .zip(&b)
        .enumerate()
        .filter_map(|(i, (x, y))| (x != y).then_some(i))
        .collect()
}

#[test]
fn every_fixture_round_trips_byte_for_byte() {
    let dir = fixtures_dir();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("read fixtures dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("conf") {
            continue;
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let doc = ConfParser::parse_str(&content, Some(path.clone()));
        assert_eq!(
            doc.to_text(),
            content,
            "no-edit round-trip is not byte-identical for {}",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "expected several .conf fixtures, found {checked}"
    );
}

#[test]
fn editing_one_value_changes_only_that_line() {
    let original = read_fixture("basic.conf");
    let mut doc = ConfParser::parse_str(&original, None);

    let outcome = doc.set_option("decoration:rounding", "12");
    assert_eq!(outcome, SetOutcome::Edited);

    let edited = doc.to_text();
    let changed = changed_line_indices(&original, &edited);
    assert_eq!(
        changed.len(),
        1,
        "exactly one line should change, got {changed:?}"
    );

    let line = edited.split('\n').nth(changed[0]).unwrap();
    assert!(line.contains("rounding = 12"), "value updated: {line:?}");
    assert!(
        line.contains("# rounded corners"),
        "inline comment preserved: {line:?}"
    );
    assert!(line.starts_with("    "), "indentation preserved: {line:?}");
}

#[test]
fn editing_via_typed_value_matches() {
    let original = read_fixture("basic.conf");
    let mut doc = ConfParser::parse_str(&original, None);
    doc.set_option_value("decoration:rounding", &Value::Int(12));
    assert_eq!(doc.option_value("decoration:rounding"), Some("12"));
}

#[test]
fn inserting_into_existing_section() {
    let original = read_fixture("basic.conf");
    let mut doc = ConfParser::parse_str(&original, None);

    // `general` exists as a block but has no `resize_on_border`.
    let outcome = doc.set_option("general:resize_on_border", "true");
    assert_eq!(outcome, SetOutcome::InsertedInSection);

    // Re-parsing the edited text resolves the new option correctly.
    let doc2 = ConfParser::parse_str(&doc.to_text(), None);
    assert_eq!(doc2.option_value("general:resize_on_border"), Some("true"));
}

#[test]
fn inserting_unknown_section_appends_flat() {
    let original = read_fixture("basic.conf");
    let mut doc = ConfParser::parse_str(&original, None);

    // No `misc {` block exists, so this is appended as a flat key.
    let outcome = doc.set_option("misc:vrr", "1");
    assert_eq!(outcome, SetOutcome::Appended);

    let doc2 = ConfParser::parse_str(&doc.to_text(), None);
    assert_eq!(doc2.option_value("misc:vrr"), Some("1"));
}

#[test]
fn maps_scalars_variables_and_directives() {
    let schema = Schema::load();
    let doc = ConfParser::parse_str(&read_fixture("basic.conf"), None);
    let (config, warnings) = document_to_config(&doc, &schema);

    // scalar options parsed to typed values
    assert_eq!(
        config.get("general:gaps_in"),
        Some(&Value::CssGap(hyprconf_core::value::CssGap::uniform(5)))
    );
    assert_eq!(config.get("decoration:rounding"), Some(&Value::Int(5)));
    // An open choice (custom `lua:<name>` layouts are valid): a string.
    assert_eq!(
        config.get("general:layout"),
        Some(&Value::String("dwindle".into()))
    );
    assert_eq!(
        config.get("decoration:blur:enabled"),
        Some(&Value::Bool(true))
    );
    match config.get("general:col.active_border") {
        Some(Value::Gradient(g)) => {
            assert_eq!(g.stops.len(), 2);
            assert_eq!(g.angle_deg, Some(45.0));
        }
        other => panic!("expected a gradient, got {other:?}"),
    }

    // variable captured and expanded into binds
    assert_eq!(config.variables.len(), 1);
    assert_eq!(config.variables[0].value.name, "mainMod");
    assert_eq!(config.variables[0].value.value, "SUPER");

    let killactive = config
        .keybinds
        .iter()
        .map(|t| &t.value)
        .find(|k| k.dispatcher == "killactive")
        .expect("killactive bind");
    assert_eq!(killactive.mods, "SUPER", "$mainMod expanded");
    assert_eq!(killactive.key, "Q");
    assert_eq!(killactive.submap, None);

    // mouse bind flag derived from `bindm`
    assert!(config.keybinds.iter().any(|t| t.value.flags.mouse));

    // submap association
    let resize = config
        .keybinds
        .iter()
        .map(|t| &t.value)
        .find(|k| k.dispatcher == "resizeactive")
        .expect("resizeactive bind");
    assert_eq!(resize.submap.as_deref(), Some("resize"));
    assert!(resize.flags.repeat, "binde => repeat flag");

    // structured collections
    assert_eq!(config.monitors.len(), 1);
    assert_eq!(config.env.len(), 1);
    assert_eq!(config.execs.len(), 1);
    assert_eq!(config.submaps.len(), 2);
    assert_eq!(config.window_rules.len(), 2);
    assert!(config.window_rules.iter().all(|t| t.value.v2));

    // unknown key preserved as a String value AND surfaced as a warning
    assert_eq!(
        config.get("general:made_up_option"),
        Some(&Value::String("123".into()))
    );
    assert!(warnings.iter().any(|w| matches!(
        w,
        ConfWarning::UnknownOption { path, .. } if path == "general:made_up_option"
    )));
}

#[test]
fn provenance_is_recorded() {
    let schema = Schema::load();
    let doc = ConfParser::parse_str(&read_fixture("basic.conf"), None);
    let (config, _) = document_to_config(&doc, &schema);

    let rounding = config.options.get("decoration:rounding").unwrap();
    assert!(rounding.provenance.line.is_some());
    assert_eq!(
        rounding.provenance.trailing_comment.as_deref(),
        Some("# rounded corners")
    );
}

#[test]
fn follows_multi_file_includes() {
    let schema = Schema::load();
    let bundle = ConfParser::parse_file(fixtures_dir().join("main.conf")).expect("parse bundle");

    // main + colors + binds
    assert_eq!(bundle.documents.len(), 3);

    // every included file still round-trips byte-for-byte
    for doc in &bundle.documents {
        let path = doc.path.as_ref().unwrap();
        let on_disk = std::fs::read_to_string(path).unwrap();
        assert_eq!(doc.to_text(), on_disk, "round-trip for {}", path.display());
    }

    let (config, _warnings) = bundle_to_config(&bundle, &schema);

    // main.conf's `gaps_in = 8` is evaluated after the includes (last wins).
    assert_eq!(
        config.get("general:gaps_in"),
        Some(&Value::CssGap(hyprconf_core::value::CssGap::uniform(8)))
    );

    // colors.conf's `$active` expanded into the border color.
    match config.get("general:col.active_border") {
        Some(Value::Gradient(g)) => assert_eq!(g.stops.len(), 1),
        other => panic!("expected gradient, got {other:?}"),
    }

    // binds.conf used main.conf's `$mainMod` across the file boundary.
    assert_eq!(config.keybinds.len(), 2);
    assert!(config.keybinds.iter().all(|t| t.value.mods == "SUPER"));
}

#[test]
fn cyclic_include_is_a_typed_error() {
    let result = ConfParser::parse_file(fixtures_dir().join("cycle_a.conf"));
    match result {
        Err(ConfError::IncludeCycle { .. }) => {}
        other => panic!("expected IncludeCycle, got {other:?}"),
    }
}

#[test]
fn missing_include_is_a_typed_error() {
    let result = ConfParser::parse_file(fixtures_dir().join("missing.conf"));
    match result {
        Err(ConfError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn missing_root_file_is_a_typed_error() {
    let result = ConfParser::parse_file(fixtures_dir().join("nope_does_not_exist.conf"));
    assert!(matches!(result, Err(ConfError::NotFound { .. })));
}
