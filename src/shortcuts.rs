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
use gtk::{gdk, gio, glib};

use crate::config;
use crate::settings::{self, SettingsError};

/// GTK's widget action for showing a widget's context menu.
const MENU_POPUP_ACTION: &str = "menu.popup";

/// Defines every shortcut once, from its schema key, the detailed action
/// it activates (such as `win.focus-tab(3)`, which carries its target as
/// the shortcut's arguments), and its title and optional subtitle on the
/// preferences Shortcuts page. This generates the [`key`] names,
/// [`entry`] rows, [`BINDINGS`], [`ALL_KEYS`] and [`TTY_REX_KEYS`].
macro_rules! shortcut_table {
    (@subtitle) => { None };
    (@subtitle $subtitle:literal) => { Some($subtitle) };
    (
        ptyxis: [$(
            $name:ident = ($key:literal, $action:expr, $title:literal $(, $subtitle:literal)?)
        ),+ $(,)?]
        tty_rex: [$(
            $tty_name:ident = ($tty_key:literal, $tty_action:expr, $tty_title:literal $(, $tty_subtitle:literal)?)
        ),+ $(,)?]
    ) => {
        /// Shortcut keys: those of the `org.gnome.Ptyxis.Shortcuts` schema
        /// and then tty-rex's own (see [`TTY_REX_KEYS`]).
        pub mod key {
            $(pub const $name: &str = $key;)+
            $(pub const $tty_name: &str = $tty_key;)+
        }

        /// The Shortcuts page row of each key.
        pub mod entry {
            use super::ShortcutEntry;
            $(pub const $name: ShortcutEntry = ShortcutEntry {
                key: $key,
                title: $title,
                subtitle: shortcut_table!(@subtitle $($subtitle)?),
            };)+
            $(pub const $tty_name: ShortcutEntry = ShortcutEntry {
                key: $tty_key,
                title: $tty_title,
                subtitle: shortcut_table!(@subtitle $($tty_subtitle)?),
            };)+
        }

        /// Shortcut setting key and the detailed action it activates.
        pub const BINDINGS: &[(&str, &str)] = &[
            $((key::$name, $action),)+
            $((key::$tty_name, $tty_action),)+
        ];

        /// Every key of the shortcuts schema, as in `src/ptyxis-shortcuts.defs`.
        pub const ALL_KEYS: &[&str] = &[$(key::$name),+];

        /// Every key of tty-rex's own shortcuts schema,
        /// [`config::TTY_REX_SHORTCUTS_SCHEMA_ID`].
        pub const TTY_REX_KEYS: &[&str] = &[$(key::$tty_name),+];
    };
}

shortcut_table! {
    ptyxis: [
        CLOSE_OTHER_TABS = ("close-other-tabs", "win.close-other-tabs", "Close Other Tabs", "Close all tabs except the active tab"),
        CLOSE_TAB = ("close-tab", "win.close-tab", "Close Tab", "Close the current tab"),
        // GTK's own action, as `action(window.close)` in `ptyxis-window.ui`.
        CLOSE_WINDOW = ("close-window", "window.close", "Close Window", "Close the current window"),
        COPY_CLIPBOARD = ("copy-clipboard", "win.copy-clipboard", "Copy to Clipboard"),
        DETACH_TAB = ("detach-tab", "win.detach-tab", "Move Tab to New Window", "Detach the current tab and move it to a new window"),
        FOCUS_TAB_1 = ("focus-tab-1", "win.focus-tab(1)", "Switch to Tab 1"),
        FOCUS_TAB_10 = ("focus-tab-10", "win.focus-tab(10)", "Switch to Tab 10"),
        FOCUS_TAB_2 = ("focus-tab-2", "win.focus-tab(2)", "Switch to Tab 2"),
        FOCUS_TAB_3 = ("focus-tab-3", "win.focus-tab(3)", "Switch to Tab 3"),
        FOCUS_TAB_4 = ("focus-tab-4", "win.focus-tab(4)", "Switch to Tab 4"),
        FOCUS_TAB_5 = ("focus-tab-5", "win.focus-tab(5)", "Switch to Tab 5"),
        FOCUS_TAB_6 = ("focus-tab-6", "win.focus-tab(6)", "Switch to Tab 6"),
        FOCUS_TAB_7 = ("focus-tab-7", "win.focus-tab(7)", "Switch to Tab 7"),
        FOCUS_TAB_8 = ("focus-tab-8", "win.focus-tab(8)", "Switch to Tab 8"),
        FOCUS_TAB_9 = ("focus-tab-9", "win.focus-tab(9)", "Switch to Tab 9"),
        FOCUS_TAB_LAST = ("focus-tab-last", "win.focus-tab(0)", "Switch to Last Tab"),
        MOVE_NEXT_TAB = ("move-next-tab", "win.next-tab", "Move to Next Tab"),
        MOVE_PREVIOUS_TAB = ("move-previous-tab", "win.previous-tab", "Move to Previous Tab"),
        MOVE_TAB_LEFT = ("move-tab-left", "win.move-tab-left", "Move Tab Left", "Move the tab one position to the left"),
        MOVE_TAB_RIGHT = ("move-tab-right", "win.move-tab-right", "Move Tab Right", "Move the tab one position to the right"),
        NEW_TAB = ("new-tab", "win.new-tab", "New Tab", "Open a new tab in the current window"),
        NEW_WINDOW = ("new-window", "win.new-window", "New Window", "Open a new tab in a new window"),
        PASTE_CLIPBOARD = ("paste-clipboard", "win.paste-clipboard", "Paste from Clipboard"),
        // As `action(menu.popup)` in `ptyxis-terminal.ui`. VTE has no such
        // action, so the key falls through to the terminal or the find bar
        // entry (which opens its own menu) instead of being swallowed.
        POPUP_MENU = ("popup-menu", MENU_POPUP_ACTION, "Show Context Menu"),
        PREFERENCES = ("preferences", "win.preferences", "Preferences", "Show this preferences window"),
        PRIMARY_MENU = ("primary-menu", "win.primary-menu", "Show Primary Menu", "Displays the primary menu for the terminal window"),
        RESET = ("reset", "win.reset(false)", "Reset"),
        RESET_AND_CLEAR = ("reset-and-clear", "win.reset(true)", "Reset and Clear"),
        SHOW_KEYBOARD_SHORTCUTS = ("show-keyboard-shortcuts", "win.show-keyboard-shortcuts", "Keyboard Shortcuts", "Show the keyboard shortcuts configuration"),
        SEARCH = ("search", "win.search", "Search History"),
        SELECT_ALL = ("select-all", "win.select-all(true)", "Select All"),
        SELECT_NONE = ("select-none", "win.select-all(false)", "Clear Selection"),
        SET_TITLE = ("set-title", "win.set-title", "Set Title"),
        TAB_MENU = ("tab-menu", "win.tab-menu", "Show New Tab Menu", "Displays the new tab menu for the terminal window"),
        TAB_OVERVIEW = ("tab-overview", "win.tab-overview", "View Tab Overview", "Show an overview of all tabs within the window"),
        TOGGLE_FULLSCREEN = ("toggle-fullscreen", "win.toggle-fullscreen", "Toggle Fullscreen", "Toggle in and out of fullscreen"),
        UNDO_CLOSE_TAB = ("undo-close-tab", "win.undo-close-tab", "Undo Close Tab", "Undo closing the last tab"),
        ZOOM_IN = ("zoom-in", "win.zoom-in", "Zoom In"),
        ZOOM_ONE = ("zoom-one", "win.zoom-one", "Reset Zoom"),
        ZOOM_OUT = ("zoom-out", "win.zoom-out", "Zoom Out"),
    ]
    tty_rex: [
        TOGGLE_TELEMETRY = ("toggle-telemetry", "win.toggle-telemetry", "Show Telemetry", "Toggle network, CPU and memory readings for the current tab"),
    ]
}

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

/// A group on the Shortcuts page: optional title and its rows.
pub type ShortcutGroup = (Option<&'static str>, &'static [ShortcutEntry]);

/// The Shortcuts page layout from `src/ptyxis-preferences-window.ui`.
pub const EDITOR_GROUPS: &[ShortcutGroup] = &[
    (
        Some("Terminal"),
        &[entry::COPY_CLIPBOARD, entry::PASTE_CLIPBOARD],
    ),
    (None, &[entry::SELECT_ALL, entry::SELECT_NONE]),
    (None, &[entry::RESET, entry::RESET_AND_CLEAR]),
    (None, &[entry::SEARCH, entry::TOGGLE_TELEMETRY]),
    (None, &[entry::POPUP_MENU]),
    (Some("Windows"), &[entry::NEW_WINDOW, entry::CLOSE_WINDOW]),
    (None, &[entry::SET_TITLE]),
    (Some("Tabs"), &[entry::NEW_TAB]),
    (None, &[entry::TAB_OVERVIEW]),
    (
        None,
        &[
            entry::CLOSE_TAB,
            entry::CLOSE_OTHER_TABS,
            entry::UNDO_CLOSE_TAB,
        ],
    ),
    (None, &[entry::MOVE_PREVIOUS_TAB, entry::MOVE_NEXT_TAB]),
    (
        None,
        &[
            entry::MOVE_TAB_LEFT,
            entry::MOVE_TAB_RIGHT,
            entry::DETACH_TAB,
        ],
    ),
    (
        None,
        &[
            entry::FOCUS_TAB_1,
            entry::FOCUS_TAB_2,
            entry::FOCUS_TAB_3,
            entry::FOCUS_TAB_4,
            entry::FOCUS_TAB_5,
            entry::FOCUS_TAB_6,
            entry::FOCUS_TAB_7,
            entry::FOCUS_TAB_8,
            entry::FOCUS_TAB_9,
            entry::FOCUS_TAB_10,
            entry::FOCUS_TAB_LAST,
        ],
    ),
    (
        Some("Zoom"),
        &[entry::ZOOM_IN, entry::ZOOM_OUT, entry::ZOOM_ONE],
    ),
    (Some("Application"), &[entry::TOGGLE_FULLSCREEN]),
    (
        None,
        &[
            entry::PRIMARY_MENU,
            entry::TAB_MENU,
            entry::PREFERENCES,
            entry::SHOW_KEYBOARD_SHORTCUTS,
        ],
    ),
];

/// The editor title of the shortcut `key`, e.g. "Search History".
pub fn title(key: &str) -> Option<&'static str> {
    EDITOR_GROUPS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .find(|row| row.key == key)
        .map(|row| row.title)
}

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

    /// The stored accelerator of the shortcut `key`.
    pub fn accelerator(&self, key: &str) -> String {
        self.for_key(key).string(key).into()
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

/// A parsed accelerator, as from `gtk::accelerator_parse()`.
pub type Accelerator = (gdk::Key, gdk::ModifierType);

/// The keyval and modifiers an accelerator matches, so spellings such as
/// `<ctrl><shift>M` and `<Control><Shift>m` compare equal.
fn normalize((key, modifiers): Accelerator) -> Accelerator {
    let key = match key.to_lower() {
        gdk::Key::ISO_Left_Tab => gdk::Key::Tab,
        key => key,
    };
    (key, modifiers)
}

/// The other shortcut, in either schema, already bound to `accel`.
///
/// Upstream Ptyxis saves duplicates silently; tty-rex refuses them so
/// one of the two actions does not quietly stop working. `current`
/// returns a shortcut's stored value and `parse` is
/// `gtk::accelerator_parse()`, passed in to keep this testable without a
/// display. A disabled (empty) `accel` or binding never conflicts, and
/// `key` itself is skipped, so keeping its current value is fine.
pub fn find_conflict(
    key: &str,
    accel: &str,
    current: impl Fn(&'static str) -> Option<String>,
    parse: impl Fn(&str) -> Option<Accelerator>,
) -> Option<&'static str> {
    let wanted = effective_accel(accel).and_then(&parse).map(normalize)?;
    ALL_KEYS
        .iter()
        .chain(TTY_REX_KEYS)
        .copied()
        .filter(|other| *other != key)
        .find(|other| {
            current(other)
                .as_deref()
                .and_then(effective_accel)
                .and_then(&parse)
                .map(normalize)
                == Some(wanted)
        })
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
        let Ok((name, target)) = gio::Action::parse_detailed_name(action) else {
            glib::g_warning!("tty-rex", "Invalid action “{action}” for {key}");
            continue;
        };
        match gtk::ShortcutTrigger::parse_string(accel) {
            Some(trigger) => {
                let shortcut =
                    gtk::Shortcut::new(Some(trigger), Some(gtk::NamedAction::new(&name)));
                shortcut.set_arguments(target.as_ref());
                controller.add_shortcut(shortcut);
            }
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
        PreferencesAction, PreviousTabAction, PrimaryMenuAction, SearchAction, SetTitleAction,
        ShowKeyboardShortcutsAction, TabMenuAction, TabOverviewAction, ToggleFullscreenAction,
        ToggleTelemetryAction, UndoCloseTabAction, ZoomInAction, ZoomOneAction, ZoomOutAction,
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
            (key::CLOSE_WINDOW, "window.close".to_string()),
            (key::COPY_CLIPBOARD, CopyAction::action_name()),
            (key::DETACH_TAB, DetachTabAction::action_name()),
            ("focus-tab-1", "win.focus-tab(1)".to_string()),
            ("focus-tab-10", "win.focus-tab(10)".to_string()),
            ("focus-tab-2", "win.focus-tab(2)".to_string()),
            ("focus-tab-3", "win.focus-tab(3)".to_string()),
            ("focus-tab-4", "win.focus-tab(4)".to_string()),
            ("focus-tab-5", "win.focus-tab(5)".to_string()),
            ("focus-tab-6", "win.focus-tab(6)".to_string()),
            ("focus-tab-7", "win.focus-tab(7)".to_string()),
            ("focus-tab-8", "win.focus-tab(8)".to_string()),
            ("focus-tab-9", "win.focus-tab(9)".to_string()),
            ("focus-tab-last", "win.focus-tab(0)".to_string()),
            (key::MOVE_NEXT_TAB, NextTabAction::action_name()),
            (key::MOVE_PREVIOUS_TAB, PreviousTabAction::action_name()),
            (key::MOVE_TAB_LEFT, MoveTabLeftAction::action_name()),
            (key::MOVE_TAB_RIGHT, MoveTabRightAction::action_name()),
            (key::NEW_TAB, NewTabAction::action_name()),
            (key::NEW_WINDOW, NewWindowAction::action_name()),
            (key::PASTE_CLIPBOARD, PasteAction::action_name()),
            (key::POPUP_MENU, MENU_POPUP_ACTION.to_string()),
            (key::PREFERENCES, PreferencesAction::action_name()),
            (key::PRIMARY_MENU, PrimaryMenuAction::action_name()),
            (key::RESET, "win.reset(false)".to_string()),
            (key::RESET_AND_CLEAR, "win.reset(true)".to_string()),
            (
                key::SHOW_KEYBOARD_SHORTCUTS,
                ShowKeyboardShortcutsAction::action_name(),
            ),
            (key::SEARCH, SearchAction::action_name()),
            (key::SELECT_ALL, "win.select-all(true)".to_string()),
            (key::SELECT_NONE, "win.select-all(false)".to_string()),
            (key::SET_TITLE, SetTitleAction::action_name()),
            (key::TAB_MENU, TabMenuAction::action_name()),
            (key::TAB_OVERVIEW, TabOverviewAction::action_name()),
            (
                key::TOGGLE_FULLSCREEN,
                ToggleFullscreenAction::action_name(),
            ),
            (key::UNDO_CLOSE_TAB, UndoCloseTabAction::action_name()),
            (key::ZOOM_IN, ZoomInAction::action_name()),
            (key::ZOOM_ONE, ZoomOneAction::action_name()),
            (key::ZOOM_OUT, ZoomOutAction::action_name()),
            (key::TOGGLE_TELEMETRY, ToggleTelemetryAction::action_name()),
        ];
        assert_eq!(BINDINGS.len(), expected.len());
        for ((key, action), (expected_key, expected_action)) in BINDINGS.iter().zip(&expected) {
            assert_eq!(key, expected_key);
            assert_eq!(*action, expected_action.as_str(), "{key}");
        }
    }

    #[test]
    fn detailed_actions_parse() {
        for (key, action) in BINDINGS {
            let (name, _) = gio::Action::parse_detailed_name(action).expect(key);
            assert!(
                name.starts_with("win.") || name == "window.close" || name == MENU_POPUP_ACTION,
                "{key}"
            );
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

    /// Enough of `gtk_accelerator_parse()` for the tests, which have no
    /// display to initialize GTK with: case-insensitive modifiers and the
    /// key name as written, leaving case folding to [`find_conflict`].
    fn parse(accel: &str) -> Option<Accelerator> {
        let mut modifiers = gdk::ModifierType::empty();
        let mut rest = accel;
        while let Some(tag) = rest.strip_prefix('<') {
            let (name, tail) = tag.split_once('>')?;
            modifiers |= match name.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "primary" => gdk::ModifierType::CONTROL_MASK,
                "shift" => gdk::ModifierType::SHIFT_MASK,
                "alt" => gdk::ModifierType::ALT_MASK,
                _ => return None,
            };
            rest = tail;
        }
        Some((gdk::Key::from_name(rest)?, modifiers))
    }

    #[test]
    fn conflicts() {
        let bindings = [
            (key::SEARCH, "<ctrl><shift>f"),
            (key::TOGGLE_TELEMETRY, "<ctrl><shift>m"),
            (key::NEW_TAB, "<Control><Shift>t"),
            (key::MOVE_NEXT_TAB, "<ctrl>Tab"),
            ("reset", ""),
            ("select-none", "  "),
        ];
        let current = |key: &str| {
            bindings
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        };
        let cases = [
            // Same accelerator, different spelling, across schemas.
            (
                key::NEW_TAB,
                "<Control><Shift>M",
                Some(key::TOGGLE_TELEMETRY),
            ),
            (
                key::TOGGLE_TELEMETRY,
                "<Primary><shift>F",
                Some(key::SEARCH),
            ),
            (key::SEARCH, "<ctrl><shift>T", Some(key::NEW_TAB)),
            // ISO_Left_Tab is Tab.
            (key::SEARCH, "<ctrl>ISO_Left_Tab", Some(key::MOVE_NEXT_TAB)),
            // Keeping the current value is not a conflict.
            (key::TOGGLE_TELEMETRY, "<ctrl><shift>M", None),
            // Disabling never conflicts, nor do disabled shortcuts.
            (key::SEARCH, "", None),
            (key::SEARCH, " ", None),
            // Different modifiers or keys.
            (key::SEARCH, "<ctrl>m", None),
            (key::SEARCH, "<ctrl><shift>n", None),
            (key::SEARCH, "<alt><shift>m", None),
            // Unparsable values match nothing.
            (key::SEARCH, "<bogus>m", None),
        ];
        for (key, accel, expected) in cases {
            assert_eq!(
                find_conflict(key, accel, current, parse),
                expected,
                "{key} {accel:?}"
            );
        }
    }

    #[test]
    fn titles() {
        assert_eq!(title(key::SEARCH), Some("Search History"));
        assert_eq!(title(key::TOGGLE_TELEMETRY), Some("Show Telemetry"));
        assert_eq!(title("no-such-key"), None);
        for key in ALL_KEYS.iter().chain(TTY_REX_KEYS) {
            assert!(title(key).is_some(), "{key}");
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
