// build.rs
//
// Copyright 2023 Christian Hergert <christian@sourceandstack.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Generates build-time assets from the shared C source tree.
//!
//! The GSettings schema is produced from `src/org.gnome.Ptyxis.gschema.xml.in`
//! using the same substitutions as `src/meson.build` (non-development
//! values, so existing user configuration under `/org/gnome/Ptyxis/` is
//! read) and compiled with `glib-compile-schemas` so the binary can run
//! uninstalled.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA_ID: &str = "org.gnome.Ptyxis";
const SCHEMA_PATH: &str = "/org/gnome/Ptyxis/";
const SCHEMA_TEMPLATE: &str = "org.gnome.Ptyxis.gschema.xml.in";

fn c_source_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    manifest_dir.join("..").join("..").join("src")
}

/// Mirrors the `schema_config` substitutions in `src/meson.build`.
fn substitutions() -> [(&'static str, String); 5] {
    [
        ("@APP_SCHEMA_ID@", SCHEMA_ID.to_owned()),
        ("@APP_SCHEMA_PATH@", SCHEMA_PATH.to_owned()),
        ("@APP_SCHEMA_PROFILE_ID@", format!("{SCHEMA_ID}.Profile")),
        (
            "@APP_SCHEMA_SHORTCUTS_ID@",
            format!("{SCHEMA_ID}.Shortcuts"),
        ),
        (
            "@APP_SCHEMA_SHORTCUTS_PATH@",
            format!("{SCHEMA_PATH}Shortcuts/"),
        ),
    ]
}

fn generate_schema(src_dir: &Path, out_dir: &Path) {
    let template_path = src_dir.join(SCHEMA_TEMPLATE);
    println!("cargo::rerun-if-changed={}", template_path.display());

    let template = fs::read_to_string(&template_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", template_path.display()));
    let schema = substitutions()
        .iter()
        .fold(template, |acc, (from, to)| acc.replace(from, to));

    let schema_dir = out_dir.join("schemas");
    fs::create_dir_all(&schema_dir)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", schema_dir.display()));
    let schema_file = schema_dir.join(format!("{SCHEMA_ID}.gschema.xml"));
    fs::write(&schema_file, schema)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", schema_file.display()));

    // A missing glib-compile-schemas is not fatal: an installed schema (or
    // GSETTINGS_SCHEMA_DIR) still works at runtime, so only warn.
    match Command::new("glib-compile-schemas")
        .arg("--strict")
        .arg(&schema_dir)
        .status()
    {
        Ok(status) if status.success() => {
            println!(
                "cargo::rustc-env=PTYXIS_BUILD_SCHEMA_DIR={}",
                schema_dir.display()
            );
        }
        Ok(status) => println!("cargo::warning=glib-compile-schemas failed with {status}"),
        Err(e) => println!(
            "cargo::warning=could not run glib-compile-schemas: {e}; after installing it, \
             run `cargo clean -p ptyxis` so the schema is compiled"
        ),
    }
}

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
    let src_dir = c_source_dir();

    println!("cargo::rerun-if-changed=build.rs");
    generate_schema(&src_dir, &out_dir);
}
