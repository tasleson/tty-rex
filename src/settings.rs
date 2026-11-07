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

//! GSettings access for the `org.gnome.Ptyxis` schema, plus the few
//! settings tty-rex adds in its own `io.github.tasleson.TtyRex` schema.
//!
//! `gio::Settings::new()` aborts the process if a schema is missing, so all
//! settings objects are created through [`open`], which resolves the schema
//! explicitly and reports an actionable error instead.
//!
//! Note that font, cursor and bell options are application-wide keys in the
//! schema (see `src/ptyxis-settings.c`), not per-profile ones.

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::config;
use crate::palette::HighlightColors;

/// Keys of the `org.gnome.Ptyxis` schema used by the port.
pub mod key {
    pub const AUDIBLE_BELL: &str = "audible-bell";
    pub const CURSOR_BLINK_MODE: &str = "cursor-blink-mode";
    pub const CURSOR_SHAPE: &str = "cursor-shape";
    pub const DEFAULT_COLUMNS: &str = "default-columns";
    pub const DEFAULT_PROFILE_UUID: &str = "default-profile-uuid";
    pub const DEFAULT_ROWS: &str = "default-rows";
    pub const DISABLE_DYNAMIC_TITLE: &str = "disable-dynamic-title";
    pub const DISABLE_PADDING: &str = "disable-padding";
    pub const ENABLE_A11Y: &str = "enable-a11y";
    pub const ENABLE_ZOOM_SCROLL_CTRL: &str = "enable-zoom-scroll-ctrl";
    pub const FONT_NAME: &str = "font-name";
    pub const IGNORE_OSC_TITLE: &str = "ignore-osc-title";
    pub const INHIBIT_LOGOUT: &str = "inhibit-logout";
    pub const INTERFACE_STYLE: &str = "interface-style";
    pub const NEW_TAB_POSITION: &str = "new-tab-position";
    pub const PROFILE_UUIDS: &str = "profile-uuids";
    pub const PROMPT_ON_CLOSE: &str = "prompt-on-close";
    pub const RESTORE_SESSION: &str = "restore-session";
    pub const RESTORE_WINDOW_SIZE: &str = "restore-window-size";
    pub const SCROLLBAR_POLICY: &str = "scrollbar-policy";
    pub const SHOW_WINDOW_SIZE: &str = "show-window-size";
    pub const TAB_MIDDLE_CLICK: &str = "tab-middle-click";
    pub const TEXT_BLINK_MODE: &str = "text-blink-mode";
    pub const TOAST_ON_COPY_CLIPBOARD: &str = "toast-on-copy-clipboard";
    pub const USE_SYSTEM_FONT: &str = "use-system-font";
    pub const VISUAL_BELL: &str = "visual-bell";
    pub const VISUAL_PROCESS_LEADER: &str = "visual-process-leader";
    pub const WINDOW_SIZE: &str = "window-size";

    /// All keys, for validation against the schema template.
    pub const ALL: &[&str] = &[
        AUDIBLE_BELL,
        CURSOR_BLINK_MODE,
        CURSOR_SHAPE,
        DEFAULT_COLUMNS,
        DEFAULT_PROFILE_UUID,
        DEFAULT_ROWS,
        DISABLE_DYNAMIC_TITLE,
        DISABLE_PADDING,
        ENABLE_A11Y,
        ENABLE_ZOOM_SCROLL_CTRL,
        FONT_NAME,
        IGNORE_OSC_TITLE,
        INHIBIT_LOGOUT,
        INTERFACE_STYLE,
        NEW_TAB_POSITION,
        PROFILE_UUIDS,
        PROMPT_ON_CLOSE,
        RESTORE_SESSION,
        RESTORE_WINDOW_SIZE,
        SCROLLBAR_POLICY,
        SHOW_WINDOW_SIZE,
        TAB_MIDDLE_CLICK,
        TEXT_BLINK_MODE,
        TOAST_ON_COPY_CLIPBOARD,
        USE_SYSTEM_FONT,
        VISUAL_BELL,
        VISUAL_PROCESS_LEADER,
        WINDOW_SIZE,
    ];
}

/// Keys of tty-rex's own schema, [`config::TTY_REX_SCHEMA_ID`].
pub mod tty_rex_key {
    /// Window highlight backgrounds as CSS colors; empty means the
    /// palette's own color. They are always drawn opaque.
    pub const SUPERUSER_HIGHLIGHT_COLOR: &str = "superuser-highlight-color";
    pub const REMOTE_HIGHLIGHT_COLOR: &str = "remote-highlight-color";
    /// Seconds between telemetry footer updates.
    pub const TELEMETRY_INTERVAL: &str = "telemetry-interval";

    /// The CSS color keys.
    pub const COLOR_KEYS: &[&str] = &[SUPERUSER_HIGHLIGHT_COLOR, REMOTE_HIGHLIGHT_COLOR];
    /// All keys, for validation against the schema.
    pub const ALL: &[&str] = &[
        SUPERUSER_HIGHLIGHT_COLOR,
        REMOTE_HIGHLIGHT_COLOR,
        TELEMETRY_INTERVAL,
    ];
}

/// Range and default of [`tty_rex_key::TELEMETRY_INTERVAL`] in seconds,
/// as in the schema.
pub const TELEMETRY_INTERVAL_MIN: u32 = 1;
pub const TELEMETRY_INTERVAL_MAX: u32 = 60;
pub const TELEMETRY_INTERVAL_DEFAULT: u32 = 2;

/// Schema and key providing the desktop monospace font.
const DESKTOP_INTERFACE_SCHEMA_ID: &str = "org.gnome.desktop.interface";
pub const DESKTOP_MONOSPACE_FONT_KEY: &str = "monospace-font-name";
/// Same fallback as `ptyxis_application_init()`.
const FALLBACK_SYSTEM_FONT: &str = "Monospace 11";

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(
        "GSettings schema \"{0}\" is not installed; rebuild with glib-compile-schemas \
         available, or set GSETTINGS_SCHEMA_DIR to a directory containing the \
         compiled Ptyxis and tty-rex schemas"
    )]
    MissingSchema(String),
    #[error(
        "GSettings schema \"{schema_id}\" has no key \"{key}\"; the installed \
         schema is probably older than this build"
    )]
    MissingKey { schema_id: String, key: String },
    #[error("\"{value}\" in setting \"{key}\" is not a valid CSS color")]
    InvalidColor { key: String, value: String },
}

/// Parses a color setting where an empty (or blank) value means "no
/// override". Any alpha is dropped: a translucent highlight would let the
/// title bar beneath it show through and muddle the color.
pub fn parse_color_override(key: &str, value: &str) -> Result<Option<gdk::RGBA>, SettingsError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    gdk::RGBA::parse(value)
        .map(|color| {
            Some(gdk::RGBA::new(
                color.red(),
                color.green(),
                color.blue(),
                1.0,
            ))
        })
        .map_err(|_| SettingsError::InvalidColor {
            key: key.to_owned(),
            value: value.to_owned(),
        })
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
            glib::g_warning!("tty-rex", "Ignoring build schema directory {dir}: {e}");
            default
        }
    }
}

/// Defines an enum mirroring a schema `<enum>` with nick conversions.
///
/// Unknown nicks map to the declared default, which matches the schema
/// default, so a corrupted value never prevents a terminal from opening.
macro_rules! nick_enum {
    (
        $(#[$meta:meta])*
        $name:ident: $schema_enum:literal, default $default:ident {
            $($variant:ident => $nick:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Name of the enum in the schema, without the schema id prefix.
            pub const SCHEMA_ENUM: &'static str = $schema_enum;
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn from_nick(nick: &str) -> Option<Self> {
                match nick {
                    $($nick => Some(Self::$variant),)+
                    _ => None,
                }
            }

            pub fn from_nick_or_default(nick: &str) -> Self {
                Self::from_nick(nick).unwrap_or(Self::$default)
            }

            pub fn nick(self) -> &'static str {
                match self {
                    $(Self::$variant => $nick),+
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default
            }
        }
    };
}

nick_enum! {
    CursorShape: "CursorShape", default Block {
        Block => "block",
        Ibeam => "ibeam",
        Underline => "underline",
    }
}

impl From<CursorShape> for vte::CursorShape {
    fn from(value: CursorShape) -> Self {
        match value {
            CursorShape::Block => vte::CursorShape::Block,
            CursorShape::Ibeam => vte::CursorShape::Ibeam,
            CursorShape::Underline => vte::CursorShape::Underline,
        }
    }
}

nick_enum! {
    CursorBlinkMode: "CursorBlinkMode", default System {
        System => "system",
        On => "on",
        Off => "off",
    }
}

impl From<CursorBlinkMode> for vte::CursorBlinkMode {
    fn from(value: CursorBlinkMode) -> Self {
        match value {
            CursorBlinkMode::System => vte::CursorBlinkMode::System,
            CursorBlinkMode::On => vte::CursorBlinkMode::On,
            CursorBlinkMode::Off => vte::CursorBlinkMode::Off,
        }
    }
}

nick_enum! {
    TextBlinkMode: "TextBlinkMode", default Always {
        Never => "never",
        Focused => "focused",
        Unfocused => "unfocused",
        Always => "always",
    }
}

impl From<TextBlinkMode> for vte::TextBlinkMode {
    fn from(value: TextBlinkMode) -> Self {
        match value {
            TextBlinkMode::Never => vte::TextBlinkMode::Never,
            TextBlinkMode::Focused => vte::TextBlinkMode::Focused,
            TextBlinkMode::Unfocused => vte::TextBlinkMode::Unfocused,
            TextBlinkMode::Always => vte::TextBlinkMode::Always,
        }
    }
}

nick_enum! {
    /// What happens to a tab when its child process exits.
    ExitAction: "ExitAction", default Close {
        None => "none",
        Restart => "restart",
        Close => "close",
    }
}

nick_enum! {
    /// Whether new tabs open in the container the current tab runs in.
    PreserveContainer: "PreserveContainer", default Never {
        Never => "never",
        Always => "always",
    }
}

nick_enum! {
    PreserveDirectory: "PreserveDirectory", default Safe {
        Never => "never",
        Safe => "safe",
        Always => "always",
    }
}

nick_enum! {
    NewTabPosition: "NewTabPosition", default Last {
        Last => "last",
        Next => "next",
    }
}

nick_enum! {
    /// What a middle click on a tab in the tab bar does
    /// (`PtyxisTabMiddleClickBehavior`). Every choice but closing also
    /// selects the tab.
    TabMiddleClick: "TabMiddleClickBehavior", default Close {
        Close => "close",
        Paste => "paste",
        Nothing => "nothing",
    }
}

nick_enum! {
    InterfaceStyle: "InterfaceStyle", default Dark {
        System => "system",
        Light => "light",
        Dark => "dark",
    }
}

impl From<InterfaceStyle> for adw::ColorScheme {
    fn from(value: InterfaceStyle) -> Self {
        match value {
            InterfaceStyle::System => adw::ColorScheme::Default,
            InterfaceStyle::Light => adw::ColorScheme::ForceLight,
            InterfaceStyle::Dark => adw::ColorScheme::ForceDark,
        }
    }
}

nick_enum! {
    /// What the Backspace and Delete keys send (`EraseBinding`).
    EraseBinding: "EraseBinding", default Auto {
        Auto => "auto",
        AsciiBackspace => "ascii-backspace",
        AsciiDelete => "ascii-delete",
        DeleteSequence => "delete-sequence",
        Tty => "tty",
    }
}

impl From<EraseBinding> for vte::EraseBinding {
    fn from(value: EraseBinding) -> Self {
        match value {
            EraseBinding::Auto => vte::EraseBinding::Auto,
            EraseBinding::AsciiBackspace => vte::EraseBinding::AsciiBackspace,
            EraseBinding::AsciiDelete => vte::EraseBinding::AsciiDelete,
            EraseBinding::DeleteSequence => vte::EraseBinding::DeleteSequence,
            EraseBinding::Tty => vte::EraseBinding::Tty,
        }
    }
}

nick_enum! {
    CjkAmbiguousWidth: "CjkAmbiguousWidth", default Narrow {
        Narrow => "narrow",
        Wide => "wide",
    }
}

impl CjkAmbiguousWidth {
    /// The value `vte_terminal_set_cjk_ambiguous_width()` expects.
    pub fn columns(self) -> i32 {
        match self {
            Self::Narrow => 1,
            Self::Wide => 2,
        }
    }
}

/// Picks the font to use, following `ptyxis_settings_dup_font_desc()`: the
/// system font wins when requested or when no custom font is configured.
pub fn effective_font_name<'a>(
    use_system_font: bool,
    font_name: &'a str,
    system_font: &'a str,
) -> &'a str {
    if use_system_font || font_name.trim().is_empty() {
        system_font
    } else {
        font_name
    }
}

/// Picks the initial terminal grid size, following
/// `ptyxis_application_apply_default_size()`. A saved size of zero means
/// nothing was saved yet.
pub fn initial_grid_size(
    restore_window_size: bool,
    saved: (u32, u32),
    default: (u32, u32),
) -> (u32, u32) {
    if restore_window_size && saved.0 > 0 && saved.1 > 0 {
        saved
    } else {
        default
    }
}

/// Typed accessors over the application-wide `org.gnome.Ptyxis` settings
/// and tty-rex's own ones.
#[derive(Debug, Clone)]
pub struct AppSettings {
    settings: gio::Settings,
    tty_rex: gio::Settings,
    desktop: Option<gio::Settings>,
}

impl AppSettings {
    pub fn new() -> Result<Self, SettingsError> {
        let settings = open(config::SCHEMA_ID, None, key::ALL)?;
        let tty_rex = open(config::TTY_REX_SCHEMA_ID, None, tty_rex_key::ALL)?;
        // Every tab reads these, so invalid values are reported here, once
        // at startup and once per change, rather than by each reader.
        for key in tty_rex_key::COLOR_KEYS {
            warn_if_invalid_color(&tty_rex, key);
        }
        tty_rex.connect_changed(None, |settings, key| {
            if tty_rex_key::COLOR_KEYS.contains(&key) {
                warn_if_invalid_color(settings, key);
            }
        });
        // The desktop schema is optional (e.g. non-GNOME sessions).
        let desktop = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(DESKTOP_INTERFACE_SCHEMA_ID, true))
            .filter(|schema| schema.has_key(DESKTOP_MONOSPACE_FONT_KEY))
            .map(|schema| gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None));
        Ok(Self {
            settings,
            tty_rex,
            desktop,
        })
    }

    /// The underlying settings, for connecting change notifications.
    pub fn gsettings(&self) -> &gio::Settings {
        &self.settings
    }

    /// Settings of tty-rex's own schema (keys in [`tty_rex_key`]).
    pub fn tty_rex_gsettings(&self) -> &gio::Settings {
        &self.tty_rex
    }

    /// The color stored in the tty-rex color setting `key`, or `None` to use
    /// the palette's. An invalid value is ignored (and reported when it is
    /// stored), so a bad edit with `gsettings` or `dconf` never breaks the
    /// window colors.
    pub fn color_override(&self, key: &str) -> Option<gdk::RGBA> {
        parse_color_override(key, &self.tty_rex.string(key))
            .ok()
            .flatten()
    }

    /// Whether the tty-rex color setting `key` holds anything, valid or not.
    pub fn has_color_override(&self, key: &str) -> bool {
        !self.tty_rex.string(key).trim().is_empty()
    }

    /// The chosen superuser and remote highlight backgrounds.
    pub fn highlight_colors(&self) -> HighlightColors {
        HighlightColors {
            superuser: self.color_override(tty_rex_key::SUPERUSER_HIGHLIGHT_COLOR),
            remote: self.color_override(tty_rex_key::REMOTE_HIGHLIGHT_COLOR),
        }
    }

    /// Stores `color` in the tty-rex color setting `key`; `None` goes back
    /// to the palette's color.
    pub fn set_color_override(
        &self,
        key: &str,
        color: Option<&gdk::RGBA>,
    ) -> Result<(), glib::BoolError> {
        match color {
            Some(color) => self.tty_rex.set_string(key, &color.to_str()),
            None => {
                self.tty_rex.reset(key);
                Ok(())
            }
        }
    }

    /// Time between telemetry footer updates. The schema enforces the
    /// range; clamping only guards against a hand-edited older schema.
    pub fn telemetry_interval(&self) -> std::time::Duration {
        let seconds = self
            .tty_rex
            .uint(tty_rex_key::TELEMETRY_INTERVAL)
            .clamp(TELEMETRY_INTERVAL_MIN, TELEMETRY_INTERVAL_MAX);
        std::time::Duration::from_secs(seconds.into())
    }

    /// Desktop interface settings, if the schema is installed.
    pub fn desktop_settings(&self) -> Option<&gio::Settings> {
        self.desktop.as_ref()
    }

    fn enum_value<T>(&self, key: &str, parse: fn(&str) -> T) -> T {
        parse(self.settings.string(key).as_str())
    }

    /// Whether every tab, not only pinned ones, comes back on the next
    /// start (`ptyxis_settings_get_restore_session()`).
    pub fn restore_session(&self) -> bool {
        self.settings.boolean(key::RESTORE_SESSION)
    }

    /// Whether closing a tab or window with a program still running asks
    /// first (`ptyxis_settings_get_prompt_on_close()`).
    pub fn prompt_on_close(&self) -> bool {
        self.settings.boolean(key::PROMPT_ON_CLOSE)
    }

    /// Whether a tab running a program other than a shell asks the session
    /// to hold off logging out (`ptyxis_settings_get_inhibit_logout()`).
    pub fn inhibit_logout(&self) -> bool {
        self.settings.boolean(key::INHIBIT_LOGOUT)
    }

    pub fn restore_window_size(&self) -> bool {
        self.settings.boolean(key::RESTORE_WINDOW_SIZE)
    }

    /// The configured (columns, rows) for new windows
    /// (`ptyxis_settings_get_default_size()`).
    pub fn default_grid_size(&self) -> (u32, u32) {
        (
            self.settings.uint(key::DEFAULT_COLUMNS),
            self.settings.uint(key::DEFAULT_ROWS),
        )
    }

    pub fn visual_bell(&self) -> bool {
        self.settings.boolean(key::VISUAL_BELL)
    }

    pub fn audible_bell(&self) -> bool {
        self.settings.boolean(key::AUDIBLE_BELL)
    }

    /// Whether terminals drop the `padded` style class
    /// (`ptyxis_settings_get_disable_padding()`).
    pub fn disable_padding(&self) -> bool {
        self.settings.boolean(key::DISABLE_PADDING)
    }

    /// Whether Ctrl+scroll zooms the terminal
    /// (`ptyxis_settings_get_enable_zoom_scroll_ctrl()`).
    pub fn enable_zoom_scroll_ctrl(&self) -> bool {
        self.settings.boolean(key::ENABLE_ZOOM_SCROLL_CTRL)
    }

    /// Whether resizing shows the terminal grid size
    /// (`ptyxis_settings_get_show_window_size()`).
    pub fn show_window_size(&self) -> bool {
        self.settings.boolean(key::SHOW_WINDOW_SIZE)
    }

    /// Whether copying the selection to the clipboard shows a toast
    /// (`ptyxis_settings_get_toast_on_copy_clipboard()`).
    pub fn toast_on_copy_clipboard(&self) -> bool {
        self.settings.boolean(key::TOAST_ON_COPY_CLIPBOARD)
    }

    pub fn ignore_osc_title(&self) -> bool {
        self.settings.boolean(key::IGNORE_OSC_TITLE)
    }

    /// Whether the window is highlighted while a superuser or remote
    /// process leads the active terminal.
    pub fn visual_process_leader(&self) -> bool {
        self.settings.boolean(key::VISUAL_PROCESS_LEADER)
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.enum_value(key::CURSOR_SHAPE, CursorShape::from_nick_or_default)
    }

    pub fn cursor_blink_mode(&self) -> CursorBlinkMode {
        self.enum_value(
            key::CURSOR_BLINK_MODE,
            CursorBlinkMode::from_nick_or_default,
        )
    }

    pub fn text_blink_mode(&self) -> TextBlinkMode {
        self.enum_value(key::TEXT_BLINK_MODE, TextBlinkMode::from_nick_or_default)
    }

    pub fn new_tab_position(&self) -> NewTabPosition {
        self.enum_value(key::NEW_TAB_POSITION, NewTabPosition::from_nick_or_default)
    }

    pub fn tab_middle_click(&self) -> TabMiddleClick {
        self.enum_value(key::TAB_MIDDLE_CLICK, TabMiddleClick::from_nick_or_default)
    }

    pub fn interface_style(&self) -> InterfaceStyle {
        self.enum_value(key::INTERFACE_STYLE, InterfaceStyle::from_nick_or_default)
    }

    pub fn profile_uuids(&self) -> Vec<String> {
        self.settings
            .strv(key::PROFILE_UUIDS)
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    pub fn set_profile_uuids(&self, uuids: &[String]) -> Result<(), glib::BoolError> {
        self.settings.set_strv(key::PROFILE_UUIDS, uuids)
    }

    pub fn default_profile_uuid(&self) -> String {
        self.settings.string(key::DEFAULT_PROFILE_UUID).into()
    }

    pub fn set_default_profile_uuid(&self, uuid: &str) -> Result<(), glib::BoolError> {
        self.settings.set_string(key::DEFAULT_PROFILE_UUID, uuid)
    }

    pub fn system_font_name(&self) -> String {
        self.desktop
            .as_ref()
            .map(|s| s.string(DESKTOP_MONOSPACE_FONT_KEY).to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| FALLBACK_SYSTEM_FONT.to_owned())
    }

    pub fn font_desc(&self) -> pango::FontDescription {
        let font_name = self.settings.string(key::FONT_NAME);
        let system_font = self.system_font_name();
        pango::FontDescription::from_string(effective_font_name(
            self.settings.boolean(key::USE_SYSTEM_FONT),
            &font_name,
            &system_font,
        ))
    }

    /// Remembers the last window's grid size for `restore-window-size`
    /// (`ptyxis_settings_set_window_size()`).
    pub fn set_window_size(&self, (columns, rows): (u32, u32)) -> Result<(), glib::BoolError> {
        self.settings.set(key::WINDOW_SIZE, (columns, rows))
    }

    /// Initial terminal size in (columns, rows) for a new window.
    pub fn initial_grid_size(&self) -> (u32, u32) {
        let saved: (u32, u32) = self.settings.get(key::WINDOW_SIZE);
        initial_grid_size(self.restore_window_size(), saved, self.default_grid_size())
    }
}

fn warn_if_invalid_color(settings: &gio::Settings, key: &str) {
    if let Err(e) = parse_color_override(key, &settings.string(key)) {
        glib::g_warning!("tty-rex", "{e}; using the palette color");
    }
}

/// Looks up a schema by id, searching parent sources too.
pub fn lookup_schema(schema_id: &str) -> Option<gio::SettingsSchema> {
    schema_source()?.lookup(schema_id, true)
}

/// Opens settings for `schema_id`, at `path` for relocatable schemas.
///
/// Reading a key the schema lacks aborts the process, which can happen
/// when an older Ptyxis schema is picked up, so `required_keys` are checked
/// up front and reported as an error instead.
pub fn open(
    schema_id: &str,
    path: Option<&str>,
    required_keys: &[&str],
) -> Result<gio::Settings, SettingsError> {
    let schema =
        lookup_schema(schema_id).ok_or_else(|| SettingsError::MissingSchema(schema_id.into()))?;
    if let Some(key) = required_keys.iter().find(|key| !schema.has_key(key)) {
        return Err(SettingsError::MissingKey {
            schema_id: schema_id.to_owned(),
            key: (*key).to_owned(),
        });
    }
    Ok(gio::Settings::new_full(&schema, backend().as_ref(), path))
}

#[cfg(test)]
thread_local! {
    static TEST_BACKEND: std::cell::RefCell<Option<gio::SettingsBackend>> =
        const { std::cell::RefCell::new(None) };
}

/// The backend settings are opened with: the default one, except in
/// tests that opted into an in-memory backend.
fn backend() -> Option<gio::SettingsBackend> {
    #[cfg(test)]
    return TEST_BACKEND.with_borrow(Clone::clone);
    #[cfg(not(test))]
    None
}

/// Makes settings opened on this test thread use a fresh in-memory
/// backend, so tests can write without touching the user's dconf.
#[cfg(test)]
pub(crate) fn use_memory_backend() {
    TEST_BACKEND.with_borrow_mut(|backend| *backend = Some(gio::memory_settings_backend_new()));
}

/// Helpers for validating Rust constants against the schema template.
#[cfg(test)]
pub(crate) mod schema_template {
    pub const TEMPLATE: &str = include_str!("../ptyxis/org.gnome.Ptyxis.gschema.xml.in");

    /// Returns the XML body of the element starting with `open_tag`.
    fn block(open_tag: &str, close_tag: &str) -> &'static str {
        let start = TEMPLATE
            .find(open_tag)
            .unwrap_or_else(|| panic!("{open_tag} not found in schema template"));
        let rest = &TEMPLATE[start..];
        let end = rest.find(close_tag).expect("unterminated block");
        &rest[..end]
    }

    pub fn schema_block(schema_id_placeholder: &str) -> &'static str {
        block(
            &format!("<schema id=\"{schema_id_placeholder}\""),
            "</schema>",
        )
    }

    pub fn enum_block(name: &str) -> &'static str {
        block(&format!("<enum id=\"@APP_SCHEMA_ID@.{name}\""), "</enum>")
    }

    pub fn has_key(schema_id_placeholder: &str, key: &str) -> bool {
        schema_block(schema_id_placeholder).contains(&format!("<key name=\"{key}\""))
    }
}

#[cfg(test)]
pub(crate) mod tests {
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

    const TTY_REX_SCHEMA: &str = include_str!("../data/io.github.tasleson.TtyRex.gschema.xml");

    #[test]
    fn tty_rex_schema_matches_config() {
        assert!(TTY_REX_SCHEMA.contains(&format!(
            "<schema id=\"{}\" path=\"{}\">",
            config::TTY_REX_SCHEMA_ID,
            config::TTY_REX_SCHEMA_PATH
        )));
        for key in tty_rex_key::ALL {
            assert!(
                TTY_REX_SCHEMA.contains(&format!("<key name=\"{key}\" ")),
                "{key} missing from tty-rex schema"
            );
        }
        for key in tty_rex_key::COLOR_KEYS {
            let start = TTY_REX_SCHEMA
                .find(&format!("<key name=\"{key}\" type=\"s\">"))
                .unwrap_or_else(|| panic!("{key} missing from tty-rex schema"));
            // Empty means "use the palette color", so it must be the default.
            assert!(
                TTY_REX_SCHEMA[start..]
                    .trim_start_matches(|c| c != '\n')
                    .trim_start()
                    .starts_with("<default>''</default>"),
                "{key} should default to ''"
            );
        }
        if let Some(schema) = expected_schema(config::TTY_REX_SCHEMA_ID) {
            assert_eq!(schema.path().as_deref(), Some(config::TTY_REX_SCHEMA_PATH));
            assert!(open(config::TTY_REX_SCHEMA_ID, None, tty_rex_key::ALL).is_ok());
        }
    }

    #[test]
    fn telemetry_interval_matches_schema() {
        let start = TTY_REX_SCHEMA
            .find(&format!(
                "<key name=\"{}\" type=\"u\">",
                tty_rex_key::TELEMETRY_INTERVAL
            ))
            .expect("telemetry interval key");
        let key = &TTY_REX_SCHEMA[start..];
        let key = &key[..key.find("</key>").unwrap()];
        assert!(key.contains(&format!(
            "<range min=\"{TELEMETRY_INTERVAL_MIN}\" max=\"{TELEMETRY_INTERVAL_MAX}\"/>"
        )));
        assert!(key.contains(&format!("<default>{TELEMETRY_INTERVAL_DEFAULT}</default>")));

        if expected_schema(config::TTY_REX_SCHEMA_ID).is_none() {
            return;
        }
        use_memory_backend();
        let settings = AppSettings::new().unwrap();
        let interval = |seconds: u64| std::time::Duration::from_secs(seconds);
        assert_eq!(
            settings.telemetry_interval(),
            interval(TELEMETRY_INTERVAL_DEFAULT.into())
        );
        settings
            .tty_rex_gsettings()
            .set_uint(tty_rex_key::TELEMETRY_INTERVAL, 15)
            .unwrap();
        assert_eq!(settings.telemetry_interval(), interval(15));
    }

    #[test]
    fn color_override_parsing() {
        let cases = [
            ("", Ok(None)),
            ("  ", Ok(None)),
            ("#ff0000", Ok(Some(gdk::RGBA::RED))),
            (" rgb(0,0,255) ", Ok(Some(gdk::RGBA::BLUE))),
            ("red", Ok(Some(gdk::RGBA::RED))),
            // Highlights are always opaque.
            ("rgba(255,0,0,0.3)", Ok(Some(gdk::RGBA::RED))),
            ("rgba(0,0,255,0)", Ok(Some(gdk::RGBA::BLUE))),
            ("nope", Err("nope")),
            ("#12", Err("#12")),
        ];
        for (value, expected) in cases {
            let result = parse_color_override("k", value);
            match (result, expected) {
                (Ok(color), Ok(expected)) => assert_eq!(color, expected, "{value:?}"),
                (Err(SettingsError::InvalidColor { key, value: bad }), Err(expected)) => {
                    assert_eq!((key.as_str(), bad.as_str()), ("k", expected));
                }
                (result, _) => panic!("{value:?}: unexpected {result:?}"),
            }
        }
    }

    #[test]
    fn color_overrides_round_trip() {
        if expected_schema(config::TTY_REX_SCHEMA_ID).is_none() {
            return;
        }
        use_memory_backend();
        let settings = AppSettings::new().unwrap();
        let key = tty_rex_key::REMOTE_HIGHLIGHT_COLOR;
        assert_eq!(settings.color_override(key), None);
        assert!(!settings.has_color_override(key));
        let color = gdk::RGBA::new(0.2, 0.4, 0.6, 1.0);
        settings.set_color_override(key, Some(&color)).unwrap();
        let stored = settings.color_override(key).expect("stored color");
        assert!(
            [
                (stored.red(), color.red()),
                (stored.green(), color.green()),
                (stored.blue(), color.blue()),
            ]
            .iter()
            .all(|(a, b)| (a - b).abs() < 1.0 / 255.0),
            "{stored} != {color}"
        );
        settings
            .tty_rex_gsettings()
            .set_string(key, "bogus")
            .unwrap();
        assert_eq!(settings.color_override(key), None);
        assert!(settings.has_color_override(key));
        settings.set_color_override(key, None).unwrap();
        assert_eq!(settings.tty_rex_gsettings().string(key), "");
    }

    #[test]
    fn app_keys_exist_in_schema() {
        for key in key::ALL {
            assert!(
                schema_template::has_key("@APP_SCHEMA_ID@", key),
                "{key} missing from schema"
            );
        }
    }

    fn assert_enum_matches_schema(schema_enum: &str, nicks: &[&str]) {
        let block = schema_template::enum_block(schema_enum);
        let schema_nicks = block.matches("nick=\"").count();
        assert_eq!(schema_nicks, nicks.len(), "{schema_enum}: nick count");
        for nick in nicks {
            assert!(
                block.contains(&format!("nick=\"{nick}\"")),
                "{schema_enum}: {nick} not in schema"
            );
        }
    }

    macro_rules! check_enum {
        ($ty:ty) => {{
            let nicks: Vec<_> = <$ty>::ALL.iter().map(|v| v.nick()).collect();
            assert_enum_matches_schema(<$ty>::SCHEMA_ENUM, &nicks);
            for v in <$ty>::ALL {
                assert_eq!(<$ty>::from_nick(v.nick()), Some(*v));
            }
            assert_eq!(<$ty>::from_nick("bogus"), None);
            assert_eq!(<$ty>::from_nick_or_default("bogus"), <$ty>::default());
        }};
    }

    #[test]
    fn enums_match_schema() {
        check_enum!(CursorShape);
        check_enum!(CursorBlinkMode);
        check_enum!(TextBlinkMode);
        check_enum!(ExitAction);
        check_enum!(PreserveContainer);
        check_enum!(PreserveDirectory);
        check_enum!(NewTabPosition);
        check_enum!(TabMiddleClick);
        check_enum!(InterfaceStyle);
        check_enum!(EraseBinding);
        check_enum!(CjkAmbiguousWidth);
    }

    #[test]
    fn enum_defaults_match_schema_defaults() {
        let cases: &[(&str, &str)] = &[
            (key::CURSOR_SHAPE, CursorShape::default().nick()),
            (key::CURSOR_BLINK_MODE, CursorBlinkMode::default().nick()),
            (key::TEXT_BLINK_MODE, TextBlinkMode::default().nick()),
            (key::NEW_TAB_POSITION, NewTabPosition::default().nick()),
            (key::INTERFACE_STYLE, InterfaceStyle::default().nick()),
            (key::TAB_MIDDLE_CLICK, TabMiddleClick::default().nick()),
        ];
        let block = schema_template::schema_block("@APP_SCHEMA_ID@");
        for (key, nick) in cases {
            let start = block.find(&format!("<key name=\"{key}\"")).unwrap();
            let default = &block[start..];
            let default = &default[default.find("<default>").unwrap()..];
            assert!(
                default.starts_with(&format!("<default>'{nick}'</default>")),
                "{key} default should be {nick}"
            );
        }
    }

    #[test]
    fn font_selection() {
        let cases = [
            (true, "Mono 12", "System 10", "System 10"),
            (false, "Mono 12", "System 10", "Mono 12"),
            (false, "", "System 10", "System 10"),
            (false, "  ", "System 10", "System 10"),
        ];
        for (use_system, font, system, expected) in cases {
            assert_eq!(effective_font_name(use_system, font, system), expected);
        }
    }

    #[test]
    fn grid_size_selection() {
        let cases = [
            (true, (120, 38), (80, 24), (120, 38)),
            (true, (0, 0), (80, 24), (80, 24)),
            (true, (120, 0), (80, 24), (80, 24)),
            (false, (120, 38), (80, 24), (80, 24)),
        ];
        for (restore, saved, default, expected) in cases {
            assert_eq!(initial_grid_size(restore, saved, default), expected);
        }
    }

    #[test]
    fn build_schemas_resolve() {
        for id in [
            config::SCHEMA_ID,
            config::PROFILE_SCHEMA_ID,
            config::SHORTCUTS_SCHEMA_ID,
            config::TTY_REX_SCHEMA_ID,
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
        let err = open("org.gnome.Ptyxis.DoesNotExist", None, &[]).unwrap_err();
        assert!(err.to_string().contains("org.gnome.Ptyxis.DoesNotExist"));
    }

    #[test]
    fn boolean_keys_round_trip() {
        if expected_schema(config::SCHEMA_ID).is_none() {
            return;
        }
        use_memory_backend();
        let settings = AppSettings::new().unwrap();
        type Getter = fn(&AppSettings) -> bool;
        // (key, getter, Ptyxis default)
        let cases: &[(&str, Getter, bool)] = &[
            // Ptyxis pads terminals unless told otherwise.
            (key::DISABLE_PADDING, AppSettings::disable_padding, false),
            // Ctrl+scroll zooms by default.
            (
                key::ENABLE_ZOOM_SCROLL_CTRL,
                AppSettings::enable_zoom_scroll_ctrl,
                true,
            ),
            // Resizing shows the grid size by default.
            (key::SHOW_WINDOW_SIZE, AppSettings::show_window_size, true),
            // Copying to the clipboard shows a toast by default.
            (
                key::TOAST_ON_COPY_CLIPBOARD,
                AppSettings::toast_on_copy_clipboard,
                true,
            ),
        ];
        for &(name, get, default) in cases {
            assert_eq!(get(&settings), default, "{name} default");
            for value in [!default, default] {
                settings.gsettings().set_boolean(name, value).unwrap();
                assert_eq!(get(&settings), value, "{name}");
            }
        }
    }

    #[test]
    fn missing_key_is_an_error() {
        if expected_schema(config::SCHEMA_ID).is_none() {
            return;
        }
        let err = open(config::SCHEMA_ID, None, &[key::AUDIBLE_BELL, "no-such-key"]).unwrap_err();
        assert!(
            matches!(&err, SettingsError::MissingKey { key, .. } if key == "no-such-key"),
            "{err}"
        );
        assert!(open(config::SCHEMA_ID, None, key::ALL).is_ok());
    }
}
