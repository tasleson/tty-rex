// profile.rs
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

//! Terminal profiles stored in the relocatable `org.gnome.Ptyxis.Profile`
//! schema at `/org/gnome/Ptyxis/Profiles/<uuid>/`.

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::config;
use crate::settings::{self, AppSettings, ExitAction, PreserveDirectory, SettingsError};

/// Keys of the `org.gnome.Ptyxis.Profile` schema used by the port.
pub mod key {
    pub const BOLD_IS_BRIGHT: &str = "bold-is-bright";
    pub const CELL_HEIGHT_SCALE: &str = "cell-height-scale";
    pub const CELL_WIDTH_SCALE: &str = "cell-width-scale";
    pub const CUSTOM_COMMAND: &str = "custom-command";
    pub const EXIT_ACTION: &str = "exit-action";
    pub const LABEL: &str = "label";
    pub const LIMIT_SCROLLBACK: &str = "limit-scrollback";
    pub const LOGIN_SHELL: &str = "login-shell";
    pub const PALETTE: &str = "palette";
    pub const PRESERVE_DIRECTORY: &str = "preserve-directory";
    pub const SCROLL_ON_KEYSTROKE: &str = "scroll-on-keystroke";
    pub const SCROLL_ON_OUTPUT: &str = "scroll-on-output";
    pub const SCROLLBACK_LINES: &str = "scrollback-lines";
    pub const USE_CUSTOM_COMMAND: &str = "use-custom-command";

    /// All keys, for validation against the schema template.
    pub const ALL: &[&str] = &[
        BOLD_IS_BRIGHT,
        CELL_HEIGHT_SCALE,
        CELL_WIDTH_SCALE,
        CUSTOM_COMMAND,
        EXIT_ACTION,
        LABEL,
        LIMIT_SCROLLBACK,
        LOGIN_SHELL,
        PALETTE,
        PRESERVE_DIRECTORY,
        SCROLL_ON_KEYSTROKE,
        SCROLL_ON_OUTPUT,
        SCROLLBACK_LINES,
        USE_CUSTOM_COMMAND,
    ];
}

/// Shown when a profile has no label, as in `ptyxis_profile_dup_label()`.
pub const UNTITLED_PROFILE_LABEL: &str = "Untitled Profile";

/// Scrollback value VTE interprets as "unlimited".
pub const UNLIMITED_SCROLLBACK: i64 = -1;

/// Converts profile scrollback settings to VTE's representation.
pub fn vte_scrollback_lines(limit_scrollback: bool, scrollback_lines: i32) -> i64 {
    if limit_scrollback {
        i64::from(scrollback_lines.max(0))
    } else {
        UNLIMITED_SCROLLBACK
    }
}

/// Returns the label to display for a profile.
pub fn display_label(label: &str) -> &str {
    if label.trim().is_empty() {
        UNTITLED_PROFILE_LABEL
    } else {
        label
    }
}

/// Chooses the default profile UUID the way
/// `ptyxis_application_dup_default_profile()` does: the configured default
/// if it still exists, otherwise the first profile.
pub fn pick_default_uuid<'a>(uuids: &'a [String], default_uuid: &str) -> Option<&'a str> {
    uuids
        .iter()
        .find(|uuid| *uuid == default_uuid)
        .or_else(|| uuids.first())
        .map(String::as_str)
}

/// A single profile backed by its own relocatable settings object.
#[derive(Debug, Clone)]
pub struct Profile {
    uuid: String,
    settings: gio::Settings,
}

impl Profile {
    pub fn open(uuid: &str) -> Result<Self, SettingsError> {
        let path = config::profile_settings_path(uuid);
        Ok(Self {
            uuid: uuid.to_owned(),
            settings: settings::open(config::PROFILE_SCHEMA_ID, Some(&path), key::ALL)?,
        })
    }

    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// The underlying settings, for connecting change notifications.
    pub fn gsettings(&self) -> &gio::Settings {
        &self.settings
    }

    pub fn label(&self) -> String {
        display_label(&self.settings.string(key::LABEL)).to_owned()
    }

    pub fn palette_id(&self) -> String {
        self.settings.string(key::PALETTE).into()
    }

    pub fn scrollback_lines(&self) -> i64 {
        vte_scrollback_lines(
            self.settings.boolean(key::LIMIT_SCROLLBACK),
            self.settings.int(key::SCROLLBACK_LINES),
        )
    }

    pub fn bold_is_bright(&self) -> bool {
        self.settings.boolean(key::BOLD_IS_BRIGHT)
    }

    pub fn scroll_on_output(&self) -> bool {
        self.settings.boolean(key::SCROLL_ON_OUTPUT)
    }

    pub fn scroll_on_keystroke(&self) -> bool {
        self.settings.boolean(key::SCROLL_ON_KEYSTROKE)
    }

    pub fn cell_height_scale(&self) -> f64 {
        self.settings.double(key::CELL_HEIGHT_SCALE)
    }

    pub fn cell_width_scale(&self) -> f64 {
        self.settings.double(key::CELL_WIDTH_SCALE)
    }

    pub fn exit_action(&self) -> ExitAction {
        ExitAction::from_nick_or_default(&self.settings.string(key::EXIT_ACTION))
    }

    pub fn preserve_directory(&self) -> PreserveDirectory {
        PreserveDirectory::from_nick_or_default(&self.settings.string(key::PRESERVE_DIRECTORY))
    }

    pub fn login_shell(&self) -> bool {
        self.settings.boolean(key::LOGIN_SHELL)
    }

    /// The custom command, if the profile enables one.
    pub fn custom_command(&self) -> Option<String> {
        self.settings
            .boolean(key::USE_CUSTOM_COMMAND)
            .then(|| self.settings.string(key::CUSTOM_COMMAND).to_string())
    }
}

/// Resolves profiles from the application settings.
#[derive(Debug, Clone)]
pub struct Profiles {
    app_settings: AppSettings,
}

impl Profiles {
    pub fn new(app_settings: AppSettings) -> Self {
        Self { app_settings }
    }

    pub fn uuids(&self) -> Vec<String> {
        self.app_settings.profile_uuids()
    }

    /// Opens every configured profile, skipping (and logging) failures.
    pub fn list(&self) -> Vec<Profile> {
        self.uuids()
            .iter()
            .filter_map(|uuid| {
                Profile::open(uuid)
                    .inspect_err(|e| {
                        glib::g_warning!("Ptyxis", "Failed to open profile {uuid}: {e}")
                    })
                    .ok()
            })
            .collect()
    }

    /// Returns the default profile, creating one if none exist yet just as
    /// the C implementation does on first run.
    pub fn default_profile(&self) -> Result<Profile, SettingsError> {
        let uuids = self.uuids();
        let default_uuid = self.app_settings.default_profile_uuid();

        if let Some(uuid) = pick_default_uuid(&uuids, &default_uuid) {
            return Profile::open(uuid);
        }

        let uuid = gio::dbus_generate_guid().to_string();
        let profile = Profile::open(&uuid)?;
        let mut uuids = uuids;
        uuids.push(uuid.clone());
        if let Err(e) = self
            .app_settings
            .set_profile_uuids(&uuids)
            .and_then(|()| self.app_settings.set_default_profile_uuid(&uuid))
        {
            glib::g_warning!("Ptyxis", "Failed to save new default profile: {e}");
        }
        Ok(profile)
    }

    /// Returns the profile with `uuid`, falling back to the default profile
    /// for empty or stale ids (as `ptyxis_application_dup_profile()` does).
    pub fn profile(&self, uuid: Option<&str>) -> Result<Profile, SettingsError> {
        match uuid {
            Some(uuid) if !uuid.is_empty() && self.uuids().iter().any(|u| u == uuid) => {
                Profile::open(uuid)
            }
            _ => self.default_profile(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::schema_template;

    #[test]
    fn profile_keys_exist_in_schema() {
        for key in key::ALL {
            assert!(
                schema_template::has_key("@APP_SCHEMA_PROFILE_ID@", key),
                "{key} missing from profile schema"
            );
        }
    }

    #[test]
    fn scrollback_conversion() {
        let cases = [
            (true, 10_000, 10_000),
            (true, 0, 0),
            (true, -5, 0),
            (false, 10_000, UNLIMITED_SCROLLBACK),
        ];
        for (limit, lines, expected) in cases {
            assert_eq!(vte_scrollback_lines(limit, lines), expected);
        }
    }

    #[test]
    fn label_fallback() {
        let cases = [
            ("", UNTITLED_PROFILE_LABEL),
            ("   ", UNTITLED_PROFILE_LABEL),
            ("Work", "Work"),
        ];
        for (label, expected) in cases {
            assert_eq!(display_label(label), expected);
        }
    }

    #[test]
    fn default_uuid_selection() {
        let uuids: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        let cases: &[(&[String], &str, Option<&str>)] = &[
            (&uuids, "b", Some("b")),
            (&uuids, "missing", Some("a")),
            (&uuids, "", Some("a")),
            (&[], "b", None),
        ];
        for (uuids, default, expected) in cases {
            assert_eq!(pick_default_uuid(uuids, default), *expected);
        }
    }
}
