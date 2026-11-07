// terminal.rs
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

//! Configuration of `vte::Terminal` widgets from settings, profiles and
//! palettes (the parts of `src/ptyxis-terminal.c` and `src/ptyxis-tab.c`
//! that bind properties onto the terminal).

use gtk::glib;
use gtk::glib::translate::ToGlibPtr;
use vte::prelude::*;

use crate::palette::{self, Palette};
use crate::profile::Profile;
use crate::settings::AppSettings;
use crate::terminal_widget::TerminalWidget;

/// Zoom step, matching `PtyxisZoomLevel` (`MINUS_14` ..= `PLUS_14`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ZoomLevel(i8);

impl ZoomLevel {
    pub const MIN: Self = Self(-14);
    pub const MAX: Self = Self(14);
    pub const DEFAULT: Self = Self(0);

    /// Each step scales by `sqrt(1.2)`, so two steps are a factor of 1.2
    /// (the `zoom_font_scales` table in `src/ptyxis-tab.c`).
    pub fn font_scale(self) -> f64 {
        1.2_f64.powf(f64::from(self.0) / 2.0)
    }

    pub fn zoom_in(self) -> Self {
        Self((self.0 + 1).min(Self::MAX.0))
    }

    /// Percentage shown for the zoom (`ptyxis_tab_dup_zoom_label()`).
    pub fn label(self) -> String {
        if self == Self::DEFAULT {
            "100%".to_owned()
        } else {
            format!("{:.0}%", self.font_scale() * 100.0)
        }
    }

    pub fn zoom_out(self) -> Self {
        Self((self.0 - 1).max(Self::MIN.0))
    }

    /// Whether `win.zoom-in` applies, i.e. the level is below the maximum
    /// (`ptyxis_window_notify_zoom_cb()`).
    pub fn can_zoom_in(self) -> bool {
        self != Self::MAX
    }

    /// Whether `win.zoom-out` applies, i.e. the level is above the minimum.
    pub fn can_zoom_out(self) -> bool {
        self != Self::MIN
    }

    /// Whether `win.zoom-one` applies, i.e. the level is not the default.
    pub fn can_reset(self) -> bool {
        self != Self::DEFAULT
    }

    /// The level after a Ctrl+scroll of `dy`: scrolling up zooms in,
    /// down zooms out, as in `on_scroll_scrolled_cb()`.
    pub fn scrolled(self, dy: f64) -> Self {
        if dy < 0.0 {
            self.zoom_in()
        } else if dy > 0.0 {
            self.zoom_out()
        } else {
            self
        }
    }

    /// The `PtyxisZoomLevel` value, which numbers the steps from 1
    /// (`PTYXIS_ZOOM_LEVEL_MINUS_14`), as stored in the session file.
    pub fn to_ptyxis_enum(self) -> u32 {
        (i32::from(self.0) - i32::from(Self::MIN.0) + 1).unsigned_abs()
    }

    /// The level for a `PtyxisZoomLevel` value, or `None` outside the enum.
    pub fn from_ptyxis_enum(value: u32) -> Option<Self> {
        let level = i64::from(value) + i64::from(Self::MIN.0) - 1;
        i8::try_from(level)
            .ok()
            .map(Self)
            .filter(|level| (Self::MIN..=Self::MAX).contains(level))
    }
}

/// Creates a terminal configured like the one in `src/ptyxis-tab.ui`.
pub fn create() -> vte::Terminal {
    let terminal: vte::Terminal = TerminalWidget::new().upcast();
    terminal.set_enable_fallback_scrolling(false);
    terminal.set_scroll_unit_is_pixels(true);
    terminal.set_allow_hyperlink(true);
    // Shells in toolbox and other containers report the container with
    // OSC 777, which sets the `vte.container.*` termprops.
    terminal.set_enable_legacy_osc777(true);
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);
    terminal
}

/// Style class from `style.css` that pads the terminal so its grid sits
/// clear of the window's rounded corners.
const PADDED_CSS_CLASS: &str = "padded";

/// Applies application-wide settings (font, cursor, bell, padding, size
/// label, accessibility).
pub fn apply_app_settings(terminal: &vte::Terminal, settings: &AppSettings) {
    if let Some(terminal) = terminal.downcast_ref::<TerminalWidget>() {
        terminal.set_show_window_size(settings.show_window_size());
    }
    terminal.set_audible_bell(settings.audible_bell());
    terminal.set_cursor_shape(settings.cursor_shape().into());
    terminal.set_cursor_blink_mode(settings.cursor_blink_mode().into());
    terminal.set_text_blink_mode(settings.text_blink_mode().into());
    terminal.set_font(Some(&settings.font_desc()));
    // ptyxis_tab_constructed() binds enable-a11y to VteTerminal:enable-a11y,
    // which decides whether VTE exposes terminal text to screen readers.
    terminal.set_enable_a11y(settings.enable_a11y());
    // Like ptyxis_tab_update_word_char_exceptions().
    set_word_char_exceptions(terminal, settings.word_char_exceptions().as_deref());
    // Like ptyxis_tab_update_padding_cb().
    if settings.disable_padding() {
        terminal.remove_css_class(PADDED_CSS_CLASS);
    } else {
        terminal.add_css_class(PADDED_CSS_CLASS);
    }
}

/// `vte_terminal_set_word_char_exceptions()`, where `None` restores VTE's
/// default set. vte4-rs only wraps it with a `&str`, which cannot express
/// the NULL that an unset Ptyxis key passes.
fn set_word_char_exceptions(terminal: &vte::Terminal, exceptions: Option<&str>) {
    // SAFETY: VTE copies the string; the stash outlives the call.
    unsafe {
        vte::ffi::vte_terminal_set_word_char_exceptions(
            terminal.to_glib_none().0,
            exceptions.to_glib_none().0,
        );
    }
}

/// Applies per-profile behavior (scrollback, scrolling, spacing).
pub fn apply_profile(terminal: &vte::Terminal, profile: &Profile) {
    terminal.set_scrollback_lines(profile.scrollback_lines() as _);
    terminal.set_bold_is_bright(profile.bold_is_bright());
    terminal.set_scroll_on_output(profile.scroll_on_output());
    terminal.set_scroll_on_keystroke(profile.scroll_on_keystroke());
    terminal.set_cell_height_scale(profile.cell_height_scale());
    terminal.set_cell_width_scale(profile.cell_width_scale());
    terminal.set_backspace_binding(profile.backspace_binding().into());
    terminal.set_delete_binding(profile.delete_binding().into());
    terminal.set_cjk_ambiguous_width(profile.cjk_ambiguous_width().columns());
}

/// Applies a palette face, as `ptyxis_terminal_update_colors()` does.
pub fn apply_palette(terminal: &vte::Terminal, palette: &Palette, dark: bool) {
    let face = palette.face(dark);
    let indexed: Vec<_> = face.indexed.iter().collect();
    terminal.set_colors(Some(&face.foreground), Some(&face.background), &indexed);

    match face.cursor_background {
        Some(cursor_bg) if cursor_bg.alpha() > 0.0 => {
            terminal.set_color_cursor(Some(&cursor_bg));
            let cursor_fg = face
                .cursor_foreground
                .filter(|fg| fg.alpha() > 0.0)
                .unwrap_or(face.background);
            terminal.set_color_cursor_foreground(Some(&cursor_fg));
        }
        _ => {
            terminal.set_color_cursor(None);
            terminal.set_color_cursor_foreground(None);
        }
    }
}

/// Looks up the profile's palette (falling back to the default palette)
/// and applies the face for the current light/dark appearance.
pub fn apply_profile_palette(terminal: &vte::Terminal, profile: &Profile) {
    let palette_id = profile.palette_id();
    match palette::lookup_or_default(&palette_id) {
        Some(palette) => {
            let dark = adw::StyleManager::default().is_dark();
            apply_palette(terminal, &palette, dark);
        }
        None => glib::g_warning!("tty-rex", "No palette available for “{palette_id}”"),
    }
}

// The OSC title and directory accessors below are deprecated in favor of
// termprops, but vte4-rs does not wrap the URI termprop getter yet and the
// C implementation uses the same deprecated API, so they are isolated here.

/// The window title set by the child via OSC 0/2.
pub fn window_title(terminal: &vte::Terminal) -> Option<String> {
    #[allow(deprecated)]
    terminal.window_title().map(|title| title.to_string())
}

pub fn connect_window_title_changed(
    terminal: &vte::Terminal,
    f: impl Fn(&vte::Terminal) + 'static,
) -> glib::SignalHandlerId {
    #[allow(deprecated)]
    terminal.connect_window_title_changed(f)
}

/// Current working directory URI reported by the shell via OSC 7.
pub fn current_directory_uri(terminal: &vte::Terminal) -> Option<String> {
    #[allow(deprecated)]
    terminal
        .current_directory_uri()
        .map(|uri| uri.to_string())
        .filter(|uri| !uri.is_empty())
}

/// Current file URI reported via OSC 6.
pub fn current_file_uri(terminal: &vte::Terminal) -> Option<String> {
    #[allow(deprecated)]
    terminal
        .current_file_uri()
        .map(|uri| uri.to_string())
        .filter(|uri| !uri.is_empty())
}

pub fn connect_current_file_uri_changed(
    terminal: &vte::Terminal,
    f: impl Fn(&vte::Terminal) + 'static,
) -> glib::SignalHandlerId {
    #[allow(deprecated)]
    terminal.connect_current_file_uri_changed(f)
}

pub fn connect_current_directory_uri_changed(
    terminal: &vte::Terminal,
    f: impl Fn(&vte::Terminal) + 'static,
) -> glib::SignalHandlerId {
    #[allow(deprecated)]
    terminal.connect_current_directory_uri_changed(f)
}

/// Termprops set by shells inside containers (OSC 777 `container;push`).
const TERMPROP_CONTAINER_NAME: &str = "vte.container.name";
const TERMPROP_CONTAINER_RUNTIME: &str = "vte.container.runtime";

/// Runtime of the container the shell reported running in, if any.
pub fn container_runtime(terminal: &vte::Terminal) -> Option<String> {
    terminal
        .termprop_string(TERMPROP_CONTAINER_RUNTIME)
        .0
        .map(|runtime| runtime.to_string())
        .filter(|runtime| !runtime.is_empty())
}

/// Name of the container the shell reported running in, if any.
pub fn container_name(terminal: &vte::Terminal) -> Option<String> {
    terminal
        .termprop_string(TERMPROP_CONTAINER_NAME)
        .0
        .map(|name| name.to_string())
        .filter(|name| !name.is_empty())
}

/// Calls `f` whenever the reported container name or runtime changes.
pub fn connect_container_changed(
    terminal: &vte::Terminal,
    f: impl Fn(&vte::Terminal) + Clone + 'static,
) -> [glib::SignalHandlerId; 2] {
    [TERMPROP_CONTAINER_NAME, TERMPROP_CONTAINER_RUNTIME].map(|prop| {
        let f = f.clone();
        terminal.connect_termprop_changed(Some(prop), move |terminal, _| f(terminal))
    })
}

/// Termprops set by shells through `vte.sh` before running a command and
/// before showing the prompt again (`VTE_TERMPROP_SHELL_PREEXEC` and
/// `VTE_TERMPROP_SHELL_PRECMD`).
const TERMPROP_SHELL_PREEXEC: &str = "vte.shell.preexec";
const TERMPROP_SHELL_PRECMD: &str = "vte.shell.precmd";

/// Calls `f` when the shell is about to run a command (the
/// `shell-preexec` signal of `PtyxisTerminal`).
pub fn connect_shell_preexec(
    terminal: &vte::Terminal,
    f: impl Fn(&vte::Terminal) + 'static,
) -> glib::SignalHandlerId {
    terminal.connect_termprop_changed(Some(TERMPROP_SHELL_PREEXEC), move |terminal, _| f(terminal))
}

/// Calls `f` when the shell is about to show its prompt (the
/// `shell-precmd` signal of `PtyxisTerminal`).
pub fn connect_shell_precmd(
    terminal: &vte::Terminal,
    f: impl Fn(&vte::Terminal) + 'static,
) -> glib::SignalHandlerId {
    terminal.connect_termprop_changed(Some(TERMPROP_SHELL_PRECMD), move |terminal, _| f(terminal))
}

/// Connects to `commit` without looking at the text. VTE commits whatever
/// it sends the child, which for a dropped file with a non-UTF-8 name is
/// not UTF-8, and vte4-rs's `connect_commit()` hands that text over as a
/// `&str` (asserting it is valid in debug builds, so the callback aborts).
pub fn connect_commit_bytes(
    terminal: &vte::Terminal,
    f: impl Fn() + 'static,
) -> glib::SignalHandlerId {
    terminal.connect_local("commit", false, move |_| {
        f();
        None
    })
}

/// Whether the clipboard holds text to paste
/// (`ptyxis_terminal_can_paste()`).
pub fn can_paste(terminal: &vte::Terminal) -> bool {
    clipboard_has_text(&terminal.clipboard())
}

/// Whether `clipboard` offers text, the check behind [`can_paste`].
pub fn clipboard_has_text(clipboard: &gtk::gdk::Clipboard) -> bool {
    clipboard.formats().contains_type(glib::Type::STRING)
}

/// Pastes the clipboard, then follows it to the bottom of the scrollback
/// when the profile scrolls on keystrokes (`ptyxis_terminal_paste()`).
pub fn paste(terminal: &vte::Terminal) {
    terminal.paste_clipboard();
    if terminal.is_scroll_on_keystroke() {
        scroll_to_bottom(terminal);
    }
}

/// Scrolls the enclosing scrolled window to the end of the scrollback
/// (`ptyxis_terminal_scroll_to_bottom()`).
fn scroll_to_bottom(terminal: &vte::Terminal) {
    let Some(scroller) = terminal
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    let adjustment = scroller.vadjustment();
    let bottom = adjustment.upper() - adjustment.page_size();
    if bottom > adjustment.value() {
        adjustment.set_value(bottom);
    }
}

/// The terminal's grid size in (columns, rows), as Ptyxis reads it with
/// `vte_terminal_get_column_count()` and `vte_terminal_get_row_count()`.
pub fn grid_size(terminal: &vte::Terminal) -> Option<(u32, u32)> {
    grid_size_from_counts(terminal.column_count(), terminal.row_count())
}

/// VTE reports the counts as `glong`; anything that is not a valid `guint`
/// means there is no usable size.
fn grid_size_from_counts(columns: i64, rows: i64) -> Option<(u32, u32)> {
    u32::try_from(columns).ok().zip(u32::try_from(rows).ok())
}

/// Collapses a directory URI for display, replacing `$HOME` with `~`
/// like `ptyxis_path_collapse()`. Non-local URIs are shown verbatim.
pub fn collapse_uri(uri: &str, home: &str) -> String {
    let Ok((path, _)) = glib::filename_from_uri(uri) else {
        return uri.to_owned();
    };
    let path = path.to_string_lossy();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.into_owned();
    }
    match path.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_size_from_vte_counts() {
        let cases = [
            ((80, 24), Some((80, 24))),
            ((0, 0), Some((0, 0))),
            ((-1, 24), None),
            ((80, -1), None),
            ((i64::from(u32::MAX) + 1, 24), None),
        ];
        for ((columns, rows), expected) in cases {
            assert_eq!(
                grid_size_from_counts(columns, rows),
                expected,
                "{columns}x{rows}"
            );
        }
    }

    #[test]
    fn zoom_scales_match_c_table() {
        let cases = [
            (ZoomLevel::DEFAULT, 1.0),
            (ZoomLevel(1), 1.095445115010332),
            (ZoomLevel(2), 1.2),
            (ZoomLevel(-2), 1.0 / 1.2),
            (ZoomLevel::MAX, 1.2_f64.powi(7)),
            (ZoomLevel::MIN, 1.2_f64.powi(-7)),
        ];
        for (level, expected) in cases {
            assert!(
                (level.font_scale() - expected).abs() < 1e-9,
                "{level:?}: {} != {expected}",
                level.font_scale()
            );
        }
    }

    #[test]
    fn zoom_labels() {
        let cases = [
            (ZoomLevel::DEFAULT, "100%"),
            (ZoomLevel(2), "120%"),
            (ZoomLevel(1), "110%"),
            (ZoomLevel(-2), "83%"),
        ];
        for (level, expected) in cases {
            assert_eq!(level.label(), expected, "{level:?}");
        }
    }

    #[test]
    fn zoom_is_clamped() {
        assert_eq!(ZoomLevel::MAX.zoom_in(), ZoomLevel::MAX);
        assert_eq!(ZoomLevel::MIN.zoom_out(), ZoomLevel::MIN);
        assert_eq!(ZoomLevel::DEFAULT.zoom_in().zoom_out(), ZoomLevel::DEFAULT);
    }

    #[test]
    fn zoom_sensitivity() {
        // (level, can zoom in, can zoom out, can reset)
        let cases = [
            (ZoomLevel::DEFAULT, true, true, false),
            (ZoomLevel(1), true, true, true),
            (ZoomLevel(-1), true, true, true),
            (ZoomLevel(13), true, true, true),
            (ZoomLevel::MAX, false, true, true),
            (ZoomLevel::MIN, true, false, true),
        ];
        for (level, zoom_in, zoom_out, reset) in cases {
            assert_eq!(level.can_zoom_in(), zoom_in, "{level:?}");
            assert_eq!(level.can_zoom_out(), zoom_out, "{level:?}");
            assert_eq!(level.can_reset(), reset, "{level:?}");
        }
    }

    #[test]
    fn zoom_scrolled() {
        let cases = [
            (ZoomLevel::DEFAULT, -1.0, ZoomLevel(1)),
            (ZoomLevel::DEFAULT, -0.25, ZoomLevel(1)),
            (ZoomLevel::DEFAULT, 1.0, ZoomLevel(-1)),
            (ZoomLevel::DEFAULT, 0.0, ZoomLevel::DEFAULT),
            (ZoomLevel::MAX, -1.0, ZoomLevel::MAX),
            (ZoomLevel::MIN, 1.0, ZoomLevel::MIN),
        ];
        for (level, dy, expected) in cases {
            assert_eq!(level.scrolled(dy), expected, "{level:?} dy={dy}");
        }
    }

    #[test]
    fn zoom_ptyxis_enum() {
        // PTYXIS_ZOOM_LEVEL_MINUS_14 = 1, DEFAULT = 15, PLUS_14 = 29.
        let cases = [
            (0, None),
            (1, Some(ZoomLevel::MIN)),
            (14, Some(ZoomLevel(-1))),
            (15, Some(ZoomLevel::DEFAULT)),
            (29, Some(ZoomLevel::MAX)),
            (30, None),
            (u32::MAX, None),
        ];
        for (value, expected) in cases {
            assert_eq!(ZoomLevel::from_ptyxis_enum(value), expected, "{value}");
            if let Some(level) = expected {
                assert_eq!(level.to_ptyxis_enum(), value);
            }
        }
    }

    #[test]
    fn uri_collapsing() {
        let cases = [
            ("file:///home/user", "/home/user", "~"),
            ("file:///home/user/src/ptyxis", "/home/user", "~/src/ptyxis"),
            ("file:///home/user/src", "/home/user/", "~/src"),
            ("file:///home/username", "/home/user", "/home/username"),
            ("file:///tmp", "/home/user", "/tmp"),
            ("file:///tmp", "", "/tmp"),
            ("sftp://host/tmp", "/home/user", "sftp://host/tmp"),
        ];
        for (uri, home, expected) in cases {
            assert_eq!(collapse_uri(uri, home), expected, "{uri}");
        }
    }
}
