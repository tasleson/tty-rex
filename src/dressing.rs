// dressing.rs
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

//! Window colors derived from the selected tab's palette
//! (`src/ptyxis-window-dressing.c`).
//!
//! The C build keeps one dressing per window and rebinds it to the active
//! tab's profile. Here every tab owns a [`Dressing`] whose rules are keyed
//! on a class unique to that tab, and a window carries the class of its
//! selected tab. A tab then restyles whichever window shows it from the
//! updates it already receives, and moving tabs between windows needs no
//! extra bookkeeping.
//!
//! Colors that the C CSS computes with GTK's `mix()` and `alpha()` are
//! computed here instead, since those functions are deprecated.
//!
//! A color the user gives a tab paints the top bar like a leader
//! highlight. This is a tty-rex addition; Ptyxis has no tab colors.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};

use gtk::gdk;

use crate::leader::LeaderKind;
use crate::palette::{self, Face, HighlightColors, Scarf};
use crate::window::{VISUAL_BELL_CLASS, WINDOW_CONTENTS_CLASS};

/// Prefix of the CSS class that marks the window showing a given tab.
const CLASS_PREFIX: &str = "ptyxis-tab-";
/// Popovers stay legible however transparent the window is.
const MIN_POPOVER_OPACITY: f64 = 0.85;
/// How far the backdrop title bar fades towards the terminal background.
const BACKDROP_FOREGROUND_MIX: f32 = 0.025;
const BACKDROP_BACKGROUND_MIX: f32 = 0.99;
/// How far the hover-URI label background leans towards the title bar
/// foreground.
const HOVER_URI_FOREGROUND_MIX: f32 = 0.05;
/// Share of the background in the palette-derived accent color.
const ACCENT_BACKGROUND_MIX: f32 = 0.15;
/// Backdrop of thumbnail close buttons per state (`ptyxis-window-dressing.c`).
const CLOSE_BUTTON_ALPHAS: [(&str, f64); 3] = [("", 0.15), (":hover", 0.25), (":active", 0.55)];
/// Palette color the accent derives from (blue).
const ACCENT_BASE_INDEX: usize = 4;

/// Everything the window colors depend on.
#[derive(Debug, Clone, Copy)]
pub struct DressingStyle<'a> {
    pub face: &'a Face,
    /// The appearance `face` was chosen for.
    pub dark: bool,
    /// Profile opacity of the window background.
    pub opacity: f64,
    /// Process leading the terminal, highlighted when `visual_process_leader`.
    pub leader: LeaderKind,
    pub visual_process_leader: bool,
    pub highlight_colors: HighlightColors,
    pub use_system_accent: bool,
    /// Color the user gave the tab (a tty-rex addition), painting the top
    /// bar unless a leader highlight does.
    pub tab_color: Option<gdk::RGBA>,
}

fn with_alpha(color: &gdk::RGBA, alpha: f64) -> gdk::RGBA {
    // GTK colors are f32; the precision lost here is invisible.
    let alpha = (f64::from(color.alpha()) * alpha) as f32;
    gdk::RGBA::new(color.red(), color.green(), color.blue(), alpha)
}

fn rule(css: &mut String, selector: &str, color: &gdk::RGBA, background: &gdk::RGBA) {
    // Writing to a String cannot fail.
    let _ = writeln!(
        css,
        "{selector} {{ color: {color}; background-color: {background}; }}"
    );
}

/// The window CSS for the window carrying `css_class`, following
/// `ptyxis_window_dressing_update()`. Selectors are adapted to the port's
/// widget tree; the rule for the size label, which the port does not have
/// yet, is left out.
pub fn dressing_css(css_class: &str, style: &DressingStyle<'_>) -> String {
    let face = style.face;
    let window = format!("window.{css_class}");
    let contents = format!("{window} toolbarview.{WINDOW_CONTENTS_CLASS}");
    // Colors go on the top-bar revealer itself, under libadwaita's devel
    // stripes. Its window handle child is drawn at half opacity while the
    // window is in the backdrop, so painting that instead would hide the
    // stripes while focused and show them through a faded color otherwise.
    let top_bar = format!("{contents} > revealer.top-bar");
    // Everything but dialogs sits in the tab overview (the C build's
    // `.window-contents`); `toolbarview.overview` is its own grid view.
    let overview = format!("{window} taboverview");
    let grid = format!("{overview} toolbarview.overview");
    let popover_alpha = style.opacity.max(MIN_POPOVER_OPACITY);
    let popover_background = with_alpha(&face.titlebar_background, popover_alpha);
    let mut css = String::new();

    // Terminals leave their background to the window (as the C build does
    // by dropping VTE's background node), so themes must not paint one.
    let _ = writeln!(css, "{window} vte-terminal {{ background: none; }}");
    rule(
        &mut css,
        &window,
        &face.foreground,
        &with_alpha(&face.background, style.opacity),
    );
    let _ = writeln!(
        css,
        "{window}.fullscreen {{ background-color: {}; }}",
        face.background
    );
    rule(
        &mut css,
        &format!("{overview} popover > contents"),
        &face.titlebar_foreground,
        &popover_background,
    );
    let _ = writeln!(
        css,
        "{overview} popover > arrow {{ background-color: {popover_background}; }}"
    );
    // The hover-URI label floats over the terminal in a tinted titlebar
    // color, as opaque as popovers.
    rule(
        &mut css,
        &format!("{overview} vte-terminal > revealer.hover-uri label"),
        &face.titlebar_foreground,
        &with_alpha(
            &palette::mix(
                &face.titlebar_foreground,
                &face.titlebar_background,
                HOVER_URI_FOREGROUND_MIX,
            ),
            popover_alpha,
        ),
    );
    rule(
        &mut css,
        &format!("{contents} banner > revealer > widget"),
        &face.visual_bell.foreground,
        &face.visual_bell.background,
    );
    rule(
        &mut css,
        &top_bar,
        &face.titlebar_foreground,
        &face.titlebar_background,
    );

    // Thumbnails show terminals that leave their background to the
    // window, so the cards provide it. As in C, they use the selected
    // tab's palette rather than each tab's own.
    rule(
        &mut css,
        &format!("{grid} overlay.card"),
        &face.foreground,
        &face.background,
    );
    let _ = writeln!(
        css,
        "{grid} tabthumbnail .icon-title-box {{ color: {}; }}",
        face.foreground
    );
    rule(
        &mut css,
        &grid,
        &face.titlebar_foreground,
        &face.titlebar_background,
    );
    let close_button = format!("{overview} tabthumbnail .tab-close-button");
    for (state, alpha) in CLOSE_BUTTON_ALPHAS {
        rule(
            &mut css,
            &format!("{close_button}{state} image"),
            &face.foreground,
            &with_alpha(&face.foreground, alpha),
        );
    }

    let highlights = &style.highlight_colors;
    let leader: Option<Scarf> = match style.leader {
        _ if !style.visual_process_leader => None,
        LeaderKind::Superuser => Some((face.superuser, highlights.superuser)),
        LeaderKind::Remote => Some((face.remote, highlights.remote)),
        LeaderKind::Unknown | LeaderKind::Container => None,
    }
    .map(|(scarf, chosen)| chosen.map_or(scarf, Scarf::on_background));
    // The leader highlight warns about the shell, so it outranks the tab
    // color, which in turn is painted like a highlight.
    let top_bar_scarf = leader.or_else(|| style.tab_color.map(Scarf::on_background));
    match top_bar_scarf {
        // Header bars in a top bar are transparent, so this covers them.
        Some(scarf) => rule(&mut css, &top_bar, &scarf.foreground, &scarf.background),
        // The highlight outranks the backdrop fade in the C rules, so the
        // fade only applies without one (or a tab color).
        None => rule(
            &mut css,
            &format!("{window}:backdrop toolbarview.{WINDOW_CONTENTS_CLASS} > revealer.top-bar"),
            &palette::mix(&face.background, &face.foreground, BACKDROP_FOREGROUND_MIX),
            &palette::mix(&face.background, &face.foreground, BACKDROP_BACKGROUND_MIX),
        ),
    }

    // The bell flashes the area behind the header bar. As in C, the
    // keyframes are named per appearance, so switching between light and
    // dark defines new ones instead of redefining the running animation.
    let keyframes = format!(
        "visual-bell-{css_class}-{}",
        if style.dark { "dark" } else { "light" }
    );
    let _ = writeln!(
        css,
        "{contents} box.{VISUAL_BELL_CLASS} headerbar {{ background-color: transparent; }}\n\
         {contents} box.{VISUAL_BELL_CLASS} {{ animation: {keyframes} 0.3s ease-out; }}\n\
         @keyframes {keyframes} {{ 50% {{ background-color: {}; color: {}; }} }}",
        face.visual_bell.background, face.visual_bell.foreground
    );

    if !style.use_system_accent {
        let accent_foreground = if style.dark {
            face.titlebar_foreground
        } else {
            face.titlebar_background
        };
        let accent_background = palette::mix(
            &face.background,
            &face.indexed[ACCENT_BASE_INDEX],
            ACCENT_BACKGROUND_MIX,
        );
        let _ = writeln!(
            css,
            "{window} {{ --accent-fg-color: {accent_foreground}; --accent-bg-color: {accent_background}; }}"
        );
    }
    css
}

/// A CSS provider coloring the window while its tab is the selected one.
pub struct Dressing {
    css_class: String,
    provider: gtk::CssProvider,
    /// The CSS last loaded, to skip needless restyling.
    css: String,
}

impl Dressing {
    pub fn new() -> Self {
        static NEXT_ID: AtomicU32 = AtomicU32::new(1);
        let provider = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
            );
        }
        Self {
            css_class: format!("{CLASS_PREFIX}{}", NEXT_ID.fetch_add(1, Ordering::Relaxed)),
            provider,
            css: String::new(),
        }
    }

    /// The class a window carries while showing this tab.
    pub fn css_class(&self) -> &str {
        &self.css_class
    }

    pub fn update(&mut self, style: &DressingStyle<'_>) {
        let css = dressing_css(&self.css_class, style);
        if css != self.css {
            self.provider.load_from_string(&css);
            self.css = css;
        }
    }
}

impl Default for Dressing {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Dressing {
    fn drop(&mut self) {
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_remove_provider_for_display(&display, &self.provider);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASS: &str = "ptyxis-tab-1";
    const TOP_BAR: &str = "window.ptyxis-tab-1 toolbarview.window-contents > revealer.top-bar";
    const BACKDROP: &str =
        "window.ptyxis-tab-1:backdrop toolbarview.window-contents > revealer.top-bar";
    /// Nothing may paint the window handle: libadwaita fades it in the
    /// backdrop, which would let the devel stripes show through.
    const WINDOW_HANDLE: &str = "> windowhandle";

    fn face() -> Face {
        palette::lookup_or_default(palette::DEFAULT_PALETTE_ID)
            .unwrap()
            .face(true)
            .clone()
    }

    fn style(face: &Face) -> DressingStyle<'_> {
        DressingStyle {
            face,
            dark: true,
            opacity: 1.0,
            leader: LeaderKind::Unknown,
            visual_process_leader: true,
            highlight_colors: HighlightColors::default(),
            use_system_accent: false,
            tab_color: None,
        }
    }

    fn rule_for(selector: &str, color: &gdk::RGBA, background: &gdk::RGBA) -> String {
        format!("{selector} {{ color: {color}; background-color: {background}; }}\n")
    }

    #[test]
    fn base_rules() {
        let face = face();
        let css = dressing_css(CLASS, &style(&face));
        for expected in [
            "window.ptyxis-tab-1 vte-terminal { background: none; }\n".to_owned(),
            rule_for("window.ptyxis-tab-1", &face.foreground, &face.background),
            rule_for(
                TOP_BAR,
                &face.titlebar_foreground,
                &face.titlebar_background,
            ),
            rule_for(
                "window.ptyxis-tab-1 toolbarview.window-contents banner > revealer > widget",
                &face.visual_bell.foreground,
                &face.visual_bell.background,
            ),
        ] {
            assert!(css.contains(&expected), "missing {expected:?} in\n{css}");
        }
        assert!(css.contains(BACKDROP), "{css}");
        assert!(!css.contains(WINDOW_HANDLE), "{css}");
        assert!(css.contains("--accent-bg-color"), "{css}");
    }

    #[test]
    fn tab_overview() {
        let face = face();
        let css = dressing_css(CLASS, &style(&face));
        let grid = "window.ptyxis-tab-1 taboverview toolbarview.overview";
        let close = "window.ptyxis-tab-1 taboverview tabthumbnail .tab-close-button";
        for expected in [
            rule_for(
                &format!("{grid} overlay.card"),
                &face.foreground,
                &face.background,
            ),
            format!(
                "{grid} tabthumbnail .icon-title-box {{ color: {}; }}\n",
                face.foreground
            ),
            rule_for(grid, &face.titlebar_foreground, &face.titlebar_background),
            rule_for(
                &format!("{close} image"),
                &face.foreground,
                &with_alpha(&face.foreground, 0.15),
            ),
            rule_for(
                &format!("{close}:hover image"),
                &face.foreground,
                &with_alpha(&face.foreground, 0.25),
            ),
            rule_for(
                &format!("{close}:active image"),
                &face.foreground,
                &with_alpha(&face.foreground, 0.55),
            ),
        ] {
            assert!(css.contains(&expected), "missing {expected:?} in\n{css}");
        }
    }

    #[test]
    fn visual_bell_flash() {
        let face = face();
        for (dark, name) in [
            (true, "visual-bell-ptyxis-tab-1-dark"),
            (false, "visual-bell-ptyxis-tab-1-light"),
        ] {
            let css = dressing_css(
                CLASS,
                &DressingStyle {
                    dark,
                    ..style(&face)
                },
            );
            let flash = format!(
                "@keyframes {name} {{ 50% {{ background-color: {}; color: {}; }} }}",
                face.visual_bell.background, face.visual_bell.foreground
            );
            for expected in [
                format!(
                    "window.ptyxis-tab-1 toolbarview.window-contents box.visual-bell {{ animation: {name} 0.3s ease-out; }}"
                ),
                flash,
                "window.ptyxis-tab-1 toolbarview.window-contents box.visual-bell headerbar { background-color: transparent; }".to_owned(),
            ] {
                assert!(css.contains(&expected), "missing {expected:?} in\n{css}");
            }
        }
    }

    #[test]
    fn opacity_applies_to_window_and_popovers() {
        let face = face();
        let cases = [(1.0, 1.0), (0.5, MIN_POPOVER_OPACITY), (0.9, 0.9)];
        for (opacity, popover_alpha) in cases {
            let css = dressing_css(
                CLASS,
                &DressingStyle {
                    opacity,
                    ..style(&face)
                },
            );
            let window = rule_for(
                "window.ptyxis-tab-1",
                &face.foreground,
                &with_alpha(&face.background, opacity),
            );
            let popover = rule_for(
                "window.ptyxis-tab-1 taboverview popover > contents",
                &face.titlebar_foreground,
                &with_alpha(&face.titlebar_background, popover_alpha),
            );
            let hover_uri = rule_for(
                "window.ptyxis-tab-1 taboverview vte-terminal > revealer.hover-uri label",
                &face.titlebar_foreground,
                &with_alpha(
                    &palette::mix(
                        &face.titlebar_foreground,
                        &face.titlebar_background,
                        HOVER_URI_FOREGROUND_MIX,
                    ),
                    popover_alpha,
                ),
            );
            assert!(css.contains(&window), "{opacity}: {css}");
            assert!(css.contains(&popover), "{opacity}: {css}");
            assert!(css.contains(&hover_uri), "{opacity}: {css}");
        }
    }

    #[test]
    fn leader_highlight() {
        let face = face();
        let cases = [
            (LeaderKind::Superuser, true, Some(face.superuser)),
            (LeaderKind::Remote, true, Some(face.remote)),
            (LeaderKind::Container, true, None),
            (LeaderKind::Unknown, true, None),
            (LeaderKind::Superuser, false, None),
        ];
        for (leader, enabled, scarf) in cases {
            let css = dressing_css(
                CLASS,
                &DressingStyle {
                    leader,
                    visual_process_leader: enabled,
                    ..style(&face)
                },
            );
            let context = format!("{leader:?} enabled={enabled}");
            assert!(!css.contains(WINDOW_HANDLE), "{context}: {css}");
            match scarf {
                Some(scarf) => {
                    let expected = rule_for(TOP_BAR, &scarf.foreground, &scarf.background);
                    assert!(css.contains(&expected), "{context}: {css}");
                    assert!(!css.contains(BACKDROP), "{context}: {css}");
                }
                None => assert!(css.contains(BACKDROP), "{context}: {css}"),
            }
        }
    }

    #[test]
    fn chosen_highlight_colors() {
        let face = face();
        let (red, yellow) = (gdk::RGBA::RED, gdk::RGBA::new(1.0, 0.9, 0.2, 1.0));
        let chosen = HighlightColors {
            superuser: Some(red),
            remote: Some(yellow),
        };
        let only_superuser = HighlightColors {
            superuser: Some(red),
            remote: None,
        };
        let cases = [
            (
                LeaderKind::Superuser,
                chosen,
                Some(Scarf::on_background(red)),
            ),
            (
                LeaderKind::Remote,
                chosen,
                Some(Scarf::on_background(yellow)),
            ),
            (LeaderKind::Remote, only_superuser, Some(face.remote)),
            (LeaderKind::Container, chosen, None),
        ];
        for (leader, highlight_colors, scarf) in cases {
            let css = dressing_css(
                CLASS,
                &DressingStyle {
                    leader,
                    highlight_colors,
                    ..style(&face)
                },
            );
            let context = format!("{leader:?} {highlight_colors:?}");
            match scarf {
                Some(scarf) => {
                    let expected = rule_for(TOP_BAR, &scarf.foreground, &scarf.background);
                    assert!(css.contains(&expected), "{context}: {css}");
                }
                None => assert!(css.contains(BACKDROP), "{context}: {css}"),
            }
        }
        // Chosen colors still only show while highlighting is enabled.
        let css = dressing_css(
            CLASS,
            &DressingStyle {
                leader: LeaderKind::Superuser,
                visual_process_leader: false,
                highlight_colors: chosen,
                ..style(&face)
            },
        );
        assert!(css.contains(BACKDROP), "{css}");
        assert!(!css.contains(&red.to_str().to_string()), "{css}");
    }

    #[test]
    fn tab_color() {
        let face = face();
        let (teal, red) = (gdk::RGBA::new(0.1, 0.6, 0.6, 1.0), gdk::RGBA::RED);
        let chosen = HighlightColors {
            superuser: Some(red),
            remote: None,
        };
        let cases = [
            // (leader, highlighting enabled, chosen highlights, top bar)
            (LeaderKind::Unknown, true, None, Scarf::on_background(teal)),
            (
                LeaderKind::Container,
                true,
                None,
                Scarf::on_background(teal),
            ),
            (LeaderKind::Superuser, true, None, face.superuser),
            (LeaderKind::Remote, true, None, face.remote),
            (
                LeaderKind::Superuser,
                true,
                Some(chosen),
                Scarf::on_background(red),
            ),
            // Without the highlight the tab color shows even under sudo.
            (
                LeaderKind::Superuser,
                false,
                None,
                Scarf::on_background(teal),
            ),
        ];
        for (leader, enabled, highlights, scarf) in cases {
            let css = dressing_css(
                CLASS,
                &DressingStyle {
                    leader,
                    visual_process_leader: enabled,
                    highlight_colors: highlights.unwrap_or_default(),
                    tab_color: Some(teal),
                    ..style(&face)
                },
            );
            let context = format!("{leader:?} enabled={enabled} {highlights:?}");
            let expected = rule_for(TOP_BAR, &scarf.foreground, &scarf.background);
            assert!(css.contains(&expected), "{context}: {css}");
            // The tab color, like a highlight, outranks the backdrop fade.
            assert!(!css.contains(BACKDROP), "{context}: {css}");
            assert!(!css.contains(WINDOW_HANDLE), "{context}: {css}");
        }
    }

    #[test]
    fn accent_follows_palette_unless_system() {
        let face = face();
        let css = dressing_css(
            CLASS,
            &DressingStyle {
                use_system_accent: true,
                ..style(&face)
            },
        );
        assert!(!css.contains("--accent"), "{css}");

        for (dark, foreground) in [
            (true, face.titlebar_foreground),
            (false, face.titlebar_background),
        ] {
            let css = dressing_css(
                CLASS,
                &DressingStyle {
                    dark,
                    ..style(&face)
                },
            );
            // 85% palette blue over 15% background, spelled out so a
            // swapped mix() argument order cannot pass.
            let (accent, bg) = (face.indexed[4], face.background);
            let channel = |a: f32, b: f32| 0.85 * a + 0.15 * b;
            let background = gdk::RGBA::new(
                channel(accent.red(), bg.red()),
                channel(accent.green(), bg.green()),
                channel(accent.blue(), bg.blue()),
                1.0,
            );
            let expected = format!(
                "window.ptyxis-tab-1 {{ --accent-fg-color: {foreground}; --accent-bg-color: {background}; }}"
            );
            assert!(css.contains(&expected), "dark={dark}: {css}");
        }
    }

    #[test]
    fn transparency_helper() {
        let color = gdk::RGBA::new(0.1, 0.2, 0.3, 0.5);
        assert_eq!(with_alpha(&color, 0.5), gdk::RGBA::new(0.1, 0.2, 0.3, 0.25));
        assert_eq!(with_alpha(&color, 1.0), color);
    }
}
