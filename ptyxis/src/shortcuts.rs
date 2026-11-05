// shortcuts.rs
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

//! Keyboard shortcuts configured in `org.gnome.Ptyxis.Shortcuts`
//! (`src/ptyxis-shortcuts.c`).
//!
//! Like the C window, shortcuts are installed as a capture-phase
//! `gtk::ShortcutController` on each window so they take precedence over
//! the terminal, and are rebuilt whenever the settings change.

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::config;
use crate::settings::{self, SettingsError};

/// Keys of the `org.gnome.Ptyxis.Shortcuts` schema used by the port.
pub mod key {
    pub const CLOSE_TAB: &str = "close-tab";
    pub const COPY_CLIPBOARD: &str = "copy-clipboard";
    pub const MOVE_NEXT_TAB: &str = "move-next-tab";
    pub const MOVE_PREVIOUS_TAB: &str = "move-previous-tab";
    pub const NEW_TAB: &str = "new-tab";
    pub const NEW_WINDOW: &str = "new-window";
    pub const PASTE_CLIPBOARD: &str = "paste-clipboard";
    pub const PREFERENCES: &str = "preferences";
    pub const SEARCH: &str = "search";
    pub const ZOOM_IN: &str = "zoom-in";
    pub const ZOOM_ONE: &str = "zoom-one";
    pub const ZOOM_OUT: &str = "zoom-out";
}

/// Shortcut setting key and the detailed window action it activates.
pub const BINDINGS: &[(&str, &str)] = &[
    (key::CLOSE_TAB, "win.close-tab"),
    (key::COPY_CLIPBOARD, "win.copy-clipboard"),
    (key::MOVE_NEXT_TAB, "win.next-tab"),
    (key::MOVE_PREVIOUS_TAB, "win.previous-tab"),
    (key::NEW_TAB, "win.new-tab"),
    (key::NEW_WINDOW, "win.new-window"),
    (key::PASTE_CLIPBOARD, "win.paste-clipboard"),
    (key::PREFERENCES, "win.preferences"),
    (key::SEARCH, "win.search"),
    (key::ZOOM_IN, "win.zoom-in"),
    (key::ZOOM_ONE, "win.zoom-one"),
    (key::ZOOM_OUT, "win.zoom-out"),
];

/// Returns the accelerator to install, or `None` when the user disabled
/// the shortcut (an empty string in the schema).
pub fn effective_accel(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// Shortcut settings plus the controller they populate.
#[derive(Debug)]
pub struct Shortcuts {
    settings: gio::Settings,
    controller: gtk::ShortcutController,
}

impl Shortcuts {
    pub fn new() -> Result<Self, SettingsError> {
        let keys: Vec<&str> = BINDINGS.iter().map(|(key, _)| *key).collect();
        let settings = settings::open(
            config::SHORTCUTS_SCHEMA_ID,
            Some(config::SHORTCUTS_SCHEMA_PATH),
            &keys,
        )?;
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);

        let shortcuts = Self {
            settings,
            controller,
        };
        shortcuts.reload();

        let controller = shortcuts.controller.downgrade();
        shortcuts
            .settings
            .connect_changed(None, move |settings, _| {
                if let Some(controller) = controller.upgrade() {
                    populate(&controller, settings);
                }
            });
        Ok(shortcuts)
    }

    pub fn controller(&self) -> &gtk::ShortcutController {
        &self.controller
    }

    fn reload(&self) {
        populate(&self.controller, &self.settings);
    }
}

fn populate(controller: &gtk::ShortcutController, settings: &gio::Settings) {
    while let Some(shortcut) = controller.item(0).and_downcast::<gtk::Shortcut>() {
        controller.remove_shortcut(&shortcut);
    }

    for (key, action) in BINDINGS {
        let value = settings.string(key);
        let Some(accel) = effective_accel(&value) else {
            continue;
        };
        match gtk::ShortcutTrigger::parse_string(accel) {
            Some(trigger) => controller.add_shortcut(gtk::Shortcut::new(
                Some(trigger),
                Some(gtk::NamedAction::new(action)),
            )),
            None => glib::g_warning!("Ptyxis", "Ignoring invalid shortcut “{accel}” for {key}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::schema_template;
    use crate::window::{
        CloseTabAction, CopyAction, NewTabAction, NewWindowAction, NextTabAction, PasteAction,
        PreferencesAction, PreviousTabAction, SearchAction, ZoomInAction, ZoomOneAction,
        ZoomOutAction,
    };
    use relm4::actions::ActionName;

    #[test]
    fn shortcut_keys_exist_in_schema() {
        for (key, _) in BINDINGS {
            assert!(
                schema_template::has_key("@APP_SCHEMA_SHORTCUTS_ID@", key),
                "{key} missing from shortcuts schema"
            );
        }
    }

    #[test]
    fn bindings_target_window_actions() {
        let expected = [
            (key::CLOSE_TAB, CloseTabAction::action_name()),
            (key::COPY_CLIPBOARD, CopyAction::action_name()),
            (key::MOVE_NEXT_TAB, NextTabAction::action_name()),
            (key::MOVE_PREVIOUS_TAB, PreviousTabAction::action_name()),
            (key::NEW_TAB, NewTabAction::action_name()),
            (key::NEW_WINDOW, NewWindowAction::action_name()),
            (key::PASTE_CLIPBOARD, PasteAction::action_name()),
            (key::PREFERENCES, PreferencesAction::action_name()),
            (key::SEARCH, SearchAction::action_name()),
            (key::ZOOM_IN, ZoomInAction::action_name()),
            (key::ZOOM_ONE, ZoomOneAction::action_name()),
            (key::ZOOM_OUT, ZoomOutAction::action_name()),
        ];
        assert_eq!(BINDINGS.len(), expected.len());
        for ((key, action), (expected_key, expected_action)) in BINDINGS.iter().zip(&expected) {
            assert_eq!(key, expected_key);
            assert_eq!(action, expected_action, "{key}");
        }
    }

    #[test]
    fn disabled_shortcuts() {
        let cases = [
            ("", None),
            ("  ", None),
            ("<ctrl><shift>t", Some("<ctrl><shift>t")),
            (" F11 ", Some("F11")),
        ];
        for (value, expected) in cases {
            assert_eq!(effective_accel(value), expected, "{value:?}");
        }
    }
}
