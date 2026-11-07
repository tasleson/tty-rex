// close_dialog.rs
//
// Copyright 2021 Christian Hergert <christian@sourceandstack.com>
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

//! Asks before closing tabs that still run a program
//! (`src/ptyxis-close-dialog.c`).
//!
//! The C dialog takes the tabs themselves and force-quits them when the
//! user confirms. Here the caller lists what to show and does the closing
//! in its response callback, since it owns the tabs.

use std::cell::Cell;

use adw::prelude::*;

const HEADING: &str = "Close Window?";
const BODY: &str = "Some processes are still running.";
const CANCEL_RESPONSE: &str = "cancel";
const CANCEL_LABEL: &str = "_Cancel";
const DISCARD_RESPONSE: &str = "discard";
const CLOSE_LABEL: &str = "_Close";
const CLOSE_ALL_LABEL: &str = "_Close All";
/// Longer titles are cut to this many characters.
const MAX_TITLE_CHARS: usize = 200;

/// One tab listed in the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub subtitle: String,
}

/// The confirming button's label, `g_dngettext()` style.
fn discard_label(n_rows: usize) -> &'static str {
    if n_rows == 1 {
        CLOSE_LABEL
    } else {
        CLOSE_ALL_LABEL
    }
}

/// Cuts `title` to [`MAX_TITLE_CHARS`] characters.
fn truncate_title(title: &str) -> &str {
    title
        .char_indices()
        .nth(MAX_TITLE_CHARS)
        .map_or(title, |(end, _)| &title[..end])
}

/// Shows the dialog over `parent` (`_ptyxis_close_dialog_run_async()`).
/// `on_response` gets `true` once the user confirms, `false` if they
/// cancel or dismiss it. With no rows there is nothing to ask and it is
/// called right away.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    rows: &[Row],
    on_response: impl FnOnce(bool) + 'static,
) {
    if rows.is_empty() {
        on_response(true);
        return;
    }

    let dialog = adw::AlertDialog::new(Some(HEADING), Some(BODY));
    dialog.add_responses(&[
        (CANCEL_RESPONSE, CANCEL_LABEL),
        (DISCARD_RESPONSE, discard_label(rows.len())),
    ]);
    dialog.set_response_appearance(DISCARD_RESPONSE, adw::ResponseAppearance::Destructive);

    let group = adw::PreferencesGroup::new();
    for row in rows {
        let action_row = adw::ActionRow::builder()
            .title(truncate_title(&row.title))
            .subtitle(&row.subtitle)
            .build();
        group.add(&action_row);
    }
    dialog.set_extra_child(Some(&group));

    let on_response = Cell::new(Some(on_response));
    dialog.connect_response(None, move |dialog, response| {
        // Like ptyxis_close_dialog_confirm(), nothing more can be chosen.
        dialog.set_sensitive(false);
        if let Some(on_response) = on_response.take() {
            on_response(response == DISCARD_RESPONSE);
        }
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discard_labels() {
        for (n, expected) in [(1, CLOSE_LABEL), (2, CLOSE_ALL_LABEL), (5, CLOSE_ALL_LABEL)] {
            assert_eq!(discard_label(n), expected, "{n}");
        }
    }

    #[test]
    fn title_truncation() {
        let long = "x".repeat(MAX_TITLE_CHARS + 1);
        let multibyte = "é".repeat(MAX_TITLE_CHARS + 1);
        let cases = [
            ("", ""),
            ("vim file.txt ", "vim file.txt "),
            (&long, &long[..MAX_TITLE_CHARS]),
            (&multibyte, &multibyte[..MAX_TITLE_CHARS * 'é'.len_utf8()]),
        ];
        for (title, expected) in cases {
            assert_eq!(truncate_title(title), expected);
        }
    }
}
