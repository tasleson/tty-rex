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
//! Shortcuts for actions only tty-rex has ([`TTY_REX_KEYS`]) live in the
//! same format in tty-rex's own `io.github.tasleson.TtyRex.Shortcuts`
//! schema, since the Ptyxis schema is used unchanged;
//! [`ShortcutSettings::for_key`] picks the right one.
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
    pub const CLOSE_OTHER_TABS: &str = "close-other-tabs";
    pub const CLOSE_TAB: &str = "close-tab";
    pub const COPY_CLIPBOARD: &str = "copy-clipboard";
    pub const DETACH_TAB: &str = "detach-tab";
    pub const MOVE_NEXT_TAB: &str = "move-next-tab";
    pub const MOVE_PREVIOUS_TAB: &str = "move-previous-tab";
    pub const MOVE_TAB_LEFT: &str = "move-tab-left";
    pub const MOVE_TAB_RIGHT: &str = "move-tab-right";
    pub const NEW_TAB: &str = "new-tab";
    pub const NEW_WINDOW: &str = "new-window";
    pub const PASTE_CLIPBOARD: &str = "paste-clipboard";
    pub const PREFERENCES: &str = "preferences";
    pub const SHOW_KEYBOARD_SHORTCUTS: &str = "show-keyboard-shortcuts";
    pub const SEARCH: &str = "search";
    pub const SET_TITLE: &str = "set-title";
    /// In tty-rex's own schema (see [`super::TTY_REX_KEYS`]).
    pub const TOGGLE_TELEMETRY: &str = "toggle-telemetry";
    pub const TAB_OVERVIEW: &str = "tab-overview";
    pub const ZOOM_IN: &str = "zoom-in";
    pub const ZOOM_ONE: &str = "zoom-one";
    pub const ZOOM_OUT: &str = "zoom-out";
}

/// Shortcut setting key and the detailed window action it activates.
pub const BINDINGS: &[(&str, &str)] = &[
    (key::CLOSE_OTHER_TABS, "win.close-other-tabs"),
    (key::CLOSE_TAB, "win.close-tab"),
    (key::COPY_CLIPBOARD, "win.copy-clipboard"),
    (key::DETACH_TAB, "win.detach-tab"),
    (key::MOVE_NEXT_TAB, "win.next-tab"),
    (key::MOVE_PREVIOUS_TAB, "win.previous-tab"),
    (key::MOVE_TAB_LEFT, "win.move-tab-left"),
    (key::MOVE_TAB_RIGHT, "win.move-tab-right"),
    (key::NEW_TAB, "win.new-tab"),
    (key::NEW_WINDOW, "win.new-window"),
    (key::PASTE_CLIPBOARD, "win.paste-clipboard"),
    (key::PREFERENCES, "win.preferences"),
    (key::SHOW_KEYBOARD_SHORTCUTS, "win.show-keyboard-shortcuts"),
    (key::SEARCH, "win.search"),
    (key::SET_TITLE, "win.set-title"),
    (key::TOGGLE_TELEMETRY, "win.toggle-telemetry"),
    (key::TAB_OVERVIEW, "win.tab-overview"),
    (key::ZOOM_IN, "win.zoom-in"),
    (key::ZOOM_ONE, "win.zoom-one"),
    (key::ZOOM_OUT, "win.zoom-out"),
];

/// Every key of the shortcuts schema, as in `src/ptyxis-shortcuts.defs`.
pub const ALL_KEYS: &[&str] = &[
    "close-other-tabs",
    "close-tab",
    "close-window",
    "copy-clipboard",
    "detach-tab",
    "focus-tab-1",
    "focus-tab-10",
    "focus-tab-2",
    "focus-tab-3",
    "focus-tab-4",
    "focus-tab-5",
    "focus-tab-6",
    "focus-tab-7",
    "focus-tab-8",
    "focus-tab-9",
    "focus-tab-last",
    "move-next-tab",
    "move-previous-tab",
    "move-tab-left",
    "move-tab-right",
    "new-tab",
    "new-window",
    "paste-clipboard",
    "popup-menu",
    "primary-menu",
    "tab-menu",
    "preferences",
    "reset",
    "reset-and-clear",
    "search",
    "select-all",
    "select-none",
    "tab-overview",
    "toggle-fullscreen",
    "undo-close-tab",
    "zoom-in",
    "zoom-one",
    "zoom-out",
    "set-title",
    "show-keyboard-shortcuts",
];

/// Every key of tty-rex's own shortcuts schema,
/// [`config::TTY_REX_SHORTCUTS_SCHEMA_ID`].
pub const TTY_REX_KEYS: &[&str] = &[key::TOGGLE_TELEMETRY];

/// The schema id and path holding the shortcut `key`.
fn schema_of(key: &str) -> (&'static str, &'static str) {
    if TTY_REX_KEYS.contains(&key) {
        (
            config::TTY_REX_SHORTCUTS_SCHEMA_ID,
            config::TTY_REX_SHORTCUTS_SCHEMA_PATH,
        )
    } else {
        (config::SHORTCUTS_SCHEMA_ID, config::SHORTCUTS_SCHEMA_PATH)
    }
}

/// A row of the Shortcuts preferences page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutEntry {
    pub key: &'static str,
    pub title: &'static str,
    pub subtitle: Option<&'static str>,
}

const fn entry(key: &'static str, title: &'static str) -> ShortcutEntry {
    ShortcutEntry {
        key,
        title,
        subtitle: None,
    }
}

const fn described(
    key: &'static str,
    title: &'static str,
    subtitle: &'static str,
) -> ShortcutEntry {
    ShortcutEntry {
        key,
        title,
        subtitle: Some(subtitle),
    }
}

/// A group on the Shortcuts page: optional title and its rows.
pub type ShortcutGroup = (Option<&'static str>, &'static [ShortcutEntry]);

/// The Shortcuts page layout from `src/ptyxis-preferences-window.ui`.
pub const EDITOR_GROUPS: &[ShortcutGroup] = &[
    (
        Some("Terminal"),
        &[
            entry("copy-clipboard", "Copy to Clipboard"),
            entry("paste-clipboard", "Paste from Clipboard"),
        ],
    ),
    (
        None,
        &[
            entry("select-all", "Select All"),
            entry("select-none", "Clear Selection"),
        ],
    ),
    (
        None,
        &[
            entry("reset", "Reset"),
            entry("reset-and-clear", "Reset and Clear"),
        ],
    ),
    (
        None,
        &[
            entry("search", "Search History"),
            described(
                key::TOGGLE_TELEMETRY,
                "Show Telemetry",
                "Toggle network, CPU and memory readings for the current tab",
            ),
        ],
    ),
    (None, &[entry("popup-menu", "Show Context Menu")]),
    (
        Some("Windows"),
        &[
            described("new-window", "New Window", "Open a new tab in a new window"),
            described("close-window", "Close Window", "Close the current window"),
        ],
    ),
    (None, &[entry("set-title", "Set Title")]),
    (
        Some("Tabs"),
        &[described(
            "new-tab",
            "New Tab",
            "Open a new tab in the current window",
        )],
    ),
    (
        None,
        &[described(
            "tab-overview",
            "View Tab Overview",
            "Show an overview of all tabs within the window",
        )],
    ),
    (
        None,
        &[
            described("close-tab", "Close Tab", "Close the current tab"),
            described(
                "close-other-tabs",
                "Close Other Tabs",
                "Close all tabs except the active tab",
            ),
            described(
                "undo-close-tab",
                "Undo Close Tab",
                "Undo closing the last tab",
            ),
        ],
    ),
    (
        None,
        &[
            entry("move-previous-tab", "Move to Previous Tab"),
            entry("move-next-tab", "Move to Next Tab"),
        ],
    ),
    (
        None,
        &[
            described(
                "move-tab-left",
                "Move Tab Left",
                "Move the tab one position to the left",
            ),
            described(
                "move-tab-right",
                "Move Tab Right",
                "Move the tab one position to the right",
            ),
            described(
                "detach-tab",
                "Move Tab to New Window",
                "Detach the current tab and move it to a new window",
            ),
        ],
    ),
    (
        None,
        &[
            entry("focus-tab-1", "Switch to Tab 1"),
            entry("focus-tab-2", "Switch to Tab 2"),
            entry("focus-tab-3", "Switch to Tab 3"),
            entry("focus-tab-4", "Switch to Tab 4"),
            entry("focus-tab-5", "Switch to Tab 5"),
            entry("focus-tab-6", "Switch to Tab 6"),
            entry("focus-tab-7", "Switch to Tab 7"),
            entry("focus-tab-8", "Switch to Tab 8"),
            entry("focus-tab-9", "Switch to Tab 9"),
            entry("focus-tab-10", "Switch to Tab 10"),
            entry("focus-tab-last", "Switch to Last Tab"),
        ],
    ),
    (
        Some("Zoom"),
        &[
            entry("zoom-in", "Zoom In"),
            entry("zoom-out", "Zoom Out"),
            entry("zoom-one", "Reset Zoom"),
        ],
    ),
    (
        Some("Application"),
        &[described(
            "toggle-fullscreen",
            "Toggle Fullscreen",
            "Toggle in and out of fullscreen",
        )],
    ),
    (
        None,
        &[
            described(
                "primary-menu",
                "Show Primary Menu",
                "Displays the primary menu for the terminal window",
            ),
            described(
                "tab-menu",
                "Show New Tab Menu",
                "Displays the new tab menu for the terminal window",
            ),
            described("preferences", "Preferences", "Show this preferences window"),
            described(
                "show-keyboard-shortcuts",
                "Keyboard Shortcuts",
                "Show the keyboard shortcuts configuration",
            ),
        ],
    ),
];

/// The schema default for a shortcut (`ptyxis_shortcuts_get_default_accelerator()`).
pub fn default_accelerator(key: &str) -> Option<String> {
    settings::lookup_schema(schema_of(key).0)
        .filter(|schema| schema.has_key(key))?
        .key(key)
        .default_value()
        .str()
        .map(str::to_owned)
}

/// The Ptyxis shortcut settings and tty-rex's own.
#[derive(Debug, Clone)]
pub struct ShortcutSettings {
    ptyxis: gio::Settings,
    tty_rex: gio::Settings,
}

impl ShortcutSettings {
    /// The settings holding the shortcut `key`.
    pub fn for_key(&self, key: &str) -> &gio::Settings {
        if schema_of(key).0 == config::TTY_REX_SHORTCUTS_SCHEMA_ID {
            &self.tty_rex
        } else {
            &self.ptyxis
        }
    }

    fn all(&self) -> [&gio::Settings; 2] {
        [&self.ptyxis, &self.tty_rex]
    }
}

/// Opens the shortcut settings without installing a controller, for the
/// preferences editor.
pub fn open_settings() -> Result<ShortcutSettings, SettingsError> {
    Ok(ShortcutSettings {
        ptyxis: settings::open(
            config::SHORTCUTS_SCHEMA_ID,
            Some(config::SHORTCUTS_SCHEMA_PATH),
            ALL_KEYS,
        )?,
        tty_rex: settings::open(
            config::TTY_REX_SHORTCUTS_SCHEMA_ID,
            Some(config::TTY_REX_SHORTCUTS_SCHEMA_PATH),
            TTY_REX_KEYS,
        )?,
    })
}

/// Returns the accelerator to install, or `None` when the user disabled
/// the shortcut (an empty string in the schema).
pub fn effective_accel(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// Shortcut settings plus the controller they populate.
#[derive(Debug)]
pub struct Shortcuts {
    settings: ShortcutSettings,
    controller: gtk::ShortcutController,
}

impl Shortcuts {
    pub fn new() -> Result<Self, SettingsError> {
        let settings = open_settings()?;
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);

        let shortcuts = Self {
            settings,
            controller,
        };
        shortcuts.reload();

        // Weak references: the handlers live as long as the settings.
        let ptyxis = shortcuts.settings.ptyxis.downgrade();
        let tty_rex = shortcuts.settings.tty_rex.downgrade();
        for settings in shortcuts.settings.all() {
            let controller = shortcuts.controller.downgrade();
            let (ptyxis, tty_rex) = (ptyxis.clone(), tty_rex.clone());
            settings.connect_changed(None, move |_, _| {
                if let (Some(controller), Some(ptyxis), Some(tty_rex)) =
                    (controller.upgrade(), ptyxis.upgrade(), tty_rex.upgrade())
                {
                    populate(&controller, &ShortcutSettings { ptyxis, tty_rex });
                }
            });
        }
        Ok(shortcuts)
    }

    pub fn controller(&self) -> &gtk::ShortcutController {
        &self.controller
    }

    fn reload(&self) {
        populate(&self.controller, &self.settings);
    }
}

fn populate(controller: &gtk::ShortcutController, settings: &ShortcutSettings) {
    while let Some(shortcut) = controller.item(0).and_downcast::<gtk::Shortcut>() {
        controller.remove_shortcut(&shortcut);
    }

    for (key, action) in BINDINGS {
        let value = settings.for_key(key).string(key);
        let Some(accel) = effective_accel(&value) else {
            continue;
        };
        match gtk::ShortcutTrigger::parse_string(accel) {
            Some(trigger) => controller.add_shortcut(gtk::Shortcut::new(
                Some(trigger),
                Some(gtk::NamedAction::new(action)),
            )),
            None => glib::g_warning!("tty-rex", "Ignoring invalid shortcut “{accel}” for {key}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::schema_template;
    use crate::window::{
        CloseOtherTabsAction, CloseTabAction, CopyAction, DetachTabAction, MoveTabLeftAction,
        MoveTabRightAction, NewTabAction, NewWindowAction, NextTabAction, PasteAction,
        PreferencesAction, PreviousTabAction, SearchAction, SetTitleAction,
        ShowKeyboardShortcutsAction, TabOverviewAction, ToggleTelemetryAction, ZoomInAction,
        ZoomOneAction, ZoomOutAction,
    };
    use relm4::actions::ActionName;

    const TTY_REX_SCHEMA: &str = include_str!("../data/io.github.tasleson.TtyRex.gschema.xml");

    /// The `<schema>` element of tty-rex's shortcuts schema.
    fn tty_rex_shortcuts_block() -> &'static str {
        let start = TTY_REX_SCHEMA
            .find(&format!(
                "<schema id=\"{}\" path=\"{}\">",
                config::TTY_REX_SHORTCUTS_SCHEMA_ID,
                config::TTY_REX_SHORTCUTS_SCHEMA_PATH
            ))
            .expect("tty-rex shortcuts schema");
        let block = &TTY_REX_SCHEMA[start..];
        &block[..block.find("</schema>").unwrap()]
    }

    #[test]
    fn shortcut_keys_exist_in_schema() {
        for (key, _) in BINDINGS {
            let found = if TTY_REX_KEYS.contains(key) {
                tty_rex_shortcuts_block().contains(&format!("<key name=\"{key}\" type=\"s\">"))
            } else {
                schema_template::has_key("@APP_SCHEMA_SHORTCUTS_ID@", key)
            };
            assert!(found, "{key} missing from its shortcuts schema");
        }
    }

    #[test]
    fn tty_rex_keys_match_schema() {
        let block = tty_rex_shortcuts_block();
        assert_eq!(block.matches("<key name=").count(), TTY_REX_KEYS.len());
        for key in TTY_REX_KEYS {
            assert!(!ALL_KEYS.contains(key), "{key} shadows a Ptyxis shortcut");
        }
    }

    #[test]
    fn bindings_target_window_actions() {
        let expected = [
            (key::CLOSE_OTHER_TABS, CloseOtherTabsAction::action_name()),
            (key::CLOSE_TAB, CloseTabAction::action_name()),
            (key::COPY_CLIPBOARD, CopyAction::action_name()),
            (key::DETACH_TAB, DetachTabAction::action_name()),
            (key::MOVE_NEXT_TAB, NextTabAction::action_name()),
            (key::MOVE_PREVIOUS_TAB, PreviousTabAction::action_name()),
            (key::MOVE_TAB_LEFT, MoveTabLeftAction::action_name()),
            (key::MOVE_TAB_RIGHT, MoveTabRightAction::action_name()),
            (key::NEW_TAB, NewTabAction::action_name()),
            (key::NEW_WINDOW, NewWindowAction::action_name()),
            (key::PASTE_CLIPBOARD, PasteAction::action_name()),
            (key::PREFERENCES, PreferencesAction::action_name()),
            (
                key::SHOW_KEYBOARD_SHORTCUTS,
                ShowKeyboardShortcutsAction::action_name(),
            ),
            (key::SEARCH, SearchAction::action_name()),
            (key::SET_TITLE, SetTitleAction::action_name()),
            (key::TOGGLE_TELEMETRY, ToggleTelemetryAction::action_name()),
            (key::TAB_OVERVIEW, TabOverviewAction::action_name()),
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
    fn all_keys_match_schema() {
        let block = schema_template::schema_block("@APP_SCHEMA_SHORTCUTS_ID@");
        assert_eq!(block.matches("<key name=").count(), ALL_KEYS.len());
        for key in ALL_KEYS {
            assert!(
                schema_template::has_key("@APP_SCHEMA_SHORTCUTS_ID@", key),
                "{key}"
            );
        }
    }

    #[test]
    fn editor_lists_every_shortcut_once() {
        let mut listed: Vec<&str> = EDITOR_GROUPS
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|row| row.key))
            .collect();
        listed.sort_unstable();
        let mut all = [ALL_KEYS, TTY_REX_KEYS].concat();
        all.sort_unstable();
        assert_eq!(listed, all);
    }

    #[test]
    fn defaults_come_from_schema() {
        if settings::tests::expected_schema(config::SHORTCUTS_SCHEMA_ID).is_none() {
            return;
        }
        let cases = [
            ("new-tab", Some("<ctrl><shift>t")),
            ("toggle-fullscreen", Some("F11")),
            ("reset", Some("")),
        ];
        for (key, expected) in cases {
            assert_eq!(default_accelerator(key).as_deref(), expected, "{key}");
        }
    }

    #[test]
    fn tty_rex_defaults_do_not_collide() {
        if settings::tests::expected_schema(config::SHORTCUTS_SCHEMA_ID).is_none()
            || settings::tests::expected_schema(config::TTY_REX_SHORTCUTS_SCHEMA_ID).is_none()
        {
            return;
        }
        assert_eq!(
            default_accelerator(key::TOGGLE_TELEMETRY).as_deref(),
            Some("<ctrl><shift>m")
        );
        // Parsing accelerators needs GTK initialized, so compare the
        // schema strings, which spell modifiers the same way.
        let normalized = |key: &str| {
            default_accelerator(key)
                .as_deref()
                .and_then(effective_accel)
                .map(|accel| accel.to_ascii_lowercase().replace("<control>", "<ctrl>"))
        };
        for key in TTY_REX_KEYS {
            let ours = normalized(key).expect("tty-rex shortcuts have a default");
            for other in ALL_KEYS.iter().chain(TTY_REX_KEYS).filter(|k| *k != key) {
                assert_ne!(
                    normalized(other).as_ref(),
                    Some(&ours),
                    "{key} collides with {other}"
                );
            }
        }
    }

    #[test]
    fn keys_map_to_their_schema() {
        if settings::tests::expected_schema(config::TTY_REX_SHORTCUTS_SCHEMA_ID).is_none() {
            return;
        }
        settings::use_memory_backend();
        let settings = open_settings().unwrap();
        for (key, _) in BINDINGS {
            let schema = settings.for_key(key).settings_schema().unwrap();
            assert!(schema.has_key(key), "{key}");
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
