// application.rs
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

//! tty-rex's `AdwApplication` subclass, which exists for
//! `ptyxis_application_dbus_register()`: the `org.freedesktop.Terminal1`
//! interface has to be exported while GApplication registers on the bus,
//! before it owns its name, or a call that D-Bus activated tty-rex could
//! arrive before the object exists. Everything else about the application
//! lives in [`crate::app`].

use std::cell::{OnceCell, RefCell};

use adw::subclass::prelude::*;
use gtk::{gio, glib};

use crate::terminal_intent::{self, LaunchCommand};

type LaunchHandler = dyn Fn(LaunchCommand) -> Result<(), glib::Error>;

const NOT_READY_ERROR: &str = "tty-rex failed to start";

/// The error for calls made while tty-rex cannot handle them.
pub fn not_ready_error() -> glib::Error {
    glib::Error::new(gio::DBusError::Failed, NOT_READY_ERROR)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Application {
        pub(super) launch_handler: OnceCell<Box<LaunchHandler>>,
        /// The exported `org.freedesktop.Terminal1` object
        /// (`terminal_intent`).
        pub(super) terminal_intent: RefCell<Option<(gio::DBusConnection, gio::RegistrationId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Application {
        const NAME: &'static str = "TtyRexApplication";
        type Type = super::Application;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for Application {}

    impl ApplicationImpl for Application {
        fn dbus_register(
            &self,
            connection: &gio::DBusConnection,
            object_path: &str,
        ) -> Result<(), glib::Error> {
            self.parent_dbus_register(connection, object_path)?;
            let application = self.obj().downgrade();
            let id = terminal_intent::export(connection, object_path, move |call| {
                let app = application.upgrade().ok_or_else(not_ready_error)?;
                let handler = app.imp().launch_handler.get().ok_or_else(not_ready_error)?;
                handler(call)
            })?;
            *self.terminal_intent.borrow_mut() = Some((connection.clone(), id));
            Ok(())
        }

        fn dbus_unregister(&self, connection: &gio::DBusConnection, object_path: &str) {
            let exported = self.terminal_intent.take();
            if let Some((exported_on, id)) = exported {
                if &exported_on == connection {
                    if let Err(e) = connection.unregister_object(id) {
                        glib::g_debug!("tty-rex", "Cannot unexport the terminal intent: {e}");
                    }
                } else {
                    *self.terminal_intent.borrow_mut() = Some((exported_on, id));
                }
            }
            self.parent_dbus_unregister(connection, object_path);
        }
    }

    impl GtkApplicationImpl for Application {}
    impl AdwApplicationImpl for Application {}
}

glib::wrapper! {
    pub struct Application(ObjectSubclass<imp::Application>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Application {
    pub fn new(
        application_id: &str,
        flags: gio::ApplicationFlags,
        resource_base_path: &str,
    ) -> Self {
        glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .property("resource-base-path", resource_base_path)
            .build()
    }

    /// Sets what handles `org.freedesktop.Terminal1` calls; until then
    /// they fail. Only the first call takes effect.
    pub fn set_launch_handler(
        &self,
        handler: impl Fn(LaunchCommand) -> Result<(), glib::Error> + 'static,
    ) {
        let _ = self.imp().launch_handler.set(Box::new(handler));
    }
}
