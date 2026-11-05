// app.rs
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

//! Application lifecycle and the registries of windows and tabs
//! (`src/ptyxis-application.c`).
//!
//! The `adw::Application` is driven directly instead of through
//! `RelmApp::run()`, which assumes a single root window created at startup.
//! A terminal needs any number of windows created on demand (activation,
//! remote command lines, tab drag-out), so window and tab components are
//! launched here and their controllers kept until the widgets go away.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use relm4::actions::{ActionName, RelmAction};
use relm4::prelude::*;

use crate::cli::{self, CommandLineAction, CommandLineOptions, WindowTarget};
use crate::config;
use crate::preferences::{Preferences, PreferencesInit, PreferencesInput};
use crate::profile::{Profile, Profiles};
use crate::settings::{self, AppSettings, SettingsError};
use crate::spawn::{HostSpawner, Spawner};
use crate::tab::{Tab, TabInit, TabInput};
use crate::terminal::ZoomLevel;
use crate::window::{NewTabWithProfileAction, Window, WindowInit, WindowInput};

/// Icons from `src/icons/`, compiled by `build.rs`.
static RESOURCES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ptyxis.gresource"));

fn register_resources() {
    match gio::Resource::from_data(&glib::Bytes::from_static(RESOURCES)) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => glib::g_warning!("Ptyxis", "Bundled icons are unavailable: {e}"),
    }
}

/// The C build's stylesheet, shared so widgets ported from it look the same.
const STYLESHEET: &str = include_str!("../../../src/style.css");

/// Parameters for a new tab; unset fields use profile/settings defaults.
#[derive(Debug, Clone, Default)]
pub struct TabRequest {
    /// Profile to use; `None` or unknown ids use the default profile.
    pub profile_uuid: Option<String>,
    pub command: Option<Vec<String>>,
    pub initial_cwd_uri: Option<String>,
    pub previous_cwd_uri: Option<String>,
    pub title_prefix: Option<String>,
    pub zoom: ZoomLevel,
    pub grid_size: Option<(u32, u32)>,
}

struct TabHandle {
    controller: Controller<Tab>,
    terminal: vte::Terminal,
    profile_uuid: String,
    zoom: Rc<Cell<ZoomLevel>>,
}

/// Cheap clones of a tab's shared state for use by windows.
pub struct TabRef {
    pub sender: relm4::Sender<TabInput>,
    pub terminal: vte::Terminal,
    pub profile_uuid: String,
    pub zoom: Rc<Cell<ZoomLevel>>,
}

/// Handle to a launched window.
pub struct WindowRef {
    pub sender: relm4::Sender<WindowInput>,
    pub window: adw::ApplicationWindow,
    pub tab_view: adw::TabView,
}

impl WindowRef {
    /// Must not be called while the window component is updating, since it
    /// borrows the component's model.
    fn new(controller: &Controller<Window>) -> Self {
        Self {
            sender: controller.sender().clone(),
            window: controller.widget().clone(),
            tab_view: controller.model().tab_view().clone(),
        }
    }
}

pub struct App {
    application: adw::Application,
    settings: AppSettings,
    profiles: Profiles,
    spawner: Rc<dyn Spawner>,
    /// Shared by every window's "new tab" split button.
    profile_menu: gio::Menu,
    /// Kept alive so their label-change handlers keep firing.
    menu_profiles: RefCell<Vec<Profile>>,
    windows: RefCell<Vec<Controller<Window>>>,
    tabs: RefCell<HashMap<gtk::Widget, TabHandle>>,
    /// `--title` given without other options, applied when activation
    /// creates a window (`next_title_prefix` in the C code).
    next_title_prefix: RefCell<Option<String>>,
    /// The single preferences window, while open.
    preferences: RefCell<Option<Controller<Preferences>>>,
}

relm4::new_action_group!(AppActions, "app");
relm4::new_stateless_action!(PreferencesAction, AppActions, "preferences");
relm4::new_stateful_action!(EditProfileAction, AppActions, "edit-profile", String, ());

impl App {
    fn new(application: &adw::Application) -> Result<Rc<Self>, SettingsError> {
        let settings = AppSettings::new()?;
        let app = Rc::new(Self {
            application: application.clone(),
            profiles: Profiles::new(settings.clone()),
            settings,
            spawner: Rc::new(HostSpawner),
            profile_menu: gio::Menu::new(),
            menu_profiles: RefCell::default(),
            windows: RefCell::default(),
            tabs: RefCell::default(),
            next_title_prefix: RefCell::default(),
            preferences: RefCell::default(),
        });

        let style_manager = adw::StyleManager::default();
        style_manager.set_color_scheme(app.settings.interface_style().into());
        let gsettings = app.settings.gsettings();
        gsettings.connect_changed(Some(settings::key::INTERFACE_STYLE), {
            let weak = Rc::downgrade(&app);
            move |_, _| {
                if let Some(app) = weak.upgrade() {
                    adw::StyleManager::default()
                        .set_color_scheme(app.settings.interface_style().into());
                }
            }
        });
        gsettings.connect_changed(Some(settings::key::PROFILE_UUIDS), {
            let weak = Rc::downgrade(&app);
            move |_, _| rebuild_profile_menu(&weak)
        });
        rebuild_profile_menu(&Rc::downgrade(&app));

        // The controllers hold references to their windows, so "destroy"
        // (emitted on dispose) would never fire; GTK removes a window from
        // the application as soon as it is destroyed, though.
        application.connect_window_removed({
            let weak = Rc::downgrade(&app);
            move |_, window| {
                let weak = weak.clone();
                let window = window.clone();
                // Defer so a controller is never dropped from inside one of
                // its own handlers.
                glib::idle_add_local_once(move || {
                    if let Some(app) = weak.upgrade() {
                        app.windows
                            .borrow_mut()
                            .retain(|c| c.widget().upcast_ref::<gtk::Window>() != &window);
                        let mut preferences = app.preferences.borrow_mut();
                        if preferences
                            .as_ref()
                            .is_some_and(|c| c.widget().upcast_ref::<gtk::Window>() == &window)
                        {
                            *preferences = None;
                        }
                    }
                });
            }
        });

        let weak = Rc::downgrade(&app);
        let preferences = RelmAction::<PreferencesAction>::new_stateless(move |_| {
            if let Some(app) = weak.upgrade() {
                app.show_preferences(None);
            }
        });
        application.add_action(preferences.gio_action());
        let weak = Rc::downgrade(&app);
        let edit_profile =
            RelmAction::<EditProfileAction>::new_with_target_value(move |_, uuid| {
                if let Some(app) = weak.upgrade() {
                    app.edit_profile(&uuid);
                }
            });
        application.add_action(edit_profile.gio_action());

        Ok(app)
    }

    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    pub fn profile_menu(&self) -> &gio::Menu {
        &self.profile_menu
    }

    /// Launches a tab component and returns its root widget for a page.
    pub fn create_tab(&self, request: TabRequest) -> Result<gtk::Widget, SettingsError> {
        let profile = self.profiles.profile(request.profile_uuid.as_deref())?;
        let profile_uuid = profile.uuid().to_owned();
        let zoom = Rc::new(Cell::new(request.zoom));
        let controller = Tab::builder()
            .launch(TabInit {
                profile,
                app_settings: self.settings.clone(),
                spawner: Rc::clone(&self.spawner),
                command: request.command,
                initial_cwd_uri: request.initial_cwd_uri,
                previous_cwd_uri: request.previous_cwd_uri,
                title_prefix: request.title_prefix,
                zoom: Rc::clone(&zoom),
                grid_size: request.grid_size,
            })
            .detach();
        let child: gtk::Widget = controller.widget().clone().upcast();
        let terminal = controller.model().terminal().clone();
        self.tabs.borrow_mut().insert(
            child.clone(),
            TabHandle {
                controller,
                terminal,
                profile_uuid,
                zoom,
            },
        );
        Ok(child)
    }

    pub fn tab(&self, child: &gtk::Widget) -> Option<TabRef> {
        self.tabs.borrow().get(child).map(|handle| TabRef {
            sender: handle.controller.sender().clone(),
            terminal: handle.terminal.clone(),
            profile_uuid: handle.profile_uuid.clone(),
            zoom: Rc::clone(&handle.zoom),
        })
    }

    /// Drops a tab's component; its terminal (and child) go away with it.
    pub fn remove_tab(&self, child: &gtk::Widget) {
        self.tabs.borrow_mut().remove(child);
    }

    pub fn new_window(self: &Rc<Self>) -> WindowRef {
        let controller = Window::builder()
            .launch(WindowInit {
                app: Rc::clone(self),
            })
            .detach();
        let handle = WindowRef::new(&controller);
        self.windows.borrow_mut().push(controller);
        handle
    }

    /// The focused window, or any window if none is focused.
    pub fn current_window(&self) -> Option<WindowRef> {
        let windows = self.windows.borrow();
        let active = self.application.active_window();
        let controller = windows
            .iter()
            .find(|c| active.as_ref() == Some(c.widget().upcast_ref()))
            .or_else(|| windows.first())?;
        Some(WindowRef::new(controller))
    }

    /// Presents the preferences window, creating it if needed
    /// (`ptyxis_preferences_window_get_default()`).
    pub fn show_preferences(&self, transient_for: Option<&gtk::Window>) -> gtk::Window {
        let mut preferences = self.preferences.borrow_mut();
        let controller = preferences.get_or_insert_with(|| {
            let controller = Preferences::builder()
                .launch(PreferencesInit {
                    settings: self.settings.clone(),
                    profiles: self.profiles.clone(),
                })
                .detach();
            // Like the C window it gets its own window group so it stacks
            // independently of terminal windows, and keeps the application
            // running on its own (e.g. `ptyxis --preferences`).
            gtk::WindowGroup::new().add_window(controller.widget());
            self.application.add_window(controller.widget());
            controller
        });
        let window: gtk::Window = controller.widget().clone().upcast();
        if window.transient_for().is_none() {
            window.set_transient_for(transient_for);
        }
        window.present();
        window
    }

    /// Shows the Shortcuts page of the preferences window.
    pub fn show_shortcuts(&self, transient_for: Option<&gtk::Window>) {
        self.show_preferences(transient_for);
        if let Some(preferences) = self.preferences.borrow().as_ref() {
            preferences.emit(PreferencesInput::ShowShortcuts);
        }
    }

    /// Opens the editor for a profile in the preferences window.
    pub fn edit_profile(&self, uuid: &str) {
        self.show_preferences(None);
        if let Some(preferences) = self.preferences.borrow().as_ref() {
            preferences.emit(PreferencesInput::EditProfile(uuid.to_owned()));
        }
    }

    /// Opens a new window with a single tab.
    pub fn open_window_with_tab(self: &Rc<Self>, request: TabRequest) -> WindowRef {
        let window = self.new_window();
        window.sender.emit(WindowInput::AddTab(request));
        window
    }

    fn activate(self: &Rc<Self>) {
        match self.current_window() {
            Some(window) => window.window.present(),
            None => {
                let request = TabRequest {
                    title_prefix: self.next_title_prefix.take(),
                    ..Default::default()
                };
                self.open_window_with_tab(request).window.present();
            }
        }
    }

    fn command_line(self: &Rc<Self>, cmdline: &gio::ApplicationCommandLine) -> glib::ExitCode {
        let options = CommandLineOptions::from_dict(&cmdline.options_dict());
        let caller_cwd = cmdline.cwd();
        let action = match cli::plan(&options, caller_cwd.as_deref(), &glib::home_dir()) {
            Ok(action) => action,
            Err(e) => {
                cmdline.printerr_literal(&format!("{e}\n"));
                return glib::ExitCode::FAILURE;
            }
        };

        match action {
            CommandLineAction::Preferences => {
                // GApplication action names carry no "app." prefix.
                self.application
                    .activate_action(<PreferencesAction as ActionName>::NAME, None);
            }
            CommandLineAction::Activate { title } => {
                *self.next_title_prefix.borrow_mut() = title;
                self.application.activate();
            }
            CommandLineAction::OpenTab {
                target,
                profile_uuid,
                command,
                cwd_uri,
                title,
            } => {
                let request = TabRequest {
                    profile_uuid,
                    command,
                    initial_cwd_uri: cwd_uri,
                    title_prefix: title,
                    ..Default::default()
                };
                let window = match target {
                    WindowTarget::Current => self.current_window(),
                    WindowTarget::New => None,
                };
                let window = window.unwrap_or_else(|| self.new_window());
                window.sender.emit(WindowInput::AddTab(request));
                window.window.present();
            }
        }
        glib::ExitCode::SUCCESS
    }
}

fn rebuild_profile_menu(app: &Weak<App>) {
    let Some(app) = app.upgrade() else {
        return;
    };
    let profiles = app.profiles.list();
    let section = gio::Menu::new();
    for profile in &profiles {
        let item = RelmAction::<NewTabWithProfileAction>::to_menu_item_with_target_value(
            &profile.label(),
            &profile.uuid().to_owned(),
        );
        section.append_item(&item);

        let weak = Rc::downgrade(&app);
        profile
            .gsettings()
            .connect_changed(Some(crate::profile::key::LABEL), move |_, _| {
                rebuild_profile_menu(&weak)
            });
    }
    app.profile_menu.remove_all();
    app.profile_menu.append_section(None, &section);
    *app.menu_profiles.borrow_mut() = profiles;
}

/// Runs the application with the process arguments and returns its exit code.
pub fn run() -> glib::ExitCode {
    glib::set_prgname(Some("ptyxis"));
    glib::set_application_name(config::APP_NAME);

    let early = cli::preprocess_args(std::env::args().collect());
    // Like src/main.c, importing happens synchronously before any instance
    // is started or contacted.
    if let Some(source) = &early.import_palette {
        return match crate::palette::import(source, &crate::palette::import_dir()) {
            Ok(_) => glib::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error: {e}");
                glib::ExitCode::FAILURE
            }
        };
    }
    let mut flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    if early.standalone {
        flags |= gio::ApplicationFlags::NON_UNIQUE | gio::ApplicationFlags::CAN_OVERRIDE_APP_ID;
    }

    register_resources();
    let application = adw::Application::builder()
        .application_id(config::APP_ID)
        .flags(flags)
        .resource_base_path(config::RESOURCE_BASE_PATH)
        .build();
    cli::register_options(&application);
    // Registers the application with relm4 (for `relm4::main_application()`);
    // we never call `RelmApp::run()`, see the module docs.
    let _ = relm4::RelmApp::<()>::from_app(application.clone());

    let state: Rc<RefCell<Option<Rc<App>>>> = Rc::default();
    let failed = Rc::new(Cell::new(false));

    application.connect_startup({
        let state = Rc::clone(&state);
        let failed = Rc::clone(&failed);
        move |application| {
            gtk::Window::set_default_icon_name(config::APP_ID);
            relm4::set_global_css(STYLESHEET);
            match App::new(application) {
                Ok(app) => *state.borrow_mut() = Some(app),
                Err(e) => {
                    eprintln!("ptyxis: {e}");
                    failed.set(true);
                    application.quit();
                }
            }
        }
    });
    application.connect_activate({
        let state = Rc::clone(&state);
        move |_| {
            if let Some(app) = state.borrow().as_ref() {
                app.activate();
            }
        }
    });
    application.connect_command_line({
        let state = Rc::clone(&state);
        move |_, cmdline| {
            let app = state.borrow().clone();
            match app {
                Some(app) => app.command_line(cmdline),
                None => glib::ExitCode::FAILURE,
            }
        }
    });

    let code = application.run_with_args(&early.args);
    if failed.get() {
        glib::ExitCode::FAILURE
    } else {
        code
    }
}
