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

/// The application id, distinct from Ptyxis's so the two never hand
/// launches to each other. Override it with `TTY_REX_APP_ID` at build time,
/// e.g. with an id ending in `.Devel` for development builds.
pub const APP_ID: &str = match option_env!("TTY_REX_APP_ID") {
    Some(id) => id,
    None => "io.github.tasleson.TtyRex",
};

/// Displayed application name and program name.
pub const APP_NAME: &str = "tty-rex";

/// Project home page and issue tracker, shown in the About dialog.
pub const WEBSITE: &str = "https://github.com/tasleson/tty-rex";
pub const ISSUE_URL: &str = "https://github.com/tasleson/tty-rex/issues";

/// Version advertised to child processes via `PTYXIS_VERSION`.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `git describe` at build time, or [`PACKAGE_VERSION`] outside a tagged
/// checkout (`PTYXIS_BUILD_IDENTIFIER`, set by `build.rs`).
pub const BUILD_IDENTIFIER: &str = env!("TTY_REX_BUILD_IDENTIFIER");

/// Library versions tty-rex was built against, as pkg-config reported them
/// to `build.rs` ("unknown" if it could not). The C build takes these from
/// the `GLIB_*_VERSION`, `GTK_*_VERSION` and `VTE_*_VERSION` macros.
pub const BUILD_GLIB_VERSION: &str = env!("TTY_REX_BUILD_GLIB_VERSION");
pub const BUILD_GTK_VERSION: &str = env!("TTY_REX_BUILD_GTK_VERSION");
pub const BUILD_VTE_VERSION: &str = env!("TTY_REX_BUILD_VTE_VERSION");

/// Whether this is a development build, which Ptyxis decides with its
/// `development` meson option (`DEVELOPMENT_BUILD`) and tty-rex with a
/// `.Devel` application id.
pub fn is_development_build() -> bool {
    APP_ID.ends_with(".Devel")
}

/// The schema id is intentionally the stable (non-development) one so that
/// existing user configuration is read and written.
pub const SCHEMA_ID: &str = "org.gnome.Ptyxis";
pub const SCHEMA_PATH: &str = "/org/gnome/Ptyxis/";
pub const PROFILE_SCHEMA_ID: &str = "org.gnome.Ptyxis.Profile";
pub const SHORTCUTS_SCHEMA_ID: &str = "org.gnome.Ptyxis.Shortcuts";
pub const SHORTCUTS_SCHEMA_PATH: &str = "/org/gnome/Ptyxis/Shortcuts/";

/// Settings tty-rex adds on top of Ptyxis's
/// (`data/io.github.tasleson.TtyRex.gschema.xml`). Like [`SCHEMA_ID`] this
/// does not follow a `TTY_REX_APP_ID` override, so every build shares them.
pub const TTY_REX_SCHEMA_ID: &str = "io.github.tasleson.TtyRex";
pub const TTY_REX_SCHEMA_PATH: &str = "/io/github/tasleson/TtyRex/";
/// Shortcuts for tty-rex's own actions, laid out like [`SHORTCUTS_SCHEMA_ID`].
pub const TTY_REX_SHORTCUTS_SCHEMA_ID: &str = "io.github.tasleson.TtyRex.Shortcuts";
pub const TTY_REX_SHORTCUTS_SCHEMA_PATH: &str = "/io/github/tasleson/TtyRex/Shortcuts/";

/// Resource base path, matching the C application's.
pub const RESOURCE_BASE_PATH: &str = "/org/gnome/Ptyxis";

/// Directory containing the schemas compiled by `build.rs`, if
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

    #[test]
    fn tty_rex_paths_match_ids() {
        for (id, path) in [
            (TTY_REX_SCHEMA_ID, TTY_REX_SCHEMA_PATH),
            (TTY_REX_SHORTCUTS_SCHEMA_ID, TTY_REX_SHORTCUTS_SCHEMA_PATH),
        ] {
            assert_eq!(path, format!("/{}/", id.replace('.', "/")));
        }
        assert!(TTY_REX_SHORTCUTS_SCHEMA_PATH.starts_with(TTY_REX_SCHEMA_PATH));
    }
}
