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
use crate::settings::{
    self, AppSettings, CjkAmbiguousWidth, EraseBinding, ExitAction, PreserveContainer,
    PreserveDirectory, SettingsError,
};

/// Keys of the `org.gnome.Ptyxis.Profile` schema used by the port.
pub mod key {
    pub const BACKSPACE_BINDING: &str = "backspace-binding";
    pub const BOLD_IS_BRIGHT: &str = "bold-is-bright";
    pub const CELL_HEIGHT_SCALE: &str = "cell-height-scale";
    pub const CELL_WIDTH_SCALE: &str = "cell-width-scale";
    pub const CJK_AMBIGUOUS_WIDTH: &str = "cjk-ambiguous-width";
    pub const CUSTOM_COMMAND: &str = "custom-command";
    pub const CUSTOM_LINKS: &str = "custom-links";
    pub const DEFAULT_CONTAINER: &str = "default-container";
    pub const DELETE_BINDING: &str = "delete-binding";
    pub const EXIT_ACTION: &str = "exit-action";
    pub const LABEL: &str = "label";
    pub const LIMIT_SCROLLBACK: &str = "limit-scrollback";
    pub const LOGIN_SHELL: &str = "login-shell";
    pub const OPACITY: &str = "opacity";
    pub const PALETTE: &str = "palette";
    pub const PRESERVE_CONTAINER: &str = "preserve-container";
    pub const PRESERVE_DIRECTORY: &str = "preserve-directory";
    pub const SCROLL_ON_KEYSTROKE: &str = "scroll-on-keystroke";
    pub const SCROLL_ON_OUTPUT: &str = "scroll-on-output";
    pub const SCROLLBACK_LINES: &str = "scrollback-lines";
    pub const USE_CUSTOM_COMMAND: &str = "use-custom-command";
    pub const USE_PROXY: &str = "use-proxy";

    /// All keys, for validation against the schema template.
    pub const ALL: &[&str] = &[
        BACKSPACE_BINDING,
        BOLD_IS_BRIGHT,
        CELL_HEIGHT_SCALE,
        CELL_WIDTH_SCALE,
        CJK_AMBIGUOUS_WIDTH,
        CUSTOM_COMMAND,
        CUSTOM_LINKS,
        DEFAULT_CONTAINER,
        DELETE_BINDING,
        EXIT_ACTION,
        LABEL,
        LIMIT_SCROLLBACK,
        LOGIN_SHELL,
        OPACITY,
        PALETTE,
        PRESERVE_CONTAINER,
        PRESERVE_DIRECTORY,
        SCROLL_ON_KEYSTROKE,
        SCROLL_ON_OUTPUT,
        SCROLLBACK_LINES,
        USE_CUSTOM_COMMAND,
        USE_PROXY,
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

/// Appends `uuid` to the profile list unless it is already present, as
/// `ptyxis_settings_add_profile_uuid()` does. Returns `None` when unchanged.
pub fn with_profile_added(uuids: &[String], uuid: &str) -> Option<Vec<String>> {
    if uuids.iter().any(|u| u == uuid) {
        return None;
    }
    let mut uuids = uuids.to_vec();
    uuids.push(uuid.to_owned());
    Some(uuids)
}

/// Result of removing a profile from the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub uuids: Vec<String>,
    /// New default profile, if the removed one was the default.
    pub new_default: Option<String>,
}

/// Removes `uuid` like `ptyxis_settings_remove_profile_uuid()`: the list
/// never becomes empty (a fresh id from `new_uuid` replaces the last
/// profile) and removing the default promotes the first remaining profile.
pub fn with_profile_removed(
    uuids: &[String],
    default_uuid: &str,
    uuid: &str,
    new_uuid: impl FnOnce() -> String,
) -> Removal {
    let mut remaining: Vec<String> = uuids.iter().filter(|u| *u != uuid).cloned().collect();
    if remaining.is_empty() {
        remaining.push(new_uuid());
    }
    let new_default = (uuid == default_uuid).then(|| remaining[0].clone());
    Removal {
        uuids: remaining,
        new_default,
    }
}

/// A user-defined hyperlink pattern (`PtyxisCustomLink`): text matching
/// `pattern` (a PCRE2 regex) links to `target`, which may use `$1`-style
/// references to capture groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomLink {
    pub pattern: String,
    pub target: String,
}

/// Decodes the `custom-links` value (`a(ss)`).
pub fn custom_links_from_variant(value: &glib::Variant) -> Option<Vec<CustomLink>> {
    let pairs: Vec<(String, String)> = value.get()?;
    Some(
        pairs
            .into_iter()
            .map(|(pattern, target)| CustomLink { pattern, target })
            .collect(),
    )
}

/// Encodes links as the `custom-links` value (`a(ss)`).
pub fn custom_links_to_variant(links: &[CustomLink]) -> glib::Variant {
    links
        .iter()
        .map(|link| (link.pattern.clone(), link.target.clone()))
        .collect::<Vec<_>>()
        .to_variant()
}

fn new_profile_uuid() -> String {
    gio::dbus_generate_guid().to_string()
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

    /// Opacity of the window background (`ptyxis_profile_get_opacity()`).
    pub fn opacity(&self) -> f64 {
        self.settings.double(key::OPACITY).clamp(0.0, 1.0)
    }

    pub fn backspace_binding(&self) -> EraseBinding {
        EraseBinding::from_nick_or_default(&self.settings.string(key::BACKSPACE_BINDING))
    }

    pub fn delete_binding(&self) -> EraseBinding {
        EraseBinding::from_nick_or_default(&self.settings.string(key::DELETE_BINDING))
    }

    pub fn cjk_ambiguous_width(&self) -> CjkAmbiguousWidth {
        CjkAmbiguousWidth::from_nick_or_default(&self.settings.string(key::CJK_AMBIGUOUS_WIDTH))
    }

    pub fn custom_links(&self) -> Vec<CustomLink> {
        let value = self.settings.value(key::CUSTOM_LINKS);
        custom_links_from_variant(&value).unwrap_or_else(|| {
            glib::g_warning!(
                "tty-rex",
                "Invalid variant type for custom-links: {}",
                value.type_()
            );
            Vec::new()
        })
    }

    pub fn set_custom_links(&self, links: &[CustomLink]) -> Result<(), glib::BoolError> {
        self.settings
            .set_value(key::CUSTOM_LINKS, &custom_links_to_variant(links))
    }

    pub fn exit_action(&self) -> ExitAction {
        ExitAction::from_nick_or_default(&self.settings.string(key::EXIT_ACTION))
    }

    /// Id of the container new tabs run in. An empty id, which the C
    /// build would fail to find, means the host session.
    pub fn default_container(&self) -> String {
        let id = self.settings.string(key::DEFAULT_CONTAINER);
        if id.is_empty() {
            crate::container::SESSION_ID.to_owned()
        } else {
            id.into()
        }
    }

    pub fn preserve_container(&self) -> PreserveContainer {
        PreserveContainer::from_nick_or_default(&self.settings.string(key::PRESERVE_CONTAINER))
    }

    pub fn preserve_directory(&self) -> PreserveDirectory {
        PreserveDirectory::from_nick_or_default(&self.settings.string(key::PRESERVE_DIRECTORY))
    }

    pub fn use_proxy(&self) -> bool {
        self.settings.boolean(key::USE_PROXY)
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
                        glib::g_warning!("tty-rex", "Failed to open profile {uuid}: {e}")
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

        let uuid = new_profile_uuid();
        let profile = Profile::open(&uuid)?;
        let mut uuids = uuids;
        uuids.push(uuid.clone());
        if let Err(e) = self
            .app_settings
            .set_profile_uuids(&uuids)
            .and_then(|()| self.app_settings.set_default_profile_uuid(&uuid))
        {
            glib::g_warning!("tty-rex", "Failed to save new default profile: {e}");
        }
        Ok(profile)
    }

    pub fn default_uuid(&self) -> String {
        self.app_settings.default_profile_uuid()
    }

    /// The profile actually used as default: the configured one if it is
    /// still listed, otherwise the first (see [`pick_default_uuid`]).
    pub fn effective_default_uuid(&self) -> Option<String> {
        pick_default_uuid(&self.uuids(), &self.default_uuid()).map(str::to_owned)
    }

    /// Adds an existing profile id back to the list (e.g. to undo a removal).
    pub fn add(&self, uuid: &str) -> Result<(), glib::BoolError> {
        match with_profile_added(&self.uuids(), uuid) {
            Some(uuids) => self.app_settings.set_profile_uuids(&uuids),
            None => Ok(()),
        }
    }

    /// Creates and registers a new profile with default settings.
    pub fn create(&self) -> Result<Profile, Box<dyn std::error::Error>> {
        let profile = Profile::open(&new_profile_uuid())?;
        self.add(profile.uuid())?;
        Ok(profile)
    }

    /// Creates a copy of `profile`, copying only explicitly set keys like
    /// `ptyxis_profile_duplicate()`.
    pub fn duplicate(&self, profile: &Profile) -> Result<Profile, Box<dyn std::error::Error>> {
        let copy = Profile::open(&new_profile_uuid())?;
        let source = profile.gsettings();
        if let Some(schema) = source.settings_schema() {
            for key in schema.list_keys() {
                if let Some(value) = source.user_value(&key) {
                    copy.gsettings().set_value(&key, &value)?;
                }
            }
        }
        self.add(copy.uuid())?;
        Ok(copy)
    }

    /// Removes a profile id; its settings stay so the removal can be undone.
    pub fn remove(&self, uuid: &str) -> Result<(), glib::BoolError> {
        let removal =
            with_profile_removed(&self.uuids(), &self.default_uuid(), uuid, new_profile_uuid);
        self.app_settings.set_profile_uuids(&removal.uuids)?;
        if let Some(default) = removal.new_default {
            self.app_settings.set_default_profile_uuid(&default)?;
        }
        Ok(())
    }

    pub fn set_default(&self, uuid: &str) -> Result<(), glib::BoolError> {
        self.app_settings.set_default_profile_uuid(uuid)
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

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn adding_profiles() {
        type Case<'a> = (&'a [&'a str], &'a str, Option<&'a [&'a str]>);
        let cases: &[Case] = &[
            (&["a"], "b", Some(&["a", "b"])),
            (&[], "a", Some(&["a"])),
            (&["a", "b"], "a", None),
        ];
        for (list, uuid, expected) in cases {
            assert_eq!(
                with_profile_added(&ids(list), uuid),
                expected.map(ids),
                "{list:?} + {uuid}"
            );
        }
    }

    #[test]
    fn removing_profiles() {
        // (list, default, removed, expected list, expected new default)
        type Case<'a> = (
            &'a [&'a str],
            &'a str,
            &'a str,
            &'a [&'a str],
            Option<&'a str>,
        );
        let cases: &[Case] = &[
            (&["a", "b", "c"], "a", "b", &["a", "c"], None),
            (&["a", "b", "c"], "b", "b", &["a", "c"], Some("a")),
            (&["a", "b"], "a", "a", &["b"], Some("b")),
            (&["a"], "a", "a", &["fresh"], Some("fresh")),
            (&["a"], "z", "a", &["fresh"], None),
            (&["a", "b"], "a", "missing", &["a", "b"], None),
        ];
        for (list, default, removed, expected, new_default) in cases {
            let removal = with_profile_removed(&ids(list), default, removed, || "fresh".into());
            assert_eq!(removal.uuids, ids(expected), "{list:?} - {removed}");
            assert_eq!(
                removal.new_default.as_deref(),
                *new_default,
                "{list:?} - {removed}"
            );
        }
    }

    /// Exercises the list operations against real (in-memory) settings.
    #[test]
    fn profile_operations_on_settings() {
        if settings::tests::expected_schema(config::SCHEMA_ID).is_none() {
            return;
        }
        settings::use_memory_backend();
        let profiles = Profiles::new(AppSettings::new().unwrap());

        let first = profiles.default_profile().unwrap();
        assert_eq!(profiles.uuids(), [first.uuid()]);
        assert_eq!(profiles.default_uuid(), first.uuid());

        // A stale default still resolves to the profile that is used.
        profiles.set_default("no-longer-listed").unwrap();
        assert_eq!(
            profiles.effective_default_uuid().as_deref(),
            Some(first.uuid())
        );
        profiles.set_default(first.uuid()).unwrap();

        first.gsettings().set_string(key::LABEL, "Work").unwrap();
        let copy = profiles.duplicate(&first).unwrap();
        assert_ne!(copy.uuid(), first.uuid());
        assert_eq!(copy.label(), "Work");
        assert_eq!(profiles.uuids(), [first.uuid(), copy.uuid()]);

        let created = profiles.create().unwrap();
        assert_eq!(created.label(), UNTITLED_PROFILE_LABEL);
        profiles.set_default(created.uuid()).unwrap();
        assert_eq!(profiles.default_uuid(), created.uuid());

        profiles.remove(created.uuid()).unwrap();
        assert_eq!(profiles.uuids(), [first.uuid(), copy.uuid()]);
        assert_eq!(profiles.default_uuid(), first.uuid());

        // Undo keeps the old settings since removal does not reset them.
        profiles.add(copy.uuid()).unwrap();
        profiles.remove(copy.uuid()).unwrap();
        profiles.add(copy.uuid()).unwrap();
        assert_eq!(Profile::open(copy.uuid()).unwrap().label(), "Work");
        assert_eq!(profiles.uuids(), [first.uuid(), copy.uuid()]);

        let links = vec![CustomLink {
            pattern: "x".into(),
            target: "y".into(),
        }];
        first.set_custom_links(&links).unwrap();
        assert_eq!(first.custom_links(), links);
    }

    #[test]
    fn custom_links_round_trip() {
        let links = vec![
            CustomLink {
                pattern: r"BUG-(\d+)".into(),
                target: "https://bugs.example.com/$1".into(),
            },
            CustomLink::default(),
        ];
        let variant = custom_links_to_variant(&links);
        assert_eq!(variant.type_().as_str(), "a(ss)");
        assert_eq!(custom_links_from_variant(&variant), Some(links));
        assert_eq!(
            custom_links_from_variant(&custom_links_to_variant(&[])),
            Some(vec![])
        );
        assert_eq!(custom_links_from_variant(&"nope".to_variant()), None);
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
