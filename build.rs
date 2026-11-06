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

//! Generates build-time assets from the files copied from Ptyxis.
//!
//! The GSettings schema is produced from `ptyxis/org.gnome.Ptyxis.gschema.xml.in`
//! using the same substitutions as Ptyxis's `src/meson.build` (non-development
//! values, so existing user configuration under `/org/gnome/Ptyxis/` is
//! read) and compiled with `glib-compile-schemas` so the binary can run
//! uninstalled. tty-rex's own schema,
//! `data/io.github.tasleson.TtyRex.gschema.xml`, is compiled into the same
//! directory.
//!
//! The bundled palettes in `ptyxis/palettes/` are embedded into the binary
//! (the C build does the same via GResource), keyed by file stem.
//!
//! The icons in `ptyxis/icons/` are compiled into a GResource at the same
//! paths as in Ptyxis's `src/ptyxis.gresource.xml`, so GTK finds them through the
//! application's resource base path.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Assets copied from the Ptyxis source tree, see `ptyxis/README.md`.
const PTYXIS_ASSETS_DIR: &str = "ptyxis";
const SCHEMA_ID: &str = "org.gnome.Ptyxis";
const SCHEMA_PATH: &str = "/org/gnome/Ptyxis/";
const SCHEMA_TEMPLATE: &str = "org.gnome.Ptyxis.gschema.xml.in";
/// tty-rex's own settings, see `config::TTY_REX_SCHEMA_ID`.
const TTY_REX_DATA_DIR: &str = "data";
const TTY_REX_SCHEMA_FILE: &str = "io.github.tasleson.TtyRex.gschema.xml";
const PALETTES_DIR: &str = "palettes";
const PALETTE_SUFFIX: &str = ".palette";
const ICONS_DIR: &str = "icons";
const RESOURCE_PREFIX: &str = "/org/gnome/Ptyxis";
const RESOURCE_FILE: &str = "ptyxis.gresource";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"))
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

/// Generates the Ptyxis schema, copies tty-rex's next to it and compiles
/// both into `OUT_DIR/schemas`.
fn generate_schemas(src_dir: &Path, data_dir: &Path, out_dir: &Path) {
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

    let tty_rex_schema = data_dir.join(TTY_REX_SCHEMA_FILE);
    println!("cargo::rerun-if-changed={}", tty_rex_schema.display());
    let tty_rex_dest = schema_dir.join(TTY_REX_SCHEMA_FILE);
    fs::copy(&tty_rex_schema, &tty_rex_dest).unwrap_or_else(|e| {
        panic!(
            "failed to copy {} to {}: {e}",
            tty_rex_schema.display(),
            tty_rex_dest.display()
        )
    });

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
             run `cargo clean -p tty-rex` so the schema is compiled"
        ),
    }
}

fn generate_palettes(src_dir: &Path, out_dir: &Path) {
    let palettes_dir = src_dir.join(PALETTES_DIR);
    println!("cargo::rerun-if-changed={}", palettes_dir.display());

    let mut palettes: Vec<(String, PathBuf)> = fs::read_dir(&palettes_dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", palettes_dir.display()))
        .filter_map(|entry| {
            let path = entry.expect("readable palette directory entry").path();
            let name = path.file_name()?.to_str()?;
            let id = name.strip_suffix(PALETTE_SUFFIX)?.to_owned();
            Some((id, path))
        })
        .collect();
    palettes.sort();

    let mut code = String::from("pub static BUILTIN_PALETTES: &[(&str, &str)] = &[\n");
    for (id, path) in &palettes {
        code.push_str(&format!(
            "    ({id:?}, include_str!({:?})),\n",
            path.display()
        ));
    }
    code.push_str("];\n");

    let dest = out_dir.join("builtin_palettes.rs");
    fs::write(&dest, code).unwrap_or_else(|e| panic!("failed to write {}: {e}", dest.display()));
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("readable icon directory entry").path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn generate_resources(src_dir: &Path, out_dir: &Path) {
    let icons_dir = src_dir.join(ICONS_DIR);
    println!("cargo::rerun-if-changed={}", icons_dir.display());

    let mut files = Vec::new();
    collect_files(&icons_dir, &mut files);
    files.sort();

    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"{RESOURCE_PREFIX}\">\n"
    );
    for file in &files {
        let relative = file
            .strip_prefix(src_dir)
            .expect("icon below the source directory");
        // Only XML (SVG) files may be stripped; anything else is copied.
        let preprocess = if file.extension().is_some_and(|ext| ext == "svg") {
            " preprocess=\"xml-stripblanks\""
        } else {
            ""
        };
        xml.push_str(&format!(
            "    <file{preprocess}>{}</file>\n",
            relative.display()
        ));
    }
    xml.push_str("  </gresource>\n</gresources>\n");
    let manifest = out_dir.join("ptyxis.gresource.xml");
    fs::write(&manifest, xml)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", manifest.display()));

    // Like the schema, a missing tool only costs the icons; the binary
    // still builds and embeds an empty (rejected at runtime) resource.
    let target = out_dir.join(RESOURCE_FILE);
    match Command::new("glib-compile-resources")
        .arg("--sourcedir")
        .arg(src_dir)
        .arg("--target")
        .arg(&target)
        .arg(&manifest)
        .status()
    {
        Ok(status) if status.success() => {}
        result => {
            println!(
                "cargo::warning=glib-compile-resources failed ({result:?}); icons will be \
                 missing. After installing it, run `cargo clean -p tty-rex`"
            );
            fs::write(&target, b"")
                .unwrap_or_else(|e| panic!("failed to write {}: {e}", target.display()));
        }
    }
}

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
    let manifest_dir = manifest_dir();
    let src_dir = manifest_dir.join(PTYXIS_ASSETS_DIR);

    println!("cargo::rerun-if-changed=build.rs");
    generate_schemas(&src_dir, &manifest_dir.join(TTY_REX_DATA_DIR), &out_dir);
    generate_palettes(&src_dir, &out_dir);
    generate_resources(&src_dir, &out_dir);
}
