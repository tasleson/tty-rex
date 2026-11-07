// profile_dialog.rs
//
// Copyright 2025 Mikel Olasagasti Uranga <mikel@olasagasti.info>
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

//! The "Change Profile" dialog applying another profile to a tab
//! (`src/ptyxis-profile-dialog.c`). Like the C dialog, picking a row
//! applies it at once and there is nothing to confirm.

use adw::prelude::*;

use crate::profile::Profile;

/// Also the label of the menu item opening the dialog.
pub const CHANGE_PROFILE_LABEL: &str = "Change Profile";
const ROW_TITLE: &str = "Profile";
const ROW_SUBTITLE: &str = "Select a profile to apply to this terminal";
/// Margins of the preferences group in `src/ptyxis-profile-dialog.ui`.
const MARGIN: i32 = 18;
const MARGIN_TOP: i32 = 9;

pub struct ProfileDialog {
    dialog: adw::Dialog,
}

impl ProfileDialog {
    /// Builds the dialog listing `profiles` with the one whose uuid is
    /// `current_uuid` selected (the first when it is not listed, as
    /// `find_profile_index()` does); `on_selected` is called with the
    /// profile picked and `on_closed` once the dialog has gone away.
    pub fn new(
        profiles: Vec<Profile>,
        current_uuid: &str,
        on_selected: impl Fn(Profile) + 'static,
        on_closed: impl Fn() + 'static,
    ) -> Self {
        let labels: Vec<String> = profiles.iter().map(Profile::label).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected = profiles
            .iter()
            .position(|profile| profile.uuid() == current_uuid)
            .unwrap_or(0);
        let combo = adw::ComboRow::builder()
            .title(ROW_TITLE)
            .subtitle(ROW_SUBTITLE)
            .model(&gtk::StringList::new(&labels))
            .selected(selected as u32)
            .build();
        let group = adw::PreferencesGroup::builder()
            .margin_top(MARGIN_TOP)
            .margin_bottom(MARGIN)
            .margin_start(MARGIN)
            .margin_end(MARGIN)
            .build();
        group.add(&combo);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&adw::HeaderBar::new());
        toolbar_view.set_content(Some(&group));

        let dialog = adw::Dialog::builder()
            .title(CHANGE_PROFILE_LABEL)
            .presentation_mode(adw::DialogPresentationMode::Floating)
            .child(&toolbar_view)
            .build();

        combo.connect_selected_notify(move |combo| {
            if let Some(profile) = profiles.get(combo.selected() as usize) {
                on_selected(profile.clone());
            }
        });
        dialog.connect_closed(move |_| on_closed());

        Self { dialog }
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.dialog.present(Some(parent));
    }

    /// Closes the dialog even if it is being interacted with.
    pub fn force_close(&self) {
        self.dialog.force_close();
    }
}
