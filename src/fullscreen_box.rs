// fullscreen_box.rs
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

//! The header bars that reveal themselves in fullscreen, a port of
//! `PtyxisFullscreenBox`. Instead of a widget subclass wrapping its own
//! `AdwToolbarView`, [`FullscreenBox`] drives the window's toolbar view.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

/// How long the bars stay once the pointer leaves them.
const FULLSCREEN_HIDE_DELAY: Duration = Duration::from_millis(300);
/// How long [`FullscreenBox::reveal`] keeps the bars regardless of the pointer.
const FORCED_REVEAL_DELAY: Duration = Duration::from_millis(1500);
/// Distance from the top edge that reveals the bars when they have no height.
const SHOW_HEADERBAR_DISTANCE_PX: f64 = 5.0;

struct Inner {
    toolbar_view: glib::WeakRef<adw::ToolbarView>,
    fullscreen: Cell<bool>,
    autohide: Cell<bool>,
    timeout: RefCell<Option<glib::SourceId>>,
    /// Set while a [`FullscreenBox::reveal`] holds the bars open.
    forced_reveal: RefCell<Option<glib::SourceId>>,
    last_focus: RefCell<Option<gtk::Widget>>,
    last_y: Cell<f64>,
    is_touch: Cell<bool>,
    headers: Vec<glib::WeakRef<gtk::Widget>>,
}

/// Reveals a toolbar view's top bars on demand while the window is fullscreen.
#[derive(Clone)]
pub struct FullscreenBox(Rc<Inner>);

impl FullscreenBox {
    /// Takes over `toolbar_view`, whose top bars `headers` are the ones that
    /// keep it revealed while they hold the focus, and follows the focus of
    /// `window`.
    pub fn new(
        window: &impl IsA<gtk::Window>,
        toolbar_view: &adw::ToolbarView,
        headers: &[gtk::Widget],
    ) -> Self {
        toolbar_view.set_top_bar_style(adw::ToolbarStyle::Raised);
        toolbar_view.set_bottom_bar_style(adw::ToolbarStyle::Raised);
        let this = Self(Rc::new(Inner {
            toolbar_view: toolbar_view.downgrade(),
            fullscreen: Cell::new(false),
            autohide: Cell::new(true),
            timeout: RefCell::new(None),
            forced_reveal: RefCell::new(None),
            last_focus: RefCell::new(window.focus()),
            last_y: Cell::new(0.0),
            is_touch: Cell::new(false),
            headers: headers.iter().map(|header| header.downgrade()).collect(),
        }));

        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&this.0);
        motion.connect_enter(move |_, x, y| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).motion(x, y);
            }
        });
        let weak = Rc::downgrade(&this.0);
        motion.connect_motion(move |_, x, y| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).motion(x, y);
            }
        });
        toolbar_view.add_controller(motion);

        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.set_touch_only(true);
        let weak = Rc::downgrade(&this.0);
        click.connect_pressed(move |gesture, _, _, y| {
            gesture.set_state(gtk::EventSequenceState::Denied);
            if let Some(inner) = weak.upgrade() {
                let this = Self(inner);
                this.0.is_touch.set(true);
                if y > this.titlebar_area_height() {
                    this.update(true);
                }
            }
        });
        toolbar_view.add_controller(click);

        let weak = Rc::downgrade(&this.0);
        window.connect_notify_local(Some("focus-widget"), move |window, _| {
            if let Some(inner) = weak.upgrade() {
                let this = Self(inner);
                *this.0.last_focus.borrow_mut() = window.focus();
                this.update(true);
            }
        });
        this
    }

    /// Whether the window is fullscreen; hides the bars when it is.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        if fullscreen == self.0.fullscreen.get() {
            return;
        }
        self.0.fullscreen.set(fullscreen);
        if !self.0.autohide.get() {
            return;
        }
        if let Some(view) = self.0.toolbar_view.upgrade() {
            view.set_extend_content_to_top_edge(fullscreen);
        }
        if fullscreen {
            self.update(false);
        } else {
            self.show_ui();
        }
    }

    /// Shows the bars for a moment, as when entering fullscreen or
    /// switching tabs, whatever the pointer does meanwhile unless it goes
    /// to the top edge (`ptyxis_fullscreen_box_reveal()`).
    pub fn reveal(&self) {
        self.clear_forced_reveal();
        self.show_ui();
        // Without this, a pointer that does not move would keep the bars
        // shown after a tab switch.
        self.0.last_y.set(f64::MAX);
        self.clear_timeout();
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(FORCED_REVEAL_DELAY, move || {
            if let Some(inner) = weak.upgrade() {
                let this = Self(inner);
                // The source is gone once it fires.
                this.0.forced_reveal.take();
                this.update(true);
            }
        });
        *self.0.forced_reveal.borrow_mut() = Some(id);
    }

    /// Ends a [`Self::reveal`] early, as typing does
    /// (`ptyxis_fullscreen_box_unreveal()`).
    pub fn unreveal(&self) {
        if self.clear_forced_reveal() {
            self.update(true);
        }
    }

    /// Whether a forced reveal was pending.
    fn clear_forced_reveal(&self) -> bool {
        let id = self.0.forced_reveal.take();
        let pending = id.is_some();
        if let Some(id) = id {
            id.remove();
        }
        pending
    }

    fn show_ui(&self) {
        self.clear_timeout();
        if let Some(view) = self.0.toolbar_view.upgrade() {
            view.set_reveal_top_bars(true);
            view.set_reveal_bottom_bars(true);
        }
    }

    fn hide_ui(&self) {
        self.clear_timeout();
        if !self.0.fullscreen.get() {
            return;
        }
        if let Some(view) = self.0.toolbar_view.upgrade() {
            view.set_reveal_top_bars(false);
            view.set_reveal_bottom_bars(false);
            view.grab_focus();
        }
    }

    fn clear_timeout(&self) {
        if let Some(id) = self.0.timeout.take() {
            id.remove();
        }
    }

    fn start_hide_timeout(&self) {
        let Some(view) = self.0.toolbar_view.upgrade() else {
            return;
        };
        if !view.reveals_top_bars() || self.0.timeout.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(FULLSCREEN_HIDE_DELAY, move || {
            if let Some(inner) = weak.upgrade() {
                let this = Self(inner);
                // The source is gone once it fires.
                this.0.timeout.take();
                this.hide_ui();
            }
        });
        *self.0.timeout.borrow_mut() = Some(id);
    }

    fn titlebar_area_height(&self) -> f64 {
        let height = self
            .0
            .toolbar_view
            .upgrade()
            .map_or(0.0, |view| f64::from(view.top_bar_height()));
        height.max(SHOW_HEADERBAR_DISTANCE_PX)
    }

    fn is_focusing_titlebar(&self) -> bool {
        let focus = self.0.last_focus.borrow();
        focus.as_ref().is_some_and(|focus| {
            self.0
                .headers
                .iter()
                .filter_map(|header| header.upgrade())
                .any(|header| *focus == header || focus.is_ancestor(&header))
        })
    }

    fn update(&self, hide_immediately: bool) {
        if !self.0.autohide.get() || !self.0.fullscreen.get() {
            return;
        }
        if self.0.forced_reveal.borrow().is_some() {
            if self.0.last_y.get() > self.titlebar_area_height() {
                return;
            }
            self.clear_forced_reveal();
        }
        if !self.0.is_touch.get() && self.0.last_y.get() <= self.titlebar_area_height() {
            self.show_ui();
            return;
        }
        if self.is_focusing_titlebar() {
            self.show_ui();
        } else if hide_immediately {
            self.hide_ui();
        } else {
            self.start_hide_timeout();
        }
    }

    fn motion(&self, _x: f64, y: f64) {
        self.0.is_touch.set(false);
        self.0.last_y.set(y);
        self.update(true);
    }
}
