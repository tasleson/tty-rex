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

//! GSettings access for the `org.gnome.Ptyxis` schema.
//!
//! `gio::Settings::new()` aborts the process if a schema is missing, so all
//! settings objects are created through [`open`], which resolves the schema
//! explicitly and reports an actionable error instead.
//!
//! Note that font, cursor and bell options are application-wide keys in the
//! schema (see `src/ptyxis-settings.c`), not per-profile ones.

use gtk::prelude::*;
use gtk::{gio, glib, pango};

use crate::config;

/// Keys of the `org.gnome.Ptyxis` schema used by the port.
pub mod key {
    pub const AUDIBLE_BELL: &str = "audible-bell";
    pub const CURSOR_BLINK_MODE: &str = "cursor-blink-mode";
    pub const CURSOR_SHAPE: &str = "cursor-shape";
    pub const DEFAULT_COLUMNS: &str = "default-columns";
    pub const DEFAULT_PROFILE_UUID: &str = "default-profile-uuid";
    pub const DEFAULT_ROWS: &str = "default-rows";
    pub const DISABLE_DYNAMIC_TITLE: &str = "disable-dynamic-title";
    pub const ENABLE_A11Y: &str = "enable-a11y";
    pub const FONT_NAME: &str = "font-name";
    pub const IGNORE_OSC_TITLE: &str = "ignore-osc-title";
    pub const INTERFACE_STYLE: &str = "interface-style";
    pub const NEW_TAB_POSITION: &str = "new-tab-position";
    pub const PROFILE_UUIDS: &str = "profile-uuids";
    pub const RESTORE_SESSION: &str = "restore-session";
    pub const RESTORE_WINDOW_SIZE: &str = "restore-window-size";
    pub const SCROLLBAR_POLICY: &str = "scrollbar-policy";
    pub const TEXT_BLINK_MODE: &str = "text-blink-mode";
    pub const USE_SYSTEM_FONT: &str = "use-system-font";
    pub const VISUAL_BELL: &str = "visual-bell";
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
        ENABLE_A11Y,
        FONT_NAME,
        IGNORE_OSC_TITLE,
        INTERFACE_STYLE,
        NEW_TAB_POSITION,
        PROFILE_UUIDS,
        RESTORE_SESSION,
        RESTORE_WINDOW_SIZE,
        SCROLLBAR_POLICY,
        TEXT_BLINK_MODE,
        USE_SYSTEM_FONT,
        VISUAL_BELL,
        WINDOW_SIZE,
    ];
}

/// Schema and key providing the desktop monospace font.
const DESKTOP_INTERFACE_SCHEMA_ID: &str = "org.gnome.desktop.interface";
pub const DESKTOP_MONOSPACE_FONT_KEY: &str = "monospace-font-name";
/// Same fallback as `ptyxis_application_init()`.
const FALLBACK_SYSTEM_FONT: &str = "Monospace 11";

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(
        "GSettings schema \"{0}\" is not installed; install Ptyxis or set \
         GSETTINGS_SCHEMA_DIR to a directory containing the compiled schema"
    )]
    MissingSchema(String),
    #[error(
        "GSettings schema \"{schema_id}\" has no key \"{key}\"; the installed \
         schema is probably older than this build"
    )]
    MissingKey { schema_id: String, key: String },
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

/// Typed accessors over the application-wide `org.gnome.Ptyxis` settings.
#[derive(Debug, Clone)]
pub struct AppSettings {
    settings: gio::Settings,
    desktop: Option<gio::Settings>,
}

impl AppSettings {
    pub fn new() -> Result<Self, SettingsError> {
        let settings = open(config::SCHEMA_ID, None, key::ALL)?;
        // The desktop schema is optional (e.g. non-GNOME sessions).
        let desktop = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(DESKTOP_INTERFACE_SCHEMA_ID, true))
            .filter(|schema| schema.has_key(DESKTOP_MONOSPACE_FONT_KEY))
            .map(|schema| gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None));
        Ok(Self { settings, desktop })
    }

    /// The underlying settings, for connecting change notifications.
    pub fn gsettings(&self) -> &gio::Settings {
        &self.settings
    }

    /// Desktop interface settings, if the schema is installed.
    pub fn desktop_settings(&self) -> Option<&gio::Settings> {
        self.desktop.as_ref()
    }

    fn enum_value<T>(&self, key: &str, parse: fn(&str) -> T) -> T {
        parse(self.settings.string(key).as_str())
    }

    pub fn audible_bell(&self) -> bool {
        self.settings.boolean(key::AUDIBLE_BELL)
    }

    pub fn ignore_osc_title(&self) -> bool {
        self.settings.boolean(key::IGNORE_OSC_TITLE)
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

    /// Initial terminal size in (columns, rows) for a new window.
    pub fn initial_grid_size(&self) -> (u32, u32) {
        let saved: (u32, u32) = self.settings.get(key::WINDOW_SIZE);
        initial_grid_size(
            self.settings.boolean(key::RESTORE_WINDOW_SIZE),
            saved,
            (
                self.settings.uint(key::DEFAULT_COLUMNS),
                self.settings.uint(key::DEFAULT_ROWS),
            ),
        )
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
    pub const TEMPLATE: &str = include_str!("../../../src/org.gnome.Ptyxis.gschema.xml.in");

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
        check_enum!(PreserveDirectory);
        check_enum!(NewTabPosition);
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
