// color_dialog.rs
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

//! Running `gtk::ColorDialog` for the color settings and tab colors, which
//! are tty-rex additions without a C equivalent.

use gtk::{gdk, gio, glib};

/// An opaque-color dialog titled `title`.
pub fn new(title: &str) -> gtk::ColorDialog {
    gtk::ColorDialog::builder()
        .title(title)
        .with_alpha(false)
        .build()
}

/// Shows `dialog` starting from `initial` and calls `on_closed` once it
/// has gone away, with the picked color or `None` when the dialog was
/// dismissed or cancelled (through `cancellable`). Other failures are
/// logged with `what` naming the color being chosen.
pub fn choose(
    dialog: &gtk::ColorDialog,
    parent: Option<&gtk::Window>,
    initial: Option<&gdk::RGBA>,
    cancellable: Option<&gio::Cancellable>,
    what: &str,
    on_closed: impl FnOnce(Option<gdk::RGBA>) + 'static,
) {
    let what = what.to_owned();
    dialog.choose_rgba(parent, initial, cancellable, move |result| {
        on_closed(
            result
                .inspect_err(|e| {
                    if !e.matches(gtk::DialogError::Dismissed)
                        && !e.matches(gtk::DialogError::Cancelled)
                    {
                        glib::g_warning!("tty-rex", "Cannot choose {what}: {e}");
                    }
                })
                .ok(),
        )
    });
}
