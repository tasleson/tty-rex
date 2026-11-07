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
use gtk::{gdk, gio, glib};
use relm4::actions::{ActionName, RelmAction};
use relm4::prelude::*;
use vte::prelude::*;

use crate::cli::{self, CommandLineAction, CommandLineOptions, WindowTarget};
use crate::config;
use crate::container::{self, Container, Containers};
use crate::leader;
use crate::preferences::{Preferences, PreferencesInit, PreferencesInput};
use crate::profile::{Profile, Profiles};
use crate::session::{self, RestoreOptions, Session, SessionTab, SessionWindow};
use crate::settings::{self, AppSettings, SettingsError};
use crate::spawn::{self, HostSpawner, Spawner};
use crate::tab::{Tab, TabInit, TabInput};
use crate::telemetry::Pid;
use crate::terminal::{self, ZoomLevel};
use crate::window::{
    NewTabInContainerAction, NewTabWithProfileAction, Window, WindowInit, WindowInput,
};

/// Section titles of the "new tab" menu once it lists containers.
const PROFILES_SECTION_TITLE: &str = "Profiles";
const CONTAINERS_SECTION_TITLE: &str = "Containers";

/// Icons from `ptyxis/icons/`, compiled by `build.rs`.
static RESOURCES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ptyxis.gresource"));

fn register_resources() {
    match gio::Resource::from_data(&glib::Bytes::from_static(RESOURCES)) {
        Ok(resource) => gio::resources_register(&resource),
        Err(e) => glib::g_warning!("tty-rex", "Bundled icons are unavailable: {e}"),
    }
}

/// The C build's stylesheet, shared so widgets ported from it look the same.
const STYLESHEET: &str = include_str!("../ptyxis/style.css");

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
    /// Container to run in instead of the profile's default one.
    pub container: Option<Container>,
    /// Id of a container to look up once containers are discovered, used
    /// when `container` is unset (session restore).
    pub container_id: Option<String>,
    /// Title shown until the program sets one.
    pub initial_title: Option<String>,
}

struct TabHandle {
    controller: Controller<Tab>,
    terminal: vte::Terminal,
    profile_uuid: String,
    zoom: Rc<Cell<ZoomLevel>>,
    color: Rc<Cell<Option<gdk::RGBA>>>,
    child_pid: Rc<Cell<Option<Pid>>>,
    css_class: String,
    container: Rc<RefCell<Option<Container>>>,
    initial_title: Option<String>,
    /// The directory the tab was started in, saved in the session until
    /// the shell reports one.
    start_cwd_uri: Option<String>,
}

/// Cheap clones of a tab's shared state for use by windows.
pub struct TabRef {
    pub sender: relm4::Sender<TabInput>,
    pub terminal: vte::Terminal,
    pub profile_uuid: String,
    pub zoom: Rc<Cell<ZoomLevel>>,
    /// The color the user gave the tab, if any.
    pub color: Rc<Cell<Option<gdk::RGBA>>>,
    /// Added to the window while the tab is selected (see `Tab::css_class`).
    pub css_class: String,
    /// The tab's running child process, if any.
    pub child_pid: Rc<Cell<Option<Pid>>>,
    /// The container the tab was opened in, if not its profile's default.
    pub container: Option<Container>,
}

impl TabRef {
    /// The program in the foreground if it is not the tab's own child
    /// (`ptyxis_tab_has_foreground_process()`).
    pub fn foreground(&self) -> Option<leader::Foreground> {
        let pty = self.terminal.pty()?;
        leader::foreground_process(pty.fd(), self.child_pid.get())
    }

    /// Whether closing the tab would end a program other than a shell
    /// (`ptyxis_tab_is_running()`).
    pub fn is_running(&self) -> bool {
        self.foreground()
            .is_some_and(|foreground| foreground.is_running(spawn::is_shell))
    }
}

/// Handle to a launched window.
pub struct WindowRef {
    pub sender: relm4::Sender<WindowInput>,
    pub window: adw::ApplicationWindow,
    pub tab_view: adw::TabView,
}

/// A launched window with its tab view, which is kept here so the session
/// can be saved while a window component is updating (when its model
/// cannot be borrowed).
struct WindowEntry {
    controller: Controller<Window>,
    tab_view: adw::TabView,
}

impl WindowEntry {
    fn window_ref(&self) -> WindowRef {
        WindowRef {
            sender: self.controller.sender().clone(),
            window: self.controller.widget().clone(),
            tab_view: self.tab_view.clone(),
        }
    }

    fn is(&self, window: &gtk::Window) -> bool {
        self.controller.widget().upcast_ref::<gtk::Window>() == window
    }
}

pub struct App {
    application: adw::Application,
    settings: AppSettings,
    profiles: Profiles,
    spawner: Rc<dyn Spawner>,
    containers: Containers,
    /// Shared by every window's "new tab" split button.
    profile_menu: gio::Menu,
    /// Kept alive so their label-change handlers keep firing.
    menu_profiles: RefCell<Vec<Profile>>,
    windows: RefCell<Vec<WindowEntry>>,
    tabs: RefCell<HashMap<gtk::Widget, TabHandle>>,
    /// `--title` given without other options, applied when activation
    /// creates a window (`next_title_prefix` in the C code).
    next_title_prefix: RefCell<Option<String>>,
    /// The single preferences window, while open.
    preferences: RefCell<Option<Controller<Preferences>>>,
    /// The session saved by the previous run, until it is restored
    /// (`session` and `has_restored_session` in the C code).
    session: RefCell<Option<Session>>,
}

relm4::new_action_group!(AppActions, "app");
relm4::new_stateless_action!(PreferencesAction, AppActions, "preferences");
relm4::new_stateless_action!(AboutAction, AppActions, "about");
// Desktop actions of the D-Bus activatable desktop file; GNOME Shell also
// launches through `new-window` (`ptyxis_application_new_window_action()`).
relm4::new_stateless_action!(NewWindowAction, AppActions, "new-window");
relm4::new_stateless_action!(NewTabAction, AppActions, "new-tab");
relm4::new_stateful_action!(EditProfileAction, AppActions, "edit-profile", String, ());

/// tty-rex's icon, named after the default application id like the desktop
/// file's `Icon=`, so builds with another id (e.g. `.Devel`) find it too.
const ICON_NAME: &str = "io.github.tasleson.TtyRex";
/// tty-rex is ported from Ptyxis, so the About dialog credits its author
/// and keeps its copyright (`ptyxis_application_about()`).
const PTYXIS_DEVELOPER: &str = "Christian Hergert";
const COPYRIGHT: &str = "© 2023-2024 Christian Hergert, et al.";
const BASED_ON_TITLE: &str = "Based on";
const PTYXIS_CREDIT: &str = "Ptyxis by Christian Hergert https://gitlab.gnome.org/chergert/ptyxis";

/// Icon used while tty-rex's own icon is not installed.
const FALLBACK_ICON_NAME: &str = "org.gnome.Ptyxis";
/// How long a force-quit tab's program gets to handle `SIGHUP` before
/// `SIGKILL` (`ptyxis_tab_force_quit()`).
const FORCE_QUIT_KILL_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Sends `signal` to `pid`, logging failures (e.g. a process that is gone).
fn send_signal(pid: Pid, signal: libc::c_int) {
    // SAFETY: kill() has no memory-safety preconditions.
    if unsafe { libc::kill(pid, signal) } != 0 {
        let e = std::io::Error::last_os_error();
        glib::g_debug!(
            "tty-rex",
            "Cannot send signal {signal} to process {pid}: {e}"
        );
    }
}

/// Sends `SIGHUP` to the program in `child_pid`, then `SIGKILL` shortly
/// after unless it has exited (`ptyxis_tab_force_quit()`). `keep_alive` is
/// held until then; while the tab lives its exit clears `child_pid`, so an
/// already reaped pid is never signaled.
pub fn hang_up<T: 'static>(child_pid: &Rc<Cell<Option<Pid>>>, keep_alive: T) {
    let Some(pid) = child_pid.get() else {
        return;
    };
    send_signal(pid, libc::SIGHUP);
    let child_pid = Rc::clone(child_pid);
    glib::timeout_add_local_once(FORCE_QUIT_KILL_DELAY, move || {
        // A tab whose profile restarts its program may have a new one
        // by now; that one goes with the terminal, like after a close.
        if child_pid.get() == Some(pid) {
            send_signal(pid, libc::SIGKILL);
        }
        drop(keep_alive);
    });
}

/// The window icon: tty-rex's own when the icon theme has it, otherwise
/// Ptyxis's, which an installed Ptyxis provides.
fn app_icon_name(has_icon: impl Fn(&str) -> bool) -> &'static str {
    if has_icon(ICON_NAME) {
        ICON_NAME
    } else {
        FALLBACK_ICON_NAME
    }
}

impl App {
    fn new(application: &adw::Application) -> Result<Rc<Self>, SettingsError> {
        let settings = AppSettings::new()?;
        let app = Rc::new(Self {
            application: application.clone(),
            profiles: Profiles::new(settings.clone()),
            settings,
            spawner: Rc::new(HostSpawner),
            containers: Containers::new(),
            profile_menu: gio::Menu::new(),
            menu_profiles: RefCell::default(),
            windows: RefCell::default(),
            tabs: RefCell::default(),
            next_title_prefix: RefCell::default(),
            preferences: RefCell::default(),
            // Loaded at startup so it is at hand however tty-rex is
            // launched (`ptyxis_application_startup()`).
            session: RefCell::new(Session::load()),
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
        app.containers.connect_changed({
            let weak = Rc::downgrade(&app);
            move || rebuild_profile_menu(&weak)
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
                        app.windows.borrow_mut().retain(|e| !e.is(&window));
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
        let about = RelmAction::<AboutAction>::new_stateless(move |_| {
            if let Some(app) = weak.upgrade() {
                app.show_about();
            }
        });
        application.add_action(about.gio_action());
        let weak = Rc::downgrade(&app);
        let edit_profile =
            RelmAction::<EditProfileAction>::new_with_target_value(move |_, uuid| {
                if let Some(app) = weak.upgrade() {
                    app.edit_profile(&uuid);
                }
            });
        application.add_action(edit_profile.gio_action());
        let weak = Rc::downgrade(&app);
        let new_window = RelmAction::<NewWindowAction>::new_stateless(move |_| {
            if let Some(app) = weak.upgrade() {
                // GNOME Shell launches through this action rather than
                // activation, so restore here too when nothing is open
                // (`ptyxis_application_new_window_action()`).
                let has_windows = !app.windows.borrow().is_empty();
                if has_windows || !app.restore_session() {
                    app.open_window_with_tab(TabRequest::default())
                        .window
                        .present();
                }
            }
        });
        application.add_action(new_window.gio_action());
        let weak = Rc::downgrade(&app);
        let new_tab = RelmAction::<NewTabAction>::new_stateless(move |_| {
            if let Some(app) = weak.upgrade() {
                app.new_tab_in_current_window();
            }
        });
        application.add_action(new_tab.gio_action());

        Ok(app)
    }

    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    pub fn profile_menu(&self) -> &gio::Menu {
        &self.profile_menu
    }

    pub fn containers(&self) -> &Containers {
        &self.containers
    }

    /// Whether `uuid` (or the default profile) keeps the container of the
    /// tab a new one is opened from.
    pub fn preserves_container(&self, uuid: Option<&str>) -> bool {
        self.profiles
            .profile(uuid)
            .is_ok_and(|p| p.preserve_container() != settings::PreserveContainer::Never)
    }

    /// Launches a tab component and returns its root widget for a page.
    pub fn create_tab(&self, request: TabRequest) -> Result<gtk::Widget, SettingsError> {
        let profile = self.profiles.profile(request.profile_uuid.as_deref())?;
        let profile_uuid = profile.uuid().to_owned();
        let zoom = Rc::new(Cell::new(request.zoom));
        let color = Rc::new(Cell::new(None));
        let child_pid = Rc::new(Cell::new(None));
        let container = Rc::new(RefCell::new(request.container));
        let start_cwd_uri = request
            .initial_cwd_uri
            .clone()
            .or_else(|| request.previous_cwd_uri.clone());
        let controller = Tab::builder()
            .launch(TabInit {
                profile,
                app_settings: self.settings.clone(),
                spawner: Rc::clone(&self.spawner),
                containers: self.containers.clone(),
                container: Rc::clone(&container),
                restored_container_id: request.container_id,
                command: request.command,
                initial_cwd_uri: request.initial_cwd_uri,
                previous_cwd_uri: request.previous_cwd_uri,
                title_prefix: request.title_prefix,
                initial_title: request.initial_title.clone(),
                zoom: Rc::clone(&zoom),
                color: Rc::clone(&color),
                child_pid: Rc::clone(&child_pid),
                grid_size: request.grid_size,
            })
            .detach();
        let child: gtk::Widget = controller.widget().clone().upcast();
        let terminal = controller.model().terminal().clone();
        let css_class = controller.model().css_class().to_owned();
        self.tabs.borrow_mut().insert(
            child.clone(),
            TabHandle {
                controller,
                terminal,
                profile_uuid,
                zoom,
                color,
                child_pid,
                css_class,
                container,
                initial_title: request.initial_title,
                start_cwd_uri,
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
            color: Rc::clone(&handle.color),
            css_class: handle.css_class.clone(),
            child_pid: Rc::clone(&handle.child_pid),
            container: handle.container.borrow().clone(),
        })
    }

    /// Drops a tab's component; its terminal (and child) go away with it.
    pub fn remove_tab(&self, child: &gtk::Widget) {
        self.tabs.borrow_mut().remove(child);
    }

    /// Ends a closed tab once it can no longer be reopened
    /// (`ptyxis_tab_force_quit()`). Like Ptyxis it sends `SIGHUP` first, so
    /// shells such as bash save their history, and `SIGKILL` shortly after
    /// in case the program ignored the hangup. The tab is kept until then
    /// so its exit is noticed and an already reaped pid is never signaled.
    pub fn force_quit_tab(&self, child: &gtk::Widget) {
        let Some(handle) = self.tabs.borrow_mut().remove(child) else {
            return;
        };
        let child_pid = Rc::clone(&handle.child_pid);
        hang_up(&child_pid, handle);
    }

    pub fn new_window(self: &Rc<Self>) -> WindowRef {
        self.launch_window(false)
    }

    fn launch_window(self: &Rc<Self>, single_terminal: bool) -> WindowRef {
        let controller = Window::builder()
            .launch(WindowInit {
                app: Rc::clone(self),
                single_terminal,
            })
            .detach();
        let tab_view = controller.model().tab_view().clone();
        let entry = WindowEntry {
            controller,
            tab_view,
        };
        let handle = entry.window_ref();
        self.windows.borrow_mut().push(entry);
        handle
    }

    /// The focused window, or any window if none is focused.
    pub fn current_window(&self) -> Option<WindowRef> {
        let windows = self.windows.borrow();
        let active = self.application.active_window();
        let entry = windows
            .iter()
            .find(|e| active.as_ref().is_some_and(|w| e.is(w)))
            .or_else(|| windows.first())?;
        Some(entry.window_ref())
    }

    /// Whether `window` is the only terminal window (`is_last_window()`).
    /// Asks GTK rather than `windows`, which forgets closed windows only
    /// on idle.
    pub fn is_last_window(&self, window: &gtk::Window) -> bool {
        let entries = self.windows.borrow();
        self.application
            .windows()
            .iter()
            .filter(|w| entries.iter().any(|e| e.is(w)))
            .all(|w| w == window)
    }

    /// Recreates the windows of the previous session, once per run
    /// (`ptyxis_application_restore()`); false if none came back.
    fn restore_session(self: &Rc<Self>) -> bool {
        let Some(saved) = self.session.take() else {
            return false;
        };
        let plan = saved.restore_plan(RestoreOptions {
            restore_session: self.settings.restore_session(),
            restore_window_size: self.settings.restore_window_size(),
            default_size: self.settings.default_grid_size(),
        });
        let mut added_window = false;
        for planned in plan {
            let window = self.new_window();
            let mut active = None;
            for tab in planned.tabs {
                let Some(page) = self.append_tab(&window.tab_view, tab.request) else {
                    continue;
                };
                window.tab_view.set_page_pinned(&page, tab.pinned);
                if tab.active {
                    active = Some(page);
                }
            }
            if planned.add_default_tab
                && let Some(page) = self.append_tab(&window.tab_view, TabRequest::default())
            {
                active.get_or_insert(page);
            }
            if window.tab_view.n_pages() == 0 {
                window.window.destroy();
                continue;
            }
            if planned.maximized {
                window.window.maximize();
            }
            window.window.present();
            if let Some(page) = active {
                window.tab_view.set_selected_page(&page);
            }
            added_window = true;
        }
        added_window
    }

    /// Adds a tab at the end of `tab_view`, logging a failure.
    fn append_tab(&self, tab_view: &adw::TabView, request: TabRequest) -> Option<adw::TabPage> {
        match self.create_tab(request) {
            Ok(child) => Some(tab_view.append(&child)),
            Err(e) => {
                glib::g_warning!("tty-rex", "Cannot restore a tab: {e}");
                None
            }
        }
    }

    /// The windows and tabs to save, most recently focused window first
    /// like `gtk_application_get_windows()` (`ptyxis_session_save()`).
    fn session_state(&self) -> Session {
        let restore_session = self.settings.restore_session();
        let entries = self.windows.borrow();
        let windows = self
            .application
            .windows()
            .iter()
            .filter_map(|window| entries.iter().find(|e| e.is(window)))
            .map(|entry| {
                let tab_view = &entry.tab_view;
                let selected = tab_view.selected_page();
                let tabs = (0..tab_view.n_pages())
                    .map(|i| tab_view.nth_page(i))
                    .filter(|page| restore_session || page.is_pinned())
                    .filter_map(|page| self.session_tab(&page, selected.as_ref() == Some(&page)))
                    .collect();
                SessionWindow {
                    maximized: entry.controller.widget().is_maximized(),
                    tabs,
                }
            })
            .collect();
        Session { windows }
    }

    fn session_tab(&self, page: &adw::TabPage, active: bool) -> Option<SessionTab> {
        let tabs = self.tabs.borrow();
        let handle = tabs.get(&page.child())?;
        let terminal = &handle.terminal;
        let profile_default = self
            .profiles
            .profile(Some(&handle.profile_uuid))
            .map(|p| p.default_container())
            .unwrap_or_default();
        let container = handle.container.borrow();
        Some(SessionTab {
            profile: Some(handle.profile_uuid.clone()),
            pinned: page.is_pinned(),
            size: u32::try_from(terminal.column_count())
                .ok()
                .zip(u32::try_from(terminal.row_count()).ok()),
            zoom: handle.zoom.get(),
            active,
            // Before the program sets a title, keep the restored one.
            window_title: terminal::window_title(terminal)
                .filter(|t| !t.is_empty())
                .or_else(|| handle.initial_title.clone()),
            // A tab whose shell never reported a directory keeps the one
            // it was started in.
            cwd: terminal::current_directory_uri(terminal).or_else(|| handle.start_cwd_uri.clone()),
            container: session::container_to_save(
                container.as_ref().map(Container::id),
                &profile_default,
            ),
        })
    }

    /// Writes the session in the background, keeping the application
    /// alive until it is written (`ptyxis_application_save_session()`).
    pub fn save_session(&self) {
        let bytes = self.session_state().to_bytes();
        let path = session::file_path();
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            glib::g_warning!("tty-rex", "Failed to save session state: {e}");
            return;
        }
        let hold = self.application.hold();
        gio::File::for_path(&path).replace_contents_async(
            bytes,
            None,
            false,
            gio::FileCreateFlags::REPLACE_DESTINATION,
            None::<&gio::Cancellable>,
            move |result| {
                drop(hold);
                if let Err((_, e)) = result {
                    glib::g_warning!("tty-rex", "Failed to save session state: {e}");
                }
            },
        );
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
                    containers: self.containers.clone(),
                })
                .detach();
            // Like the C window it gets its own window group so it stacks
            // independently of terminal windows, and keeps the application
            // running on its own (e.g. `tty-rex --preferences`).
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

    /// Shows the About dialog over the active window.
    pub fn show_about(&self) {
        // The icon chosen at startup: tty-rex's own, or the Ptyxis fallback.
        let icon = gtk::Window::default_icon_name()
            .map(|name| name.to_string())
            .unwrap_or_else(|| FALLBACK_ICON_NAME.to_owned());
        let dialog = adw::AboutDialog::builder()
            .application_name(config::APP_NAME)
            .application_icon(icon)
            .version(config::PACKAGE_VERSION)
            .developers([PTYXIS_DEVELOPER])
            .copyright(COPYRIGHT)
            .license_type(gtk::License::Gpl30)
            .website(config::WEBSITE)
            .issue_url(config::ISSUE_URL)
            .build();
        dialog.add_acknowledgement_section(Some(BASED_ON_TITLE), &[PTYXIS_CREDIT]);
        dialog.present(self.application.active_window().as_ref());
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

    /// Opens a tab with the default profile in the current window, or in a
    /// new one (`ptyxis_application_new_tab_action()`).
    fn new_tab_in_current_window(self: &Rc<Self>) {
        let request = TabRequest::default();
        match self.current_window() {
            Some(window) => {
                window.sender.emit(WindowInput::AddTab(request));
                window.window.present();
            }
            None => self.open_window_with_tab(request).window.present(),
        }
    }

    fn activate(self: &Rc<Self>) {
        match self.current_window() {
            Some(window) => window.window.present(),
            None if self.restore_session() => {}
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

        // Restore first so the session is not lost when tty-rex closes
        // again, whatever the options; a standalone instance only restores
        // when activated (`ptyxis_application_command_line()`).
        let standalone = self
            .application
            .flags()
            .contains(gio::ApplicationFlags::NON_UNIQUE);
        let did_restore = !standalone && self.restore_session();

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
                // A bare --new-window (e.g. a keybinding that makes sure a
                // window exists) is satisfied by the restored windows; with
                // a directory it opens a tab in one of them.
                let plain_new_window = options.new_window
                    && request.command.is_none()
                    && request.profile_uuid.is_none();
                let target = match target {
                    WindowTarget::New if did_restore && plain_new_window => {
                        if options.working_directory.is_none()
                            && let Some(window) = self.current_window()
                        {
                            window.window.present();
                            return glib::ExitCode::SUCCESS;
                        }
                        WindowTarget::Current
                    }
                    target => target,
                };
                let window = match target {
                    WindowTarget::Current => self.current_window(),
                    WindowTarget::New => None,
                };
                // Like ptyxis_window_new_for_command(), a standalone -x
                // window (e.g. a Terminal=true desktop file) never saves
                // the session, which would replace the user's windows.
                let single_terminal = standalone
                    && request.command.is_some()
                    && !options.tab
                    && !options.new_window
                    && request.profile_uuid.is_none();
                let window = window.unwrap_or_else(|| self.launch_window(single_terminal));
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
    let containers = menu_containers(app.containers.list());
    if containers.is_empty() {
        app.profile_menu.append_section(None, &section);
    } else {
        // Like the C new terminal menu: the host, profiles, containers.
        let host = gio::Menu::new();
        host.append_item(
            &RelmAction::<NewTabInContainerAction>::to_menu_item_with_target_value(
                container::SESSION_TITLE,
                &container::SESSION_ID.to_owned(),
            ),
        );
        app.profile_menu.append_section(None, &host);
        app.profile_menu
            .append_section(Some(PROFILES_SECTION_TITLE), &section);
        let section = gio::Menu::new();
        for container in containers {
            section.append_item(
                &RelmAction::<NewTabInContainerAction>::to_menu_item_with_target_value(
                    container.display_name(),
                    &container.id().to_owned(),
                ),
            );
        }
        app.profile_menu
            .append_section(Some(CONTAINERS_SECTION_TITLE), &section);
    }
    *app.menu_profiles.borrow_mut() = profiles;
}

/// The named containers other than the host, by name, as the C window's
/// `maybe_containers` model lists them.
fn menu_containers(containers: Vec<Container>) -> Vec<Container> {
    let mut containers: Vec<Container> = containers
        .into_iter()
        .filter(|c| !c.is_session() && !c.display_name().is_empty())
        .collect();
    containers.sort_by(|a, b| a.display_name().cmp(b.display_name()));
    containers
}

/// Runs the application with the process arguments and returns its exit code.
pub fn run() -> glib::ExitCode {
    glib::set_prgname(Some(config::APP_NAME));
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
            let icon_theme = gtk::gdk::Display::default().map(|d| gtk::IconTheme::for_display(&d));
            gtk::Window::set_default_icon_name(app_icon_name(|name| {
                icon_theme
                    .as_ref()
                    .is_some_and(|theme| theme.has_icon(name))
            }));
            relm4::set_global_css(STYLESHEET);
            match App::new(application) {
                Ok(app) => *state.borrow_mut() = Some(app),
                Err(e) => {
                    eprintln!("{}: {e}", config::APP_NAME);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_falls_back_to_ptyxis() {
        let cases: [(&[&str], &str); 3] = [
            (&[ICON_NAME, FALLBACK_ICON_NAME], ICON_NAME),
            (&[FALLBACK_ICON_NAME], FALLBACK_ICON_NAME),
            // Without either, still ask for Ptyxis's: GTK then shows its
            // generic icon, as for any missing name.
            (&[], FALLBACK_ICON_NAME),
        ];
        for (installed, expected) in cases {
            assert_eq!(
                app_icon_name(|name| installed.contains(&name)),
                expected,
                "{installed:?}"
            );
        }
    }

    #[test]
    fn menu_lists_named_containers_by_name() {
        let discovered = container::parse_podman_ps(
            r#"[{"Id": "z", "Names": ["zeta"]}, {"Id": "n"},
                {"Id": "a", "Names": ["alpha"]}]"#,
        )
        .unwrap();
        let all = std::iter::once(Container::session())
            .chain(discovered)
            .collect();
        let ids: Vec<String> = menu_containers(all)
            .iter()
            .map(|c| c.id().to_owned())
            .collect();
        assert_eq!(ids, ["a", "z"]);
    }
}
