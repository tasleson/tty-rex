// theme_selector.rs
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

//! The light/dark style switcher in the primary menu
//! (`src/ptyxis-theme-selector.c` / `.ui`), styled by `src/style.css`.

use gtk::prelude::*;

use crate::settings::InterfaceStyle;
use crate::signal_guard::{SignalGuard, drop_on_destroy};

const CSS_NAME: &str = "themeselector";
const BUTTON_CSS_CLASS: &str = "theme-selector";
const DARK_CSS_CLASS: &str = "dark";

/// The action target for a style: the `AdwColorScheme` nick, since the C
/// window drives a property action on an `AdwColorScheme` property.
pub fn color_scheme_nick(style: InterfaceStyle) -> &'static str {
    match style {
        InterfaceStyle::System => "default",
        InterfaceStyle::Light => "force-light",
        InterfaceStyle::Dark => "force-dark",
    }
}

pub fn style_for_color_scheme_nick(nick: &str) -> Option<InterfaceStyle> {
    InterfaceStyle::ALL
        .iter()
        .copied()
        .find(|style| color_scheme_nick(*style) == nick)
}

/// Builds the selector; its buttons activate `action_name` with a
/// color scheme nick as target.
pub fn theme_selector(action_name: &str) -> gtk::Box {
    let root = gtk::Box::builder().css_name(CSS_NAME).hexpand(true).build();
    let row = gtk::Box::builder()
        .hexpand(true)
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    root.append(&row);

    let button = |class: &str, style: InterfaceStyle, tooltip: &str, label: &str| {
        let button = gtk::CheckButton::builder()
            .css_classes([BUTTON_CSS_CLASS, class])
            .hexpand(true)
            .halign(gtk::Align::Center)
            .focus_on_click(false)
            .action_name(action_name)
            .action_target(&color_scheme_nick(style).to_variant())
            .tooltip_text(tooltip)
            .build();
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        row.append(&button);
        button
    };
    let follow = button(
        "follow",
        InterfaceStyle::System,
        "Follow System Style",
        "Follow system style",
    );
    let light = button("light", InterfaceStyle::Light, "Light Style", "Light style");
    let dark = button("dark", InterfaceStyle::Dark, "Dark Style", "Dark style");
    follow.set_group(Some(&light));
    dark.set_group(Some(&light));

    let style_manager = adw::StyleManager::default();
    let update_follow = {
        let follow = follow.downgrade();
        move |manager: &adw::StyleManager| {
            if let Some(follow) = follow.upgrade() {
                follow.set_visible(manager.system_supports_color_schemes());
            }
        }
    };
    update_follow(&style_manager);
    let update_dark = {
        let root = root.downgrade();
        move |manager: &adw::StyleManager| {
            if let Some(root) = root.upgrade() {
                if manager.is_dark() {
                    root.add_css_class(DARK_CSS_CLASS);
                } else {
                    root.remove_css_class(DARK_CSS_CLASS);
                }
            }
        }
    };
    update_dark(&style_manager);
    let handlers = [
        style_manager.connect_system_supports_color_schemes_notify(update_follow),
        style_manager.connect_dark_notify(update_dark),
    ]
    .map(|id| SignalGuard::new(&style_manager, id));
    drop_on_destroy(&root, handlers);
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_scheme_round_trip() {
        for style in InterfaceStyle::ALL {
            assert_eq!(
                style_for_color_scheme_nick(color_scheme_nick(*style)),
                Some(*style)
            );
        }
        assert_eq!(style_for_color_scheme_nick("prefer-dark"), None);
    }
}
