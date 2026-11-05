// config.rs
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

//! Build-time identifiers shared with the C implementation (see the
//! toplevel `meson.build` and `src/meson.build`).

/// The application id. Uninstalled builds default to the development id so
/// they never take over a running stable instance; packagers can override it
/// with `PTYXIS_APP_ID` at build time.
pub const APP_ID: &str = match option_env!("PTYXIS_APP_ID") {
    Some(id) => id,
    None => "org.gnome.Ptyxis.Devel",
};

/// Displayed application name.
pub const APP_NAME: &str = "Ptyxis";

/// Version advertised to child processes via `PTYXIS_VERSION`.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The schema id is intentionally the stable (non-development) one so that
/// existing user configuration is read and written.
pub const SCHEMA_ID: &str = "org.gnome.Ptyxis";
pub const SCHEMA_PATH: &str = "/org/gnome/Ptyxis/";
pub const PROFILE_SCHEMA_ID: &str = "org.gnome.Ptyxis.Profile";
pub const SHORTCUTS_SCHEMA_ID: &str = "org.gnome.Ptyxis.Shortcuts";
pub const SHORTCUTS_SCHEMA_PATH: &str = "/org/gnome/Ptyxis/Shortcuts/";

/// Directory containing the schema compiled by `build.rs`, if
/// `glib-compile-schemas` was available at build time.
pub const BUILD_SCHEMA_DIR: Option<&str> = option_env!("PTYXIS_BUILD_SCHEMA_DIR");

/// GSettings path of a relocatable profile schema, matching
/// `ptyxis_profile_constructed()` in `src/ptyxis-profile.c`.
pub fn profile_settings_path(uuid: &str) -> String {
    format!("{SCHEMA_PATH}Profiles/{uuid}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_path_matches_c_layout() {
        assert_eq!(
            profile_settings_path("4f0907c7c61cb3233f5c77246a3d48c1"),
            "/org/gnome/Ptyxis/Profiles/4f0907c7c61cb3233f5c77246a3d48c1/"
        );
    }

    #[test]
    fn shortcuts_path_is_nested_under_app_path() {
        assert!(SHORTCUTS_SCHEMA_PATH.starts_with(SCHEMA_PATH));
        assert!(PROFILE_SCHEMA_ID.starts_with(SCHEMA_ID));
        assert!(SHORTCUTS_SCHEMA_ID.starts_with(SCHEMA_ID));
    }
}
