// signal_guard.rs
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

//! Signal connections that disconnect themselves.
//!
//! Handlers on objects that outlive their owner (shared `GSettings`, the
//! style manager, a terminal that moves between windows) must be
//! disconnected when the owner goes away. [`SignalGuard`] does that on
//! drop, so owners need no bookkeeping beyond holding the guard.

use std::cell::Cell;

use gtk::glib::{self, prelude::*};
use gtk::prelude::*;

/// A connected signal handler, disconnected when dropped. It holds its
/// object weakly: if the object is gone, so is the handler.
#[derive(Debug)]
pub struct SignalGuard {
    object: glib::WeakRef<glib::Object>,
    id: Option<glib::SignalHandlerId>,
}

impl SignalGuard {
    /// Guards `id`, which must have been connected on `object`.
    pub fn new(object: &impl IsA<glib::Object>, id: glib::SignalHandlerId) -> Self {
        Self {
            object: object.upcast_ref::<glib::Object>().downgrade(),
            id: Some(id),
        }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        if let (Some(id), Some(object)) = (self.id.take(), self.object.upgrade()) {
            object.disconnect(id);
        }
    }
}

/// Keeps `value` (typically guards) alive until `widget` is destroyed.
pub fn drop_on_destroy<T: 'static>(widget: &impl IsA<gtk::Widget>, value: T) {
    let value = Cell::new(Some(value));
    widget.connect_destroy(move |_| drop(value.take()));
}
