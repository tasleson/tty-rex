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
use vte::prelude::*;

use crate::palette::{self, Palette};
use crate::profile::Profile;
use crate::settings::AppSettings;

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
}

/// Creates a terminal configured like the one in `src/ptyxis-tab.ui`.
pub fn create() -> vte::Terminal {
    let terminal = vte::Terminal::new();
    terminal.set_enable_fallback_scrolling(false);
    terminal.set_scroll_unit_is_pixels(true);
    terminal.set_allow_hyperlink(true);
    // The window paints the background (see `dressing`), which lets
    // profile opacity show through like the C build.
    terminal.set_clear_background(false);
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);
    terminal
}

/// Applies application-wide settings (font, cursor, bell).
pub fn apply_app_settings(terminal: &vte::Terminal, settings: &AppSettings) {
    terminal.set_audible_bell(settings.audible_bell());
    terminal.set_cursor_shape(settings.cursor_shape().into());
    terminal.set_cursor_blink_mode(settings.cursor_blink_mode().into());
    terminal.set_text_blink_mode(settings.text_blink_mode().into());
    terminal.set_font(Some(&settings.font_desc()));
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
        None => glib::g_warning!("Ptyxis", "No palette available for “{palette_id}”"),
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
