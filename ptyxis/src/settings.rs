// settings.rs
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

//! GSettings access.
//!
//! `gio::Settings::new()` aborts the process if a schema is missing, so all
//! settings objects are created through [`open`], which resolves the schema
//! explicitly and reports an actionable error instead.

use gtk::gio;
use gtk::glib;

use crate::config;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(
        "GSettings schema \"{0}\" is not installed; install Ptyxis or set \
         GSETTINGS_SCHEMA_DIR to a directory containing the compiled schema"
    )]
    MissingSchema(String),
}

/// Returns the schema source to resolve Ptyxis schemas from.
///
/// When `GSETTINGS_SCHEMA_DIR` is set, the user explicitly chose where
/// schemas come from and the default source is used. Otherwise the schema
/// compiled by `build.rs` takes precedence (with the default source as
/// parent) so an uninstalled build sees exactly the keys it was compiled
/// against even if an older Ptyxis schema is installed system-wide.
fn schema_source() -> Option<gio::SettingsSchemaSource> {
    thread_local! {
        static SOURCE: Option<gio::SettingsSchemaSource> = create_schema_source();
    }
    SOURCE.with(Clone::clone)
}

fn create_schema_source() -> Option<gio::SettingsSchemaSource> {
    let default = gio::SettingsSchemaSource::default();
    let build_dir = config::BUILD_SCHEMA_DIR
        .filter(|_| std::env::var_os("GSETTINGS_SCHEMA_DIR").is_none())
        .filter(|dir| std::path::Path::new(dir).is_dir());

    let Some(dir) = build_dir else {
        return default;
    };
    match gio::SettingsSchemaSource::from_directory(dir, default.as_ref(), true) {
        Ok(source) => Some(source),
        Err(e) => {
            glib::g_warning!("Ptyxis", "Ignoring build schema directory {dir}: {e}");
            default
        }
    }
}

/// Looks up a schema by id, searching parent sources too.
pub fn lookup_schema(schema_id: &str) -> Option<gio::SettingsSchema> {
    schema_source()?.lookup(schema_id, true)
}

/// Opens settings for `schema_id`, at `path` for relocatable schemas.
pub fn open(schema_id: &str, path: Option<&str>) -> Result<gio::Settings, SettingsError> {
    let schema =
        lookup_schema(schema_id).ok_or_else(|| SettingsError::MissingSchema(schema_id.into()))?;
    Ok(gio::Settings::new_full(
        &schema,
        None::<&gio::SettingsBackend>,
        path,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Looks up a schema that must exist whenever `build.rs` compiled one;
    /// returns `None` (so the caller can skip) only when it did not.
    pub(crate) fn expected_schema(schema_id: &str) -> Option<gio::SettingsSchema> {
        let schema = lookup_schema(schema_id);
        if config::BUILD_SCHEMA_DIR.is_some() {
            assert!(
                schema.is_some(),
                "compiled schema {schema_id} should resolve"
            );
        } else if schema.is_none() {
            eprintln!("schema {schema_id} unavailable (no glib-compile-schemas); skipping");
        }
        schema
    }

    #[test]
    fn build_schemas_resolve() {
        for id in [
            config::SCHEMA_ID,
            config::PROFILE_SCHEMA_ID,
            config::SHORTCUTS_SCHEMA_ID,
        ] {
            expected_schema(id);
        }
    }

    #[test]
    fn profile_schema_is_relocatable() {
        if let Some(schema) = expected_schema(config::PROFILE_SCHEMA_ID) {
            assert!(
                schema.path().is_none(),
                "profile schema must be relocatable"
            );
        }
    }

    #[test]
    fn missing_schema_is_an_error() {
        let err = open("org.gnome.Ptyxis.DoesNotExist", None).unwrap_err();
        assert!(err.to_string().contains("org.gnome.Ptyxis.DoesNotExist"));
    }
}
