// SPDX-License-Identifier: MIT OR Apache-2.0
//! Convert a Hyprland config between formats and print the result.
//!
//! Handy for eyeballing conversions and for piping into
//! `Hyprland --verify-config`, which is the real test of whether the output
//! works:
//!
//! ```sh
//! cargo run --example convert -- ~/.config/hypr/hyprland.conf > /tmp/out.lua
//! Hyprland --verify-config -c /tmp/out.lua
//! ```
//!
//! Notes about anything the target format cannot express exactly are printed to
//! stderr, so stdout stays a clean config file.

use std::path::Path;

use hyprconf_core::conf::{bundle_to_config, config_to_conf, ConfParser};
use hyprconf_core::lua::{bundle_to_config as lua_bundle_to_config, LuaParser, LuaSerializer};
use hyprconf_core::schema::Schema;
use hyprconf_core::ConfigFormat;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: convert <config file> [--to lua|conf]");
        std::process::exit(2);
    };

    let path = Path::new(&input);
    let source_format = match path.extension().and_then(|e| e.to_str()) {
        Some("lua") => ConfigFormat::Lua,
        _ => ConfigFormat::Conf,
    };

    // Default to converting to the *other* format, which is the common case.
    let mut target = match source_format {
        ConfigFormat::Conf => ConfigFormat::Lua,
        ConfigFormat::Lua => ConfigFormat::Conf,
    };
    while let Some(arg) = args.next() {
        if arg == "--to" {
            target = match args.next().as_deref() {
                Some("lua") => ConfigFormat::Lua,
                Some("conf") => ConfigFormat::Conf,
                other => {
                    eprintln!("unknown target format: {other:?}");
                    std::process::exit(2);
                }
            };
        }
    }

    let schema = Schema::shared();
    let (config, warnings) = match source_format {
        ConfigFormat::Conf => {
            let bundle = ConfParser::parse_file(path)?;
            let (config, w) = bundle_to_config(&bundle, schema);
            (config, w.len())
        }
        ConfigFormat::Lua => {
            let bundle = LuaParser::parse_file(path)?;
            let (config, w) = lua_bundle_to_config(&bundle, schema);
            (config, w.len())
        }
    };

    match target {
        ConfigFormat::Lua => {
            let out = LuaSerializer::generate(&config);
            print!("{}", out.text);
            for note in &out.notes {
                eprintln!("note [{}] {}: {}", note.kind, note.subject, note.detail);
            }
        }
        ConfigFormat::Conf => print!("{}", config_to_conf(&config)),
    }

    if warnings > 0 {
        eprintln!("{warnings} warning(s) while reading {input}");
    }
    Ok(())
}
