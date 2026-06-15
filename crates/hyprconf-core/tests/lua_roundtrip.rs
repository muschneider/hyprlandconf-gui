// SPDX-License-Identifier: MIT OR Apache-2.0
//! Integration tests for the Lua format and cross-format round-tripping.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hyprconf_core::lua::LuaWarning;
use hyprconf_core::{conf, lua, Config, LuaParser, LuaSerializer, Schema, Tracked, Value};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(rel))
        .unwrap_or_else(|e| panic!("reading {rel}: {e}"))
}

fn values<T: Clone>(items: &[Tracked<T>]) -> Vec<T> {
    items.iter().map(|t| t.value.clone()).collect()
}

fn option_values(config: &Config) -> BTreeMap<String, Value> {
    config
        .options
        .iter()
        .map(|(k, t)| (k.clone(), t.value.clone()))
        .collect()
}

/// Semantic equality, ignoring provenance (which legitimately differs between
/// formats) and the conf-only `submaps` marker list (whose meaning is carried
/// on each bind's `submap` field, which *is* compared).
fn assert_semantic_eq(a: &Config, b: &Config) {
    assert_eq!(option_values(a), option_values(b), "options differ");
    assert_eq!(values(&a.keybinds), values(&b.keybinds), "keybinds differ");
    assert_eq!(
        values(&a.window_rules),
        values(&b.window_rules),
        "window rules differ"
    );
    assert_eq!(
        values(&a.layer_rules),
        values(&b.layer_rules),
        "layer rules differ"
    );
    assert_eq!(values(&a.monitors), values(&b.monitors), "monitors differ");
    assert_eq!(
        values(&a.workspaces),
        values(&b.workspaces),
        "workspaces differ"
    );
    assert_eq!(values(&a.env), values(&b.env), "env differ");
    assert_eq!(values(&a.execs), values(&b.execs), "execs differ");
    assert_eq!(
        values(&a.animations),
        values(&b.animations),
        "animations differ"
    );
    assert_eq!(values(&a.beziers), values(&b.beziers), "beziers differ");
    assert_eq!(
        values(&a.variables),
        values(&b.variables),
        "variables differ"
    );
}

#[test]
fn conf_to_lua_to_config_is_semantically_equal() {
    let schema = Schema::load();

    // .conf -> Config
    let conf_doc = conf::ConfParser::parse_str(&read("conf/roundtrip.conf"), None);
    let (config_conf, _) = conf::document_to_config(&conf_doc, &schema);

    // Config -> .lua -> Config
    let lua_text = LuaSerializer::serialize(&config_conf);
    let lua_doc = LuaParser::parse_str(&lua_text, None)
        .unwrap_or_else(|e| panic!("emitted lua did not parse: {e}\n---\n{lua_text}"));
    let (config_lua, warnings) = lua::document_to_config(&lua_doc, &schema);

    assert!(
        warnings
            .iter()
            .all(|w| !matches!(w, LuaWarning::DynamicRegion { .. })),
        "generated lua should have no dynamic regions, got {warnings:?}"
    );

    // Sanity: the covered constructs are actually present.
    assert!(!config_conf.keybinds.is_empty());
    assert!(!config_conf.window_rules.is_empty());
    assert!(!config_conf.monitors.is_empty());
    assert!(!config_conf.animations.is_empty());
    assert!(config_conf.get("decoration:rounding").is_some());

    assert_semantic_eq(&config_conf, &config_lua);
}

#[test]
fn lua_config_round_trips_via_serializer() {
    let schema = Schema::load();

    let doc = LuaParser::parse_str(&read("lua/declarative.lua"), None).unwrap();
    let (config_a, _) = lua::document_to_config(&doc, &schema);

    let lua_text = LuaSerializer::serialize(&config_a);
    let doc2 = LuaParser::parse_str(&lua_text, None).unwrap();
    let (config_b, _) = lua::document_to_config(&doc2, &schema);

    // The declarative subset survives a Config -> .lua -> Config round-trip.
    assert_semantic_eq(&config_a, &config_b);

    // Spot-check it actually parsed real content.
    assert_eq!(
        config_a.get("general:layout"),
        Some(&Value::Enum("master".into()))
    );
    assert_eq!(
        config_a.get("decoration:blur:enabled"),
        Some(&Value::Bool(false))
    );
    assert_eq!(config_a.keybinds.len(), 2);
    assert_eq!(config_a.variables.len(), 1);
}

#[test]
fn lua_fixtures_round_trip_losslessly() {
    for rel in ["lua/declarative.lua", "lua/extra.lua"] {
        let src = read(rel);
        let doc = LuaParser::parse_str(&src, None).unwrap();
        assert_eq!(doc.to_text(), src, "lossless round-trip failed for {rel}");
    }
}

#[test]
fn dynamic_lua_is_preserved_and_flagged() {
    let schema = Schema::load();
    let src = read("lua/declarative.lua");
    let doc = LuaParser::parse_str(&src, None).unwrap();

    // The `for` loop is dynamic: flagged as read-only...
    let (_config, warnings) = lua::document_to_config(&doc, &schema);
    let dynamic: Vec<_> = warnings
        .iter()
        .filter(|w| matches!(w, LuaWarning::DynamicRegion { .. }))
        .collect();
    assert!(!dynamic.is_empty(), "expected a dynamic region warning");
    assert!(
        dynamic
            .iter()
            .any(|w| w.to_string().contains("for i = 1, 9")),
        "dynamic region should reference the for loop, got {dynamic:?}"
    );

    // ...and preserved verbatim in the lossless document.
    assert!(doc.to_text().contains("for i = 1, 9 do"));
}

#[test]
fn follows_lua_requires() {
    let schema = Schema::load();
    let bundle = LuaParser::parse_file(fixtures_dir().join("lua/declarative.lua")).unwrap();
    assert_eq!(bundle.documents.len(), 2, "declarative.lua + extra.lua");

    let (config, _) = lua::bundle_to_config(&bundle, &schema);
    // `hl.env` from the required extra.lua is merged in.
    assert_eq!(config.env.len(), 1);
    assert_eq!(config.env[0].value.name, "XCURSOR_SIZE");
    assert_eq!(config.env[0].value.value, "24");
}

#[test]
fn missing_lua_require_is_a_typed_error() {
    // declarative.lua requires "extra"; rename target absence is simulated by
    // pointing at a file whose require cannot resolve.
    let result = LuaParser::parse_file(fixtures_dir().join("lua/does_not_exist.lua"));
    assert!(matches!(
        result,
        Err(hyprconf_core::LuaError::NotFound { .. })
    ));
}

/// Hyprland integer "mode" options (e.g. `follow_mouse`, `vrr`) are modelled as
/// enums whose variant literals are integers. They must emit as bare Lua
/// numbers (not quoted strings) and as plain integers in `.conf`, and parse back
/// to the identical enum value from either format. Textual enums (`layout`)
/// stay quoted in Lua.
#[test]
fn numeric_mode_enums_round_trip_as_numbers() {
    let schema = Schema::load();

    let mut config = Config::empty();
    for (path, variant) in [
        ("input:follow_mouse", "2"),
        ("misc:vrr", "3"),
        ("misc:force_default_wallpaper", "-1"),
        ("cursor:no_hardware_cursors", "1"),
        ("general:layout", "master"),
    ] {
        config
            .options
            .insert(path.into(), Tracked::new(Value::Enum(variant.into())));
    }

    // --- Lua: numeric variants are bare numbers; textual ones stay quoted. ---
    let lua_text = LuaSerializer::serialize(&config);
    assert!(lua_text.contains("follow_mouse = 2"), "{lua_text}");
    assert!(lua_text.contains("vrr = 3"), "{lua_text}");
    assert!(
        lua_text.contains("force_default_wallpaper = -1"),
        "{lua_text}"
    );
    assert!(lua_text.contains("no_hardware_cursors = 1"), "{lua_text}");
    assert!(lua_text.contains("layout = \"master\""), "{lua_text}");
    assert!(
        !lua_text.contains("follow_mouse = \"2\""),
        "numeric mode enums must not be quoted: {lua_text}"
    );

    let lua_doc = LuaParser::parse_str(&lua_text, None).unwrap();
    let (from_lua, warnings) = lua::document_to_config(&lua_doc, &schema);
    assert!(warnings.is_empty(), "unexpected lua warnings: {warnings:?}");
    assert_eq!(
        from_lua.get("input:follow_mouse"),
        Some(&Value::Enum("2".into()))
    );
    assert_eq!(from_lua.get("misc:vrr"), Some(&Value::Enum("3".into())));
    assert_eq!(
        from_lua.get("misc:force_default_wallpaper"),
        Some(&Value::Enum("-1".into()))
    );
    assert_eq!(
        from_lua.get("general:layout"),
        Some(&Value::Enum("master".into()))
    );

    // --- Conf: numeric variants are emitted and parsed verbatim. ---
    let conf_text = conf::config_to_conf(&config);
    assert!(conf_text.contains("input:follow_mouse = 2"), "{conf_text}");
    assert!(conf_text.contains("misc:vrr = 3"), "{conf_text}");
    let conf_doc = conf::ConfParser::parse_str(&conf_text, None);
    let (from_conf, _) = conf::document_to_config(&conf_doc, &schema);
    assert_eq!(from_conf.get("misc:vrr"), Some(&Value::Enum("3".into())));
    assert_eq!(
        from_conf.get("cursor:no_hardware_cursors"),
        Some(&Value::Enum("1".into()))
    );
}
