// title_dialog.rs
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

//! The "Set Title" dialog editing a tab's title prefix
//! (`src/ptyxis-title-dialog.c`).
//!
//! The C dialog binds its rows bidirectionally to the tab's `title-prefix`
//! and (inverted) `ignore-osc-title` properties, so edits apply as they are
//! typed and there is nothing to confirm. Here the rows report changes
//! through callbacks and the owning tab pushes outside changes back with
//! [`TitleDialog::set_include_process_title`].

use adw::prelude::*;

/// Also the label of the menu items opening the dialog.
pub const SET_TITLE_LABEL: &str = "Set Title";
const ENTRY_TITLE: &str = "Title";
const INCLUDE_PROCESS_TITLE: &str = "Include Process Title";
const INCLUDE_PROCESS_TITLE_SUBTITLE: &str = "Append title from Shell application";
/// Margins of the preferences group in `src/ptyxis-title-dialog.ui`.
const MARGIN: i32 = 18;
const MARGIN_TOP: i32 = 9;

pub struct TitleDialog {
    dialog: adw::Dialog,
    include_process_title: adw::SwitchRow,
}

impl TitleDialog {
    /// Builds the dialog showing `prefix`; `on_prefix_changed` and
    /// `on_include_changed` are called for every edit, and `on_closed`
    /// once the dialog has gone away.
    pub fn new(
        prefix: &str,
        include_process_title: bool,
        on_prefix_changed: impl Fn(String) + 'static,
        on_include_changed: impl Fn(bool) + 'static,
        on_closed: impl Fn() + 'static,
    ) -> Self {
        let entry = adw::EntryRow::builder()
            .title(ENTRY_TITLE)
            .text(prefix)
            .build();
        let include_row = adw::SwitchRow::builder()
            .title(INCLUDE_PROCESS_TITLE)
            .subtitle(INCLUDE_PROCESS_TITLE_SUBTITLE)
            .active(include_process_title)
            .build();
        let group = adw::PreferencesGroup::builder()
            .margin_top(MARGIN_TOP)
            .margin_bottom(MARGIN)
            .margin_start(MARGIN)
            .margin_end(MARGIN)
            .build();
        group.add(&entry);
        group.add(&include_row);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&adw::HeaderBar::new());
        toolbar_view.set_content(Some(&group));

        let dialog = adw::Dialog::builder()
            .title(SET_TITLE_LABEL)
            .presentation_mode(adw::DialogPresentationMode::Floating)
            .child(&toolbar_view)
            .build();

        entry.connect_changed(move |entry| on_prefix_changed(entry.text().into()));
        let weak_dialog = dialog.downgrade();
        entry.connect_entry_activated(move |_| {
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.close();
            }
        });
        include_row.connect_active_notify(move |row| on_include_changed(row.is_active()));
        dialog.connect_closed(move |_| on_closed());

        Self {
            dialog,
            include_process_title: include_row,
        }
    }

    pub fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.dialog.present(Some(parent));
    }

    /// Follows a change made elsewhere, like the bidirectional C binding.
    pub fn set_include_process_title(&self, include: bool) {
        if self.include_process_title.is_active() != include {
            self.include_process_title.set_active(include);
        }
    }

    /// Closes the dialog even if it is being interacted with.
    pub fn force_close(&self) {
        self.dialog.force_close();
    }
}
