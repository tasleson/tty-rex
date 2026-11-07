// window.rs
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

//! Terminal windows hosting an `adw::TabView` (`src/ptyxis-window.c`).
//!
//! Tabs are separate relm4 components owned by [`App`] rather than a
//! `FactoryVecDeque` here: `adw::TabView` owns page order (reordering,
//! pinning) and moves pages between windows by drag-and-drop, which a
//! factory owned by one window cannot follow. The window only inserts
//! pages and looks tabs up by their root widget.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use relm4::actions::{ActionName, EmptyType, RelmAction};
use relm4::prelude::*;
use vte::prelude::*;

use crate::app::{App, TabRef, TabRequest};
use crate::close_dialog;
use crate::config;
use crate::container::Container;
use crate::find_bar::{FindBar, FindBarInput, FindBarOutput};
use crate::fullscreen_box::FullscreenBox;
use crate::parking_lot::{self, ParkingLot};
use crate::settings::{NewTabPosition, TabMiddleClick};
use crate::shortcuts::Shortcuts;
use crate::signal_guard::{SignalGuard, drop_on_destroy};
use crate::tab_notify;
use crate::telemetry_bar::{TelemetryBar, TelemetryBarInput};
use crate::terminal::{self, ZoomLevel};
use crate::title_dialog::SET_TITLE_LABEL;

const DEFAULT_ICON_NAME: &str = "utilities-terminal-symbolic";
const DEVEL_STYLE_CLASS: &str = "devel";
/// Marks the window's main toolbar view, scoping window-dressing CSS so
/// it skips dialogs (`window-contents` in `src/ptyxis-window.ui`).
pub const WINDOW_CONTENTS_CLASS: &str = "window-contents";
/// Wraps the header bar so the visual bell can flash it; `style.css`
/// adjusts its padding while the tab bar shows.
const VISUAL_BELL_AREA_CLASS: &str = "visual-bell-area";
/// Added to the bell area for one flash (see `dressing`).
pub const VISUAL_BELL_CLASS: &str = "visual-bell";
/// Outlasts the flash animation so the next bell restarts it (the C
/// build's 500 ms, synced with the 0.3 s animation).
const VISUAL_BELL_DURATION: std::time::Duration = std::time::Duration::from_millis(500);
const HAS_TAB_BAR_CLASS: &str = "has-tab-bar";
/// libadwaita's private widget for one tab of an `AdwTabBar`.
const ADW_TAB_TYPE_NAME: &str = "AdwTab";
/// How long libadwaita animates closing the tab overview; focus moves to
/// the selected tab again afterwards (`ptyxis_window_tab_overview_notify_open_cb()`).
const OVERVIEW_CLOSE_DELAY: std::time::Duration = std::time::Duration::from_millis(425);
const OVERVIEW_CLOSE_DELAY_NO_ANIMATIONS: std::time::Duration =
    std::time::Duration::from_millis(10);
const MIN_WIDTH: i32 = 360;
const MIN_HEIGHT: i32 = 294;

// Actions live in the window's own `win` action map, matching the C action
// names and making them reachable over D-Bus (org.gtk.Actions on the
// window's object path) like any GtkApplicationWindow action.
relm4::new_action_group!(pub WindowActions, "win");
relm4::new_stateless_action!(pub NewTabAction, WindowActions, "new-tab");
relm4::new_stateful_action!(
    pub NewTabWithProfileAction,
    WindowActions,
    "new-tab-with-profile",
    String,
    ()
);
// Ptyxis's `win.new-terminal` takes a (profile, container) pair; tty-rex
// splits it like `new-tab-with-profile`, taking a container id.
relm4::new_stateful_action!(
    pub NewTabInContainerAction,
    WindowActions,
    "new-tab-in-container",
    String,
    ()
);
relm4::new_stateless_action!(pub NewWindowAction, WindowActions, "new-window");
relm4::new_stateless_action!(pub CloseTabAction, WindowActions, "close-tab");
relm4::new_stateless_action!(pub CloseOtherTabsAction, WindowActions, "close-other-tabs");
relm4::new_stateless_action!(pub UndoCloseTabAction, WindowActions, "undo-close-tab");
relm4::new_stateless_action!(pub MoveTabLeftAction, WindowActions, "move-tab-left");
relm4::new_stateless_action!(pub MoveTabRightAction, WindowActions, "move-tab-right");
relm4::new_stateless_action!(pub DetachTabAction, WindowActions, "detach-tab");
relm4::new_stateless_action!(pub PinTabAction, WindowActions, "pin-tab");
relm4::new_stateless_action!(pub UnpinTabAction, WindowActions, "unpin-tab");
relm4::new_stateless_action!(pub NextTabAction, WindowActions, "next-tab");
relm4::new_stateless_action!(pub PreviousTabAction, WindowActions, "previous-tab");
relm4::new_stateless_action!(pub CopyAction, WindowActions, "copy-clipboard");
relm4::new_stateless_action!(pub PasteAction, WindowActions, "paste-clipboard");
relm4::new_stateless_action!(pub ZoomInAction, WindowActions, "zoom-in");
relm4::new_stateless_action!(pub ZoomOutAction, WindowActions, "zoom-out");
relm4::new_stateless_action!(pub ZoomOneAction, WindowActions, "zoom-one");
relm4::new_stateless_action!(pub SearchAction, WindowActions, "search");
relm4::new_stateless_action!(pub SetTitleAction, WindowActions, "set-title");
relm4::new_stateless_action!(pub SetProfileAction, WindowActions, "set-profile");
relm4::new_stateless_action!(pub FullscreenAction, WindowActions, "fullscreen");
relm4::new_stateless_action!(pub UnfullscreenAction, WindowActions, "unfullscreen");
relm4::new_stateless_action!(pub ToggleFullscreenAction, WindowActions, "toggle-fullscreen");
// Tab colors and the telemetry footer are tty-rex additions, so these
// have no C counterpart. The telemetry action's state is whether the
// footer shows; activating it toggles that.
relm4::new_stateful_action!(
    pub ToggleTelemetryAction,
    WindowActions,
    "toggle-telemetry",
    (),
    bool
);
/// `win.tab.read-only`, a property action on the selected tab's terminal
/// that `on_selected_page_changed()` replaces with each tab switch.
const READ_ONLY_ACTION_NAME: &str = "tab.read-only";
pub const READ_ONLY_ACTION: &str = "win.tab.read-only";
/// Label of the telemetry toggle in menus.
pub const TOGGLE_TELEMETRY_LABEL: &str = "Show _Telemetry";
relm4::new_stateless_action!(pub SetTabColorAction, WindowActions, "set-tab-color");
relm4::new_stateless_action!(pub ResetTabColorAction, WindowActions, "reset-tab-color");
relm4::new_stateless_action!(pub TabOverviewAction, WindowActions, "tab-overview");
relm4::new_stateless_action!(pub PreferencesAction, WindowActions, "preferences");
// Shortcut-only actions of `ptyxis-window.ui` and `ptyxis-terminal.ui`;
// `win.focus-tab(n)`, `win.reset(b)` and `win.select-all(b)` take a target
// and are added by `add_shortcut_actions()`.
relm4::new_stateless_action!(pub PrimaryMenuAction, WindowActions, "primary-menu");
relm4::new_stateless_action!(pub TabMenuAction, WindowActions, "tab-menu");
relm4::new_stateless_action!(
    pub ShowKeyboardShortcutsAction,
    WindowActions,
    "show-keyboard-shortcuts"
);

pub struct WindowInit {
    pub app: Rc<App>,
    pub kind: WindowKind,
}

/// What a window was opened for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowKind {
    /// An ordinary window holding any number of tabs.
    #[default]
    Tabs,
    /// A window for the one command of a plain `-x` command line
    /// (`ptyxis_window_new_for_command()`): the actions that create, move or
    /// switch tabs are disabled and the tab bar, new tab button and overview
    /// button hidden.
    Command,
    /// A command window of a standalone instance (`single_terminal_mode`),
    /// which also stays out of the saved session, e.g. one opened for a
    /// `Terminal=true` desktop file, and never shows its tab bar.
    SingleTerminal,
}

impl WindowKind {
    /// The kind of window `ptyxis_application_command_line()` opens: only
    /// its plain `-x` branch, without `--tab`, `--new-window` or
    /// `--tab-with-profile`, makes a command window, which is in
    /// `single_terminal_mode` when the instance is standalone.
    pub fn for_command_line(plain_execute: bool, standalone: bool) -> Self {
        match (plain_execute, standalone) {
            (false, _) => Self::Tabs,
            (true, false) => Self::Command,
            (true, true) => Self::SingleTerminal,
        }
    }

    /// Whether the tab actions are enabled and their buttons shown.
    fn has_tab_actions(self) -> bool {
        self == Self::Tabs
    }

    /// Whether closing the window may save the session.
    fn saves_session(self) -> bool {
        self != Self::SingleTerminal
    }

    /// Whether the tab bar shows with `n_pages` tabs
    /// (`update_visible_and_maybe_close()`).
    fn shows_tab_bar(self, n_pages: i32) -> bool {
        self != Self::SingleTerminal && n_pages > 1
    }
}

#[derive(Debug)]
pub enum WindowInput {
    /// Adds a fully specified tab (used by the command line).
    AddTab(Box<TabRequest>),
    /// Adds tabs in order, selecting the first (`org.freedesktop.Terminal1`).
    AddTabs(Vec<TabRequest>),
    /// Adds a tab inheriting directory, zoom and size from the active tab;
    /// `None` keeps the active tab's profile.
    NewTab {
        profile_uuid: Option<String>,
        /// Runs the tab in this container rather than the profile's.
        container_id: Option<String>,
    },
    NewWindow,
    CloseTab,
    CloseOtherTabs,
    UndoCloseTab,
    /// The closed tab parked under this serial can no longer be reopened.
    ParkedTabExpired(parking_lot::Serial),
    MoveTabLeft,
    MoveTabRight,
    DetachTab,
    SetTabPinned(bool),
    NextTab,
    PreviousTab,
    Copy,
    Paste,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ShowFind,
    FindDismissed,
    /// The telemetry footer was toggled to this visibility.
    SetTelemetryVisible(bool),
    SetTitle,
    SetProfile,
    SetTabColor,
    ResetTabColor,
    ShowPreferences,
    ShowKeyboardShortcuts,
    SelectedPageChanged,
    /// The selected tab's terminal rang the bell.
    Bell,
    /// The flash started with this serial is over.
    VisualBellDone(u32),
    ToggleOverview,
    OverviewClosed,
    /// The overview's close animation started with this serial is over.
    OverviewCloseFinished(u32),
    PageDetached {
        child: gtk::Widget,
        transferring: bool,
    },
    TransferFinished,
}

pub struct Window {
    app: Rc<App>,
    window: adw::ApplicationWindow,
    tab_view: adw::TabView,
    window_title: adw::WindowTitle,
    title_bindings: glib::BindingGroup,
    find_bar: Controller<FindBar>,
    find_bar_revealer: gtk::Revealer,
    telemetry_bar: Controller<TelemetryBar>,
    telemetry_revealer: gtk::Revealer,
    /// Kept alive so the controller follows settings changes.
    _shortcuts: Option<Shortcuts>,
    /// Disabled without a selection so the shortcut reaches the terminal.
    copy_action: gio::SimpleAction,
    /// Enabled only while a tab is selected and the clipboard offers text
    /// (`ptyxis_terminal_update_clipboard_actions()`).
    paste_action: gio::SimpleAction,
    /// `win.zoom-in`, `win.zoom-out` and `win.zoom-one`, enabled as the
    /// selected tab's zoom allows, and the primary menu's zoom label.
    zoom_controls: ZoomControls,
    /// Follows the selected page's `pinned` property so the tab menu only
    /// shows the applicable one.
    _pinned_signals: glib::SignalGroup,
    /// Handlers on the selected tab's terminal, dropped on tab switch.
    terminal_handlers: Vec<SignalGuard>,
    /// CSS class of the selected tab, which lets that tab style the window
    /// (e.g. highlight it while running sudo or ssh).
    tab_css_class: Option<String>,
    input: relm4::Sender<WindowInput>,
    visual_bell: gtk::Box,
    /// Identifies the latest flash, so an earlier flash ending does not cut
    /// a later one short.
    visual_bell_serial: u32,
    tab_overview: adw::TabOverview,
    /// Identifies the latest overview close, so reopening the overview
    /// before a close finishes cancels its delayed refocus.
    overview_close_serial: u32,
    /// Set while the window is closing as a whole, when its tabs going away
    /// must not save the session again (`in_close_request`). It is cleared
    /// if the user cancels the close.
    in_close_request: Rc<Cell<bool>>,
    kind: WindowKind,
    /// Closed tabs that `win.undo-close-tab` can still reopen; shared with
    /// the close-request handler, which releases them with the window.
    parked: Rc<RefCell<ParkingLot<gtk::Widget>>>,
    /// Briefly shows the header bars in fullscreen on a tab switch.
    fullscreen_box: FullscreenBox,
}

/// The zoom actions, whose sensitivity follows the selected tab's zoom,
/// and the primary menu's reset button showing that zoom.
#[derive(Clone)]
struct ZoomControls {
    zoom_in: gio::SimpleAction,
    zoom_out: gio::SimpleAction,
    zoom_one: gio::SimpleAction,
    label: gtk::Button,
}

impl ZoomControls {
    /// Enables each action that applies at `zoom`, or none without a tab
    /// (`ptyxis_window_notify_zoom_cb()` and
    /// `ptyxis_window_page_changed_cb()`). The label follows the tab's
    /// `zoom-label` and, as with Ptyxis's binding group, keeps its last
    /// text when there is no tab.
    fn update(&self, zoom: Option<ZoomLevel>) {
        if let Some(zoom) = zoom {
            self.label.set_label(&zoom.label());
        }
        self.zoom_in
            .set_enabled(zoom.is_some_and(ZoomLevel::can_zoom_in));
        self.zoom_out
            .set_enabled(zoom.is_some_and(ZoomLevel::can_zoom_out));
        self.zoom_one
            .set_enabled(zoom.is_some_and(ZoomLevel::can_reset));
    }
}

/// Registers a parameterless action that forwards `message` to the window
/// and returns it.
fn add_action<A>(
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<Window>,
    message: fn() -> WindowInput,
) -> gio::SimpleAction
where
    A: ActionName<Group = WindowActions>,
    A::Target: EmptyType,
    A::State: EmptyType,
{
    let input = sender.input_sender().clone();
    let action = RelmAction::<A>::new_stateless(move |_| input.emit(message()));
    window.add_action(action.gio_action());
    action.gio_action().clone()
}

impl Window {
    pub fn tab_view(&self) -> &adw::TabView {
        &self.tab_view
    }

    fn selected_child(&self) -> Option<gtk::Widget> {
        self.tab_view.selected_page().map(|page| page.child())
    }

    /// Moves the selected tab one step and keeps focus in it, like
    /// `ptyxis_window_move_left_action()`.
    fn move_selected_tab(&self, reorder: fn(&adw::TabView, &adw::TabPage) -> bool) {
        if let Some(page) = self.tab_view.selected_page() {
            reorder(&self.tab_view, &page);
            self.send_to_active_tab(crate::tab::TabInput::Focus);
        }
    }

    fn send_to_active_tab(&self, message: crate::tab::TabInput) {
        if let Some(tab) = self.selected_child().and_then(|c| self.app.tab(&c)) {
            tab.sender.emit(message);
        }
    }

    fn inherited_request(&self, profile_uuid: Option<String>) -> TabRequest {
        inherited_request(&self.app, &self.tab_view, profile_uuid)
    }

    fn add_tab(&mut self, request: TabRequest) {
        if let Err(e) = insert_tab(&self.app, &self.tab_view, request) {
            glib::g_critical!("tty-rex", "Failed to create tab: {e}");
            if self.tab_view.n_pages() == 0 {
                self.window.destroy();
            }
        }
    }

    /// Adds a tab per request, selecting the first one; like
    /// `ptyxis_terminal_intent_handle_launch_command()`, the others are
    /// placed relative to it by `new-tab-position` without being selected.
    fn add_tabs(&mut self, requests: Vec<TabRequest>) {
        for (i, request) in requests.into_iter().enumerate() {
            match insert_page(&self.app, &self.tab_view, request) {
                Ok(page) if i == 0 => self.tab_view.set_selected_page(&page),
                Ok(_) => {}
                Err(e) => glib::g_critical!("tty-rex", "Failed to create tab: {e}"),
            }
        }
        if self.tab_view.n_pages() == 0 {
            self.window.destroy();
        }
    }

    /// Moves focus back to the selected tab once the overview has closed,
    /// working around <https://gitlab.gnome.org/GNOME/libadwaita/-/issues/670>
    /// like `ptyxis_window_tab_overview_notify_open_cb()`.
    fn overview_closed(&mut self) {
        self.send_to_active_tab(crate::tab::TabInput::Focus);
        let animations = gtk::Settings::default()
            .map(|settings| settings.is_gtk_enable_animations())
            .unwrap_or(true);
        let delay = if animations {
            OVERVIEW_CLOSE_DELAY
        } else {
            OVERVIEW_CLOSE_DELAY_NO_ANIMATIONS
        };
        self.overview_close_serial = self.overview_close_serial.wrapping_add(1);
        let serial = self.overview_close_serial;
        let input = self.input.clone();
        glib::timeout_add_local_once(delay, move || {
            input.emit(WindowInput::OverviewCloseFinished(serial))
        });
    }

    /// Flashes the header bar (`ptyxis_window_visual_bell()`).
    fn visual_bell(&mut self) {
        if !self.app.settings().visual_bell() {
            return;
        }
        self.visual_bell.add_css_class(VISUAL_BELL_CLASS);
        self.visual_bell_serial = self.visual_bell_serial.wrapping_add(1);
        let serial = self.visual_bell_serial;
        let input = self.input.clone();
        glib::timeout_add_local_once(VISUAL_BELL_DURATION, move || {
            input.emit(WindowInput::VisualBellDone(serial))
        });
    }

    /// Closes the window once its last tab is gone, saving the session
    /// first unless the window is being closed as a whole, which saved it
    /// already (`update_visible_and_maybe_close()`).
    fn close_if_empty(&self) {
        if self.tab_view.n_pages() == 0 && !self.tab_view.is_transferring_page() {
            if !self.in_close_request.get() && self.kind.saves_session() {
                self.app.save_session();
            }
            release_parked(&self.app, &self.parked);
            self.window.destroy();
        }
    }

    /// Keeps a closed tab alive for a few seconds so it can be reopened
    /// (`ptyxis_window_close_page_cb()`). Tabs of a window that is closing
    /// as a whole were released already. Tabs closed while the window asks
    /// whether to close the rest are parked, so they come back if the user
    /// cancels. Like Ptyxis, a command window parks its tabs too even though
    /// it has no `win.undo-close-tab`, so they live out the same timeout.
    fn park_tab(&self, child: gtk::Widget) {
        if self.app.tab(&child).is_none() {
            self.app.remove_tab(&child);
            return;
        }
        let serial = self.parked.borrow_mut().push(child);
        let input = self.input.clone();
        glib::timeout_add_local_once(parking_lot::DEFAULT_TIMEOUT, move || {
            // The window may be gone, taking its parked tabs with it.
            let _ = input.send(WindowInput::ParkedTabExpired(serial));
        });
    }

    /// Reopens the most recently closed tab where new tabs go
    /// (`ptyxis_window_undo_close_tab_action()`).
    fn undo_close_tab(&self) {
        let Some(child) = self.parked.borrow_mut().pop() else {
            return;
        };
        let Some(tab) = self.app.tab(&child) else {
            return;
        };
        let page = self
            .tab_view
            .insert(&child, new_tab_position(&self.app, &self.tab_view));
        self.tab_view.set_selected_page(&page);
        tab.sender.emit(crate::tab::TabInput::Restored);
    }

    fn on_selected_page_changed(&mut self) {
        let page = self.tab_view.selected_page();
        self.title_bindings.set_source(page.as_ref());

        self.terminal_handlers.clear();
        self.copy_action.set_enabled(false);
        update_paste_action(&self.paste_action, &self.tab_view);
        // Shows which tab is now selected when in fullscreen.
        self.fullscreen_box.reveal();
        let tab = page.as_ref().and_then(|page| self.app.tab(&page.child()));
        // Like the GPropertyAction on the tab's `read-only` property, which
        // is the terminal's `input-enabled` inverted.
        self.window.remove_action(READ_ONLY_ACTION_NAME);
        if let Some(tab) = &tab {
            self.window.add_action(&read_only_action(&tab.terminal));
        }
        if let Some(class) = self.tab_css_class.take() {
            self.window.remove_css_class(&class);
        }
        if let Some(tab) = &tab {
            self.window.add_css_class(&tab.css_class);
            self.tab_css_class = Some(tab.css_class.clone());
        }
        self.find_bar.emit(FindBarInput::SetTerminal(
            tab.as_ref().map(|t| t.terminal.clone()),
        ));
        // Unlike the find bar, the footer stays and follows the selection.
        self.telemetry_bar.emit(TelemetryBarInput::SetTab(
            tab.as_ref().map(|t| Rc::clone(&t.child_pid)),
        ));
        let Some(page) = page else {
            self.zoom_controls.update(None);
            self.find_bar_revealer.set_reveal_child(false);
            self.window_title.set_title(config::APP_NAME);
            self.window_title.set_subtitle("");
            return;
        };
        page.set_needs_attention(false);

        let Some(tab) = tab else {
            return;
        };
        let window_title = self.window_title.clone();
        let update_subtitle = move |terminal: &vte::Terminal| {
            let home = glib::home_dir();
            let subtitle = terminal::current_directory_uri(terminal)
                .map(|uri| terminal::collapse_uri(&uri, &home.to_string_lossy()))
                .unwrap_or_default();
            window_title.set_subtitle(&subtitle);
        };
        update_subtitle(&tab.terminal);
        let terminal = &tab.terminal;
        let cwd_id = terminal::connect_current_directory_uri_changed(terminal, update_subtitle);

        let copy_action = self.copy_action.clone();
        copy_action.set_enabled(tab.terminal.has_selection());
        let selection_id = terminal.connect_selection_changed(move |terminal| {
            copy_action.set_enabled(terminal.has_selection())
        });
        // Like ptyxis_window_active_tab_bell_cb(), only the selected tab
        // flashes the window.
        let input = self.input.clone();
        let bell_id = terminal.connect_bell(move |_| input.emit(WindowInput::Bell));
        // Typing hides the bars a tab switch revealed
        // (`ptyxis_window_active_tab_commit_cb()`).
        let fullscreen_box = self.fullscreen_box.clone();
        let commit_id = terminal::connect_commit_bytes(terminal, move || fullscreen_box.unreveal());
        // The tab sets its zoom before the font scale, so the notify sees
        // the new level (the tab's `notify::zoom` in Ptyxis).
        let zoom_controls = self.zoom_controls.clone();
        let zoom = Rc::clone(&tab.zoom);
        zoom_controls.update(Some(zoom.get()));
        let zoom_id =
            terminal.connect_font_scale_notify(move |_| zoom_controls.update(Some(zoom.get())));
        self.terminal_handlers = [cwd_id, selection_id, bell_id, commit_id, zoom_id]
            .into_iter()
            .map(|id| SignalGuard::new(terminal, id))
            .collect();
        tab.sender.emit(crate::tab::TabInput::Focus);
    }
}

/// `win.tab.read-only` for `terminal`, toggling whether it takes input.
fn read_only_action(terminal: &vte::Terminal) -> gio::PropertyAction {
    glib::Object::builder()
        .property("name", READ_ONLY_ACTION_NAME)
        .property("object", terminal)
        .property("property-name", "input-enabled")
        .property("invert-boolean", true)
        .build()
}

/// A request for a new tab in `tab_view` carrying over state from its
/// selected tab, as `ptyxis_window_apply_current_settings()` does; `None`
/// keeps the selected tab's profile.
fn inherited_request(
    app: &App,
    tab_view: &adw::TabView,
    profile_uuid: Option<String>,
) -> TabRequest {
    let mut request = TabRequest {
        profile_uuid,
        ..Default::default()
    };
    if let Some(active) = tab_view.selected_page().and_then(|p| app.tab(&p.child())) {
        request.profile_uuid = request.profile_uuid.or(Some(active.profile_uuid.clone()));
        request.previous_cwd_uri = terminal::current_directory_uri(&active.terminal);
        request.zoom = active.zoom.get();
        request.grid_size = terminal::grid_size(&active.terminal);
        if app.preserves_container(request.profile_uuid.as_deref()) {
            request.container = current_container(app, &active);
        }
    }
    request
}

/// The container `tab` runs in: the one its shell reports if tty-rex
/// knows it, else the one it was opened in (`ptyxis_tab_dup_container()`).
fn current_container(app: &App, tab: &TabRef) -> Option<Container> {
    terminal::container_runtime(&tab.terminal)
        .zip(terminal::container_name(&tab.terminal))
        .and_then(|(runtime, name)| app.containers().find_by_name(&runtime, &name))
        .or_else(|| tab.container.clone())
}

/// Creates a tab for `request` and selects it in `tab_view`. Free of the
/// window model so the tab overview's synchronous `create-tab` can use it.
fn insert_tab(
    app: &App,
    tab_view: &adw::TabView,
    request: TabRequest,
) -> Result<adw::TabPage, crate::settings::SettingsError> {
    let page = insert_page(app, tab_view, request)?;
    tab_view.set_selected_page(&page);
    Ok(page)
}

/// Creates a tab for `request` in `tab_view` without selecting it
/// (`ptyxis_window_add_tab()`).
fn insert_page(
    app: &App,
    tab_view: &adw::TabView,
    mut request: TabRequest,
) -> Result<adw::TabPage, crate::settings::SettingsError> {
    if request.grid_size.is_none() && tab_view.n_pages() == 0 {
        request.grid_size = Some(app.settings().initial_grid_size());
    }
    let child = app.create_tab(request)?;
    Ok(tab_view.insert(&child, new_tab_position(app, tab_view)))
}

/// Where a new or reopened tab goes, after the selected one or last
/// depending on `new-tab-position` (`ptyxis_window_add_tab()`).
fn new_tab_position(app: &App, tab_view: &adw::TabView) -> i32 {
    match (app.settings().new_tab_position(), tab_view.selected_page()) {
        (NewTabPosition::Next, Some(page)) => tab_view.page_position(&page) + 1,
        _ => tab_view.n_pages(),
    }
}

/// Adds `win.interface-style` and the theme selector to the primary menu
/// (`ptyxis_window_add_theme_controls()`). The action state is the
/// `AdwColorScheme` nick the C property action uses.
fn add_theme_controls(
    window: &adw::ApplicationWindow,
    menu_button: &gtk::MenuButton,
    settings: &crate::settings::AppSettings,
) {
    use crate::theme_selector::{color_scheme_nick, style_for_color_scheme_nick, theme_selector};

    let state = color_scheme_nick(settings.interface_style()).to_variant();
    let action = gio::SimpleAction::new_stateful(
        INTERFACE_STYLE_ITEM,
        Some(glib::VariantTy::STRING),
        &state,
    );
    let gsettings = settings.gsettings().clone();
    action.connect_activate(move |_, target| {
        let Some(style) = target
            .and_then(|t| t.str())
            .and_then(style_for_color_scheme_nick)
        else {
            return;
        };
        if let Err(e) = gsettings.set_string(crate::settings::key::INTERFACE_STYLE, style.nick()) {
            glib::g_warning!("tty-rex", "Failed to save interface style: {e}");
        }
    });
    let weak_action = action.downgrade();
    let settings_for_state = settings.clone();
    let id = settings.gsettings().connect_changed(
        Some(crate::settings::key::INTERFACE_STYLE),
        move |_, _| {
            if let Some(action) = weak_action.upgrade() {
                action.set_state(
                    &color_scheme_nick(settings_for_state.interface_style()).to_variant(),
                );
            }
        },
    );
    drop_on_destroy(window, SignalGuard::new(settings.gsettings(), id));
    window.add_action(&action);

    let action_name = format!("win.{INTERFACE_STYLE_ITEM}");
    if let Some(popover) = menu_button.popover().and_downcast::<gtk::PopoverMenu>() {
        popover.add_child(&theme_selector(&action_name), INTERFACE_STYLE_ITEM);
    }
}

/// A flat, circular zoom button with a 16 px icon, as in
/// `ptyxis_window_add_zoom_controls()`.
fn zoom_icon_button(action_name: &str, icon_name: &str, tooltip: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .action_name(action_name)
        .tooltip_text(tooltip)
        .child(
            &gtk::Image::builder()
                .icon_name(icon_name)
                .pixel_size(16)
                .build(),
        )
        .css_classes(["circular", "flat"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// Puts zoom out, the zoom percentage (resetting the zoom) and zoom in
/// into the primary menu and returns the percentage button
/// (`ptyxis_window_add_zoom_controls()`).
fn add_zoom_controls(menu_button: &gtk::MenuButton) -> gtk::Button {
    let zoom_out = zoom_icon_button(
        &ZoomOutAction::action_name(),
        "zoom-out-symbolic",
        "Zoom Out",
        "Zoom out",
    );
    let zoom_label = gtk::Button::builder()
        .css_classes(["flat", "pill"])
        .action_name(ZoomOneAction::action_name())
        .hexpand(true)
        .tooltip_text("Reset Zoom")
        .label(ZoomLevel::DEFAULT.label())
        .build();
    let zoom_in = zoom_icon_button(
        &ZoomInAction::action_name(),
        "zoom-in-symbolic",
        "Zoom In",
        "Zoom in",
    );
    let zoom_box = gtk::Box::builder()
        .spacing(12)
        .margin_start(18)
        .margin_end(18)
        .build();
    zoom_box.append(&zoom_out);
    zoom_box.append(&zoom_label);
    zoom_box.append(&zoom_in);
    if let Some(popover) = menu_button.popover().and_downcast::<gtk::PopoverMenu>() {
        popover.add_child(&zoom_box, ZOOM_ITEM);
    }
    zoom_label
}

/// The subtitle of a tab: the file or directory the shell reported
/// (`ptyxis_tab_dup_subtitle()`).
fn tab_subtitle(terminal: &vte::Terminal) -> String {
    let home = glib::home_dir();
    terminal::current_file_uri(terminal)
        .filter(|uri| !uri.is_empty())
        .or_else(|| terminal::current_directory_uri(terminal).filter(|uri| !uri.is_empty()))
        .map(|uri| terminal::collapse_uri(&uri, &home.to_string_lossy()))
        .unwrap_or_default()
}

/// How the close dialog lists a tab: the program in its foreground, else
/// the tab's own title (`_ptyxis_close_dialog_new()`).
fn close_dialog_row(tab: &TabRef, page: &adw::TabPage) -> close_dialog::Row {
    match tab.foreground() {
        Some(foreground) => close_dialog::Row {
            title: foreground.cmdline.unwrap_or_default(),
            subtitle: format!("Process {}", foreground.pid),
        },
        None => close_dialog::Row {
            title: page.title().into(),
            subtitle: tab_subtitle(&tab.terminal),
        },
    }
}

/// Asks whether to close `pages`, raising the tab when there is only one
/// like `_ptyxis_close_dialog_new()`.
fn confirm_close(
    tab_view: &adw::TabView,
    pages: &[(adw::TabPage, close_dialog::Row)],
    on_response: impl FnOnce(bool) + 'static,
) {
    if let [(page, _)] = pages {
        tab_view.set_selected_page(page);
    }
    let rows: Vec<_> = pages.iter().map(|(_, row)| row.clone()).collect();
    close_dialog::present(tab_view, &rows, on_response);
}

/// Stores the selected tab's grid size in `window-size`, whether or not
/// `restore-window-size` is on, like `ptyxis_window_save_size()`.
fn save_size(app: &App, tab_view: &adw::TabView) {
    let Some(size) = tab_view
        .selected_page()
        .and_then(|page| app.tab(&page.child()))
        .and_then(|tab| terminal::grid_size(&tab.terminal))
    else {
        return;
    };
    if let Err(e) = app.settings().set_window_size(size) {
        glib::g_warning!("tty-rex", "Failed to save the window size: {e}");
    }
}

/// The tab under a point of the tab bar
/// (`ptyxis_window_get_tab_at_tab_bar_point()`). libadwaita keeps its tab
/// widget type private, so it is found by name.
fn tab_at_tab_bar_point(
    app: &App,
    tab_bar: &adw::TabBar,
    x: f64,
    y: f64,
) -> Option<(adw::TabPage, TabRef)> {
    let tab_type = glib::Type::from_name(ADW_TAB_TYPE_NAME)?;
    let pick = tab_bar.pick(x, y, gtk::PickFlags::DEFAULT)?;
    let tab = if pick.type_().is_a(tab_type) {
        pick
    } else {
        pick.ancestor(tab_type)?
    };
    let page = tab.property::<Option<adw::TabPage>>("page")?;
    let tab = app.tab(&page.child())?;
    Some((page, tab))
}

/// Handles a middle click on the tab bar, capturing it before the tab
/// sees it unless the tab should close as libadwaita does by default
/// (`ptyxis_window_tab_bar_click_pressed_cb()`).
fn tab_bar_middle_click_pressed(
    app: &App,
    tab_bar: &adw::TabBar,
    tab_view: &adw::TabView,
    click: &gtk::GestureClick,
    x: f64,
    y: f64,
) {
    let behavior = app.settings().tab_middle_click();
    if behavior == TabMiddleClick::Close {
        return;
    }
    let Some((page, tab)) = tab_at_tab_bar_point(app, tab_bar, x, y) else {
        return;
    };
    tab_view.set_selected_page(&page);
    if behavior == TabMiddleClick::Paste && terminal::can_paste(&tab.terminal) {
        terminal::paste(&tab.terminal);
    }
    click.set_state(gtk::EventSequenceState::Claimed);
}

/// Asks before closing a tab that runs a program other than a shell
/// (`ptyxis_window_close_page_cb()`). Confirming ends the program; the
/// tab is still parked like any other closed one.
fn close_page(app: &Rc<App>, tab_view: &adw::TabView, page: &adw::TabPage) -> glib::Propagation {
    save_size(app, tab_view);
    // A released tab belongs to a window that is going away.
    let Some(tab) = app.tab(&page.child()) else {
        return glib::Propagation::Proceed;
    };
    if !tab.is_running() || !app.settings().prompt_on_close() {
        return glib::Propagation::Proceed;
    }
    let pages = [(page.clone(), close_dialog_row(&tab, page))];
    let weak_tab_view = tab_view.downgrade();
    let page = page.clone();
    confirm_close(tab_view, &pages, move |confirmed| {
        if confirmed {
            tab.sender.emit(crate::tab::TabInput::ForceQuit);
        }
        if let Some(tab_view) = weak_tab_view.upgrade() {
            tab_view.close_page_finish(&page, confirmed);
        }
    });
    glib::Propagation::Stop
}

/// What a window closes with and through (`ptyxis_window_close_request()`).
struct Closing {
    app: Rc<App>,
    window: adw::ApplicationWindow,
    tab_view: adw::TabView,
    in_close_request: Rc<Cell<bool>>,
    parked: Rc<RefCell<ParkingLot<gtk::Widget>>>,
}

impl Closing {
    /// Hands the window's tabs back and lets it go. `running` tabs are
    /// ended right away, as the user confirmed.
    fn release(&self, running: &[gtk::Widget]) {
        self.in_close_request.set(true);
        for child in running {
            self.app.force_quit_tab(child);
        }
        release_tabs(&self.app, &self.tab_view);
        release_parked(&self.app, &self.parked);
    }

    /// The window size is saved first, then closing the last window saves
    /// the session. With `prompt-on-close` on, tabs without a running
    /// program close at once and the user is asked about the rest, which
    /// keeps the window open until they confirm.
    fn request(&self, kind: WindowKind) -> glib::Propagation {
        save_size(&self.app, &self.tab_view);
        if kind.saves_session() && self.app.is_last_window(self.window.upcast_ref()) {
            self.app.save_session();
        }
        if !self.app.settings().prompt_on_close() {
            self.release(&[]);
            return glib::Propagation::Proceed;
        }

        // Last to first, like the C loop.
        let mut running = Vec::new();
        let mut idle = Vec::new();
        for page in pages(&self.tab_view).rev() {
            match self.app.tab(&page.child()).filter(TabRef::is_running) {
                Some(tab) => {
                    let row = close_dialog_row(&tab, &page);
                    running.push((page, row));
                }
                None => idle.push(page),
            }
        }
        if running.is_empty() {
            self.release(&[]);
            return glib::Propagation::Proceed;
        }

        self.in_close_request.set(true);
        for page in idle {
            self.tab_view.close_page(&page);
        }
        let children: Vec<_> = running.iter().map(|(page, _)| page.child()).collect();
        let closing = Closing {
            app: Rc::clone(&self.app),
            window: self.window.clone(),
            tab_view: self.tab_view.clone(),
            in_close_request: Rc::clone(&self.in_close_request),
            parked: Rc::clone(&self.parked),
        };
        // ptyxis_window_close_request_cb()
        confirm_close(&self.tab_view, &running, move |confirmed| {
            if confirmed {
                closing.release(&children);
                closing.window.destroy();
            } else {
                closing.in_close_request.set(false);
            }
        });
        glib::Propagation::Stop
    }
}

/// The pages of `tab_view`, first to last.
pub fn pages(tab_view: &adw::TabView) -> impl DoubleEndedIterator<Item = adw::TabPage> {
    let pages = tab_view.pages();
    (0..pages.n_items()).filter_map(move |i| pages.item(i).and_downcast::<adw::TabPage>())
}

/// Forgets all tabs of a window that is going away.
fn release_tabs(app: &App, tab_view: &adw::TabView) {
    for page in pages(tab_view) {
        app.remove_tab(&page.child());
    }
}

/// Ends the closed tabs of a window that is going away, which can no
/// longer be reopened (`ptyxis_parking_lot_dispose()`).
fn release_parked(app: &App, parked: &RefCell<ParkingLot<gtk::Widget>>) {
    // Collected first so no borrow is held while tabs are dropped.
    let children: Vec<_> = parked.borrow_mut().drain().collect();
    for child in children {
        app.force_quit_tab(&child);
    }
}

/// Adds the actions only keyboard shortcuts activate. Ptyxis installs
/// `tab.focus(i)` on the window and the terminal shortcuts act on the
/// terminal in focus; here they are window actions
/// working on the selected tab's terminal.
///
/// `win.focus-tab(n)` selects the nth tab, or the last for 0 or a number
/// past the end (`ptyxis_window_tab_focus_action()`).
fn add_shortcut_actions(
    window: &adw::ApplicationWindow,
    app: &Rc<App>,
    tab_view: &adw::TabView,
    primary_menu_button: &gtk::MenuButton,
    new_tab_button: &adw::SplitButton,
) {
    let weak = tab_view.downgrade();
    let focus_tab = gio::SimpleAction::new("focus-tab", Some(glib::VariantTy::INT32));
    focus_tab.connect_activate(move |_, position| {
        let (Some(tab_view), Some(position)) =
            (weak.upgrade(), position.and_then(|v| v.get::<i32>()))
        else {
            return;
        };
        let n_pages = tab_view.n_pages();
        if n_pages == 0 {
            return;
        }
        let nth = match position {
            1..=i32::MAX if position <= n_pages => position - 1,
            _ => n_pages - 1,
        };
        tab_view.set_selected_page(&tab_view.nth_page(nth));
    });
    window.add_action(&focus_tab);

    let terminal_action = |name: &str,
                           target: Option<&glib::VariantTy>,
                           run: fn(&vte::Terminal, Option<bool>)| {
        let action = gio::SimpleAction::new(name, target);
        let weak_app = Rc::downgrade(app);
        let weak_view = tab_view.downgrade();
        action.connect_activate(move |_, parameter| {
            let terminal = weak_app
                .upgrade()
                .zip(weak_view.upgrade())
                .and_then(|(app, view)| {
                    let child = view.selected_page()?.child();
                    app.tab(&child).map(|tab| tab.terminal.clone())
                });
            if let Some(terminal) = terminal {
                run(&terminal, parameter.and_then(|v| v.get::<bool>()));
            }
        });
        window.add_action(&action);
    };
    terminal_action(
        "select-all",
        Some(glib::VariantTy::BOOLEAN),
        |terminal, select| {
            if select.unwrap_or(true) {
                terminal.select_all();
            } else {
                terminal.unselect_all();
            }
        },
    );
    terminal_action(
        "reset",
        Some(glib::VariantTy::BOOLEAN),
        |terminal, clear| {
            terminal.reset(true, clear.unwrap_or(false));
        },
    );

    let weak = primary_menu_button.downgrade();
    let primary_menu = gio::SimpleAction::new("primary-menu", None);
    primary_menu.connect_activate(move |_, _| {
        if let Some(button) = weak.upgrade() {
            button.popup();
        }
    });
    window.add_action(&primary_menu);

    // The new tab button's dropdown is the "tab menu" (`new_terminal_menu_button`).
    let weak = new_tab_button.downgrade();
    let tab_menu = gio::SimpleAction::new("tab-menu", None);
    tab_menu.connect_activate(move |_, _| {
        if let Some(button) = weak.upgrade() {
            button.popup();
        }
    });
    window.add_action(&tab_menu);
}

/// Adds `win.fullscreen`, `win.unfullscreen` and `win.toggle-fullscreen`.
/// The first two are each enabled only when it would change something, so
/// "Leave Fullscreen" shows in the terminal menu only in fullscreen
/// (`ptyxis_window_toplevel_state_changed_cb()`), which also hands the state
/// to the `fullscreen_box` and marks the window with the `fullscreen` class.
fn track_fullscreen(window: &adw::ApplicationWindow, fullscreen_box: FullscreenBox) {
    let weak = window.downgrade();
    let fullscreen = RelmAction::<FullscreenAction>::new_stateless(move |_| {
        if let Some(window) = weak.upgrade() {
            window.fullscreen();
        }
    });
    let weak = window.downgrade();
    let unfullscreen = RelmAction::<UnfullscreenAction>::new_stateless(move |_| {
        if let Some(window) = weak.upgrade() {
            window.unfullscreen();
        }
    });
    let fullscreen = fullscreen.gio_action().clone();
    let unfullscreen = unfullscreen.gio_action().clone();
    window.add_action(&fullscreen);
    window.add_action(&unfullscreen);
    let weak = window.downgrade();
    let toggle = RelmAction::<ToggleFullscreenAction>::new_stateless(move |_| {
        if let Some(window) = weak.upgrade() {
            if window.is_fullscreen() {
                window.unfullscreen();
            } else {
                window.fullscreen();
            }
        }
    });
    window.add_action(toggle.gio_action());
    let was_fullscreen = Cell::new(false);
    let update = move |window: &adw::ApplicationWindow| {
        let is_fullscreen = window.is_fullscreen();
        fullscreen.set_enabled(!is_fullscreen);
        unfullscreen.set_enabled(is_fullscreen);
        fullscreen_box.set_fullscreen(is_fullscreen);
        // The bars slide in when entering fullscreen so the way out is seen.
        if is_fullscreen && !was_fullscreen.get() {
            fullscreen_box.reveal();
        }
        was_fullscreen.set(is_fullscreen);
        if is_fullscreen {
            window.add_css_class("fullscreen");
        } else {
            window.remove_css_class("fullscreen");
        }
    };
    update(window);
    window.connect_fullscreened_notify(update);
}

/// Adds `win.set-profile`, which \"Change Profile\" needs a tab for and
/// more than one profile to choose from.
fn track_set_profile(
    window: &adw::ApplicationWindow,
    app: &Rc<App>,
    tab_view: &adw::TabView,
    sender: &ComponentSender<Window>,
) {
    let action = add_action::<SetProfileAction>(window, sender, || WindowInput::SetProfile);
    let update = {
        let weak_app = Rc::downgrade(app);
        let weak_view = tab_view.downgrade();
        move || {
            let (Some(app), Some(tab_view)) = (weak_app.upgrade(), weak_view.upgrade()) else {
                return;
            };
            action.set_enabled(tab_view.selected_page().is_some() && app.profile_count() > 1);
        }
    };
    update();
    let notify = update.clone();
    tab_view.connect_selected_page_notify(move |_| notify());
    // The profile menu is rebuilt whenever profiles come or go.
    app.profile_menu()
        .connect_items_changed(move |_, _, _, _| update());
}

/// Whether `win.paste-clipboard` applies: Ptyxis's `clipboard.paste` lives
/// on the terminal, so it needs a tab, and is enabled only while the
/// clipboard offers text (`ptyxis_terminal_update_clipboard_actions()`).
fn update_paste_action(action: &gio::SimpleAction, tab_view: &adw::TabView) {
    let clipboard = tab_view.clipboard();
    action.set_enabled(
        tab_view.selected_page().is_some() && terminal::clipboard_has_text(&clipboard),
    );
}

/// Re-checks `win.paste-clipboard` whenever the clipboard changes, as each
/// `PtyxisTerminal` does from its clipboard's `changed` signal. The
/// clipboard is the display's and outlives the window, so the handler holds
/// weak references and goes away with the window.
fn track_clipboard(
    window: &adw::ApplicationWindow,
    tab_view: &adw::TabView,
    action: &gio::SimpleAction,
) {
    let clipboard = tab_view.clipboard();
    let weak_view = tab_view.downgrade();
    let weak_action = action.downgrade();
    let id = clipboard.connect_changed(move |_| {
        if let (Some(tab_view), Some(action)) = (weak_view.upgrade(), weak_action.upgrade()) {
            update_paste_action(&action, &tab_view);
        }
    });
    drop_on_destroy(window, SignalGuard::new(&clipboard, id));
}

/// Keeps `win.pin-tab` / `win.unpin-tab` in line with the selected page
/// (`ptyxis_window_selected_page_notify_pinned_cb()`).
///
/// With `disable_first_page`, both stay disabled when the first page is
/// selected, as `ptyxis_window_new_for_command()` disables them after adding
/// its tab. As in Ptyxis, selecting another page or (un)pinning re-enables
/// them, though a command window offers no way to do either.
fn track_pinned(
    tab_view: &adw::TabView,
    pin: gio::SimpleAction,
    unpin: gio::SimpleAction,
    disable_first_page: bool,
) -> glib::SignalGroup {
    let disable_next_page = Rc::new(Cell::new(disable_first_page));
    let update = move |page: Option<&adw::TabPage>| {
        let pinned = page
            .filter(|_| !disable_next_page.replace(false))
            .map(adw::TabPage::is_pinned);
        pin.set_enabled(pinned == Some(false));
        unpin.set_enabled(pinned == Some(true));
    };
    update(None);
    let signals = glib::SignalGroup::new::<adw::TabPage>();
    let on_notify = update.clone();
    signals.connect_notify_local(Some("pinned"), move |page, _| {
        on_notify(page.downcast_ref());
    });
    let on_bind = update.clone();
    signals.connect_bind_local(move |_, page| on_bind(page.downcast_ref()));
    signals.connect_unbind_local(move |_| update(None));
    tab_view
        .bind_property("selected-page", &signals, "target")
        .sync_create()
        .build();
    signals
}

const HIDDEN_WHEN_ATTRIBUTE: &str = "hidden-when";
const HIDDEN_WHEN_ACTION_DISABLED: &str = "action-disabled";

/// Hides `item` while its action is disabled, as the
/// `hidden-when="action-disabled"` attribute does in Ptyxis's menus.
pub fn hide_when_disabled(item: &gio::MenuItem) {
    item.set_attribute_value(
        HIDDEN_WHEN_ATTRIBUTE,
        Some(&HIDDEN_WHEN_ACTION_DISABLED.to_variant()),
    );
}

/// The tab context menu of `src/ptyxis-window.ui`.
fn build_tab_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<MoveTabLeftAction>::to_menu_item("Move _Left"));
    section.append_item(&RelmAction::<MoveTabRightAction>::to_menu_item(
        "Move _Right",
    ));
    section.append_item(&RelmAction::<DetachTabAction>::to_menu_item(
        "Move to New _Window",
    ));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    for item in [
        RelmAction::<PinTabAction>::to_menu_item("_Pin Tab"),
        RelmAction::<UnpinTabAction>::to_menu_item("Un_pin Tab"),
    ] {
        hide_when_disabled(&item);
        section.append_item(&item);
    }
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<SetTitleAction>::to_menu_item(SET_TITLE_LABEL));
    section.append_item(&RelmAction::<SetTabColorAction>::to_menu_item(
        "Set _Color…",
    ));
    let reset_color = RelmAction::<ResetTabColorAction>::to_menu_item("_Reset Color");
    hide_when_disabled(&reset_color);
    section.append_item(&reset_color);
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<CloseOtherTabsAction>::to_menu_item(
        "Close _Other Tabs",
    ));
    section.append_item(&RelmAction::<CloseTabAction>::to_menu_item("_Close Tab"));
    menu.append_section(None, &section);
    menu
}

/// Custom menu item id where the theme selector is placed.
const INTERFACE_STYLE_ITEM: &str = "interface-style";
/// Custom menu item id where the zoom controls are placed.
const ZOOM_ITEM: &str = "zoom";

/// A section holding only the custom menu item `id`.
fn custom_item_section(id: &str) -> gio::Menu {
    let section = gio::Menu::new();
    let item = gio::MenuItem::new(None, None);
    item.set_attribute_value("custom", Some(&id.to_variant()));
    section.append_item(&item);
    section
}

fn build_primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_section(None, &custom_item_section(INTERFACE_STYLE_ITEM));
    menu.append_section(None, &custom_item_section(ZOOM_ITEM));
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<NewWindowAction>::to_menu_item("New _Window"));
    section.append_item(&RelmAction::<CloseTabAction>::to_menu_item("_Close Tab"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    let fullscreen = RelmAction::<FullscreenAction>::to_menu_item("Fullscreen");
    hide_when_disabled(&fullscreen);
    section.append_item(&fullscreen);
    let unfullscreen = RelmAction::<UnfullscreenAction>::to_menu_item("Leave Fullscreen");
    hide_when_disabled(&unfullscreen);
    section.append_item(&unfullscreen);
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<PreferencesAction>::to_menu_item(
        "_Preferences",
    ));
    section.append_item(&RelmAction::<ShowKeyboardShortcutsAction>::to_menu_item(
        "_Keyboard Shortcuts",
    ));
    let about_label = format!("_About {}", config::APP_NAME);
    section.append(Some(&about_label), Some(ABOUT_ACTION));
    menu.append_section(None, &section);
    menu
}

/// Application action showing the About dialog (`app.about`).
const ABOUT_ACTION: &str = "app.about";

impl Component for Window {
    type Init = WindowInit;
    type Input = WindowInput;
    type Output = ();
    type CommandOutput = ();
    type Root = adw::ApplicationWindow;
    type Widgets = ();

    fn init_root() -> Self::Root {
        adw::ApplicationWindow::builder()
            .application(&relm4::main_application())
            .title(config::APP_NAME)
            .width_request(MIN_WIDTH)
            .height_request(MIN_HEIGHT)
            .build()
    }

    fn init(
        init: Self::Init,
        window: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let app = init.app;
        let kind = init.kind;
        if config::APP_ID.ends_with(".Devel") {
            window.add_css_class(DEVEL_STYLE_CLASS);
        }

        let tab_view = adw::TabView::new();
        tab_view.set_default_icon(&gio::ThemedIcon::new(DEFAULT_ICON_NAME));
        tab_view.set_vexpand(true);
        // Our own GSettings-driven shortcuts replace libadwaita's defaults.
        tab_view.set_shortcuts(adw::TabViewShortcuts::empty());
        tab_view.set_menu_model(Some(&build_tab_menu()));

        // A command window starts without its tab bar and shows it only if
        // tabs are dragged in, never in a standalone instance.
        let tab_bar = adw::TabBar::builder()
            .view(&tab_view)
            .autohide(kind.has_tab_actions())
            .visible(kind.has_tab_actions())
            .build();

        let window_title = adw::WindowTitle::new(config::APP_NAME, "");
        let header = adw::HeaderBar::builder()
            .title_widget(&window_title)
            .build();
        let new_tab_button = adw::SplitButton::builder()
            .icon_name("tab-new-symbolic")
            .tooltip_text("New Tab")
            .dropdown_tooltip("New Tab with Profile")
            .menu_model(app.profile_menu())
            .action_name(NewTabAction::action_name())
            .focus_on_click(false)
            .build();
        new_tab_button.set_visible(kind.has_tab_actions());
        header.pack_start(&new_tab_button);
        let primary_menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .menu_model(&build_primary_menu())
            .focus_on_click(false)
            .build();
        header.pack_end(&primary_menu_button);
        let tab_overview_button = gtk::ToggleButton::builder()
            .icon_name("view-grid-symbolic")
            .tooltip_text("Show Open Tabs")
            .focus_on_click(false)
            .visible(kind.has_tab_actions())
            .build();
        header.pack_end(&tab_overview_button);
        // The window buttons give way to "Leave Fullscreen" in fullscreen.
        let restore_button = gtk::Button::builder()
            .icon_name("view-restore-symbolic")
            .tooltip_text("Leave Fullscreen")
            .action_name(UnfullscreenAction::action_name())
            .visible(false)
            .build();
        header.pack_end(&restore_button);
        window
            .bind_property("fullscreened", &restore_button, "visible")
            .sync_create()
            .build();
        window
            .bind_property("fullscreened", &header, "show-start-title-buttons")
            .sync_create()
            .invert_boolean()
            .build();
        window
            .bind_property("fullscreened", &header, "show-end-title-buttons")
            .sync_create()
            .invert_boolean()
            .build();
        add_theme_controls(&window, &primary_menu_button, app.settings());
        let zoom_label = add_zoom_controls(&primary_menu_button);

        let find_bar = FindBar::builder()
            .launch(())
            .forward(sender.input_sender(), |output| match output {
                FindBarOutput::Dismissed => WindowInput::FindDismissed,
            });
        // No transition: VTE does not resize gracefully during one.
        let find_bar_revealer = gtk::Revealer::builder()
            .reveal_child(false)
            .transition_duration(0)
            .child(find_bar.widget())
            .build();

        // One footer per window, below the find bar, hidden at first.
        let telemetry_bar = TelemetryBar::builder()
            .launch(app.settings().clone())
            .detach();
        let telemetry_revealer = gtk::Revealer::builder()
            .reveal_child(false)
            .transition_duration(0)
            .child(telemetry_bar.widget())
            .build();

        // Copies from a terminal announce themselves here
        // (`ptyxis_terminal_toast()`).
        let toasts = adw::ToastOverlay::new();
        toasts.set_vexpand(true);
        toasts.set_child(Some(&tab_view));

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&toasts);
        content.append(&find_bar_revealer);
        content.append(&telemetry_revealer);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_css_class(WINDOW_CONTENTS_CLASS);
        let visual_bell = gtk::Box::new(gtk::Orientation::Vertical, 0);
        visual_bell.add_css_class(VISUAL_BELL_AREA_CLASS);
        visual_bell.append(&header);
        toolbar_view.add_top_bar(&visual_bell);
        toolbar_view.add_top_bar(&tab_bar);
        // Runs before the tabs' own middle-click gesture so it can keep a
        // tab from closing (`ptyxis_window_init()`).
        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_MIDDLE)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let weak_app = Rc::downgrade(&app);
        let weak_tab_view = tab_view.downgrade();
        click.connect_pressed(move |click, _, x, y| {
            let Some(app) = weak_app.upgrade() else {
                return;
            };
            let Some(tab_view) = weak_tab_view.upgrade() else {
                return;
            };
            let Some(tab_bar) = click.widget().and_downcast::<adw::TabBar>() else {
                return;
            };
            tab_bar_middle_click_pressed(&app, &tab_bar, &tab_view, click, x, y);
        });
        tab_bar.add_controller(click);
        toolbar_view.set_content(Some(&content));
        let fullscreen_box = FullscreenBox::new(
            &window,
            &toolbar_view,
            &[visual_bell.clone().upcast(), tab_bar.clone().upcast()],
        );
        let tab_overview = adw::TabOverview::builder()
            .view(&tab_view)
            .child(&toolbar_view)
            .enable_new_tab(true)
            .build();
        tab_overview
            .bind_property("open", &tab_overview_button, "active")
            .bidirectional()
            .sync_create()
            .build();
        let input = sender.input_sender().clone();
        tab_overview.connect_open_notify(move |overview| {
            if !overview.is_open() {
                input.emit(WindowInput::OverviewClosed);
            }
        });
        // The overview needs the page right away, so the tab is created
        // here rather than through a message (`win.new-tab` semantics).
        let weak_app = Rc::downgrade(&app);
        let weak_tab_view = tab_view.downgrade();
        tab_overview.connect_create_tab(move |_| {
            let tab_view = weak_tab_view
                .upgrade()
                .expect("the tab overview outlives its tab view");
            if let Some(app) = weak_app.upgrade() {
                let request = inherited_request(&app, &tab_view, None);
                match insert_tab(&app, &tab_view, request) {
                    Ok(page) => return page,
                    Err(e) => glib::g_critical!("tty-rex", "Failed to create tab: {e}"),
                }
            }
            // The overview is only open while the window has tabs.
            tab_view
                .selected_page()
                .expect("an open tab overview has a selected tab")
        });
        window.set_content(Some(&tab_overview));

        let title_bindings = glib::BindingGroup::new();
        title_bindings
            .bind("title", &window_title, "title")
            .sync_create()
            .build();
        title_bindings
            .bind("title", &window, "title")
            .sync_create()
            .build();

        let new_tab = add_action::<NewTabAction>(&window, &sender, || WindowInput::NewTab {
            profile_uuid: None,
            container_id: None,
        });
        let input = sender.input_sender().clone();
        let new_tab_with_profile =
            RelmAction::<NewTabWithProfileAction>::new_with_target_value(move |_, uuid| {
                input.emit(WindowInput::NewTab {
                    profile_uuid: Some(uuid),
                    container_id: None,
                })
            });
        window.add_action(new_tab_with_profile.gio_action());
        let input = sender.input_sender().clone();
        let new_tab_in_container =
            RelmAction::<NewTabInContainerAction>::new_with_target_value(move |_, id| {
                input.emit(WindowInput::NewTab {
                    profile_uuid: None,
                    container_id: Some(id),
                })
            });
        window.add_action(new_tab_in_container.gio_action());
        let new_window = add_action::<NewWindowAction>(&window, &sender, || WindowInput::NewWindow);
        add_action::<CloseTabAction>(&window, &sender, || WindowInput::CloseTab);
        let close_others =
            add_action::<CloseOtherTabsAction>(&window, &sender, || WindowInput::CloseOtherTabs);
        let undo_close =
            add_action::<UndoCloseTabAction>(&window, &sender, || WindowInput::UndoCloseTab);
        let move_left =
            add_action::<MoveTabLeftAction>(&window, &sender, || WindowInput::MoveTabLeft);
        let move_right =
            add_action::<MoveTabRightAction>(&window, &sender, || WindowInput::MoveTabRight);
        let detach = add_action::<DetachTabAction>(&window, &sender, || WindowInput::DetachTab);
        let pin = add_action::<PinTabAction>(&window, &sender, || WindowInput::SetTabPinned(true));
        let unpin =
            add_action::<UnpinTabAction>(&window, &sender, || WindowInput::SetTabPinned(false));
        let pinned_signals = track_pinned(&tab_view, pin, unpin, !kind.has_tab_actions());
        let next = add_action::<NextTabAction>(&window, &sender, || WindowInput::NextTab);
        let previous =
            add_action::<PreviousTabAction>(&window, &sender, || WindowInput::PreviousTab);
        let copy_action = add_action::<CopyAction>(&window, &sender, || WindowInput::Copy);
        copy_action.set_enabled(false);
        let paste_action = add_action::<PasteAction>(&window, &sender, || WindowInput::Paste);
        paste_action.set_enabled(false);
        track_clipboard(&window, &tab_view, &paste_action);
        let zoom_controls = ZoomControls {
            zoom_in: add_action::<ZoomInAction>(&window, &sender, || WindowInput::ZoomIn),
            zoom_out: add_action::<ZoomOutAction>(&window, &sender, || WindowInput::ZoomOut),
            zoom_one: add_action::<ZoomOneAction>(&window, &sender, || WindowInput::ZoomReset),
            label: zoom_label,
        };
        add_action::<SearchAction>(&window, &sender, || WindowInput::ShowFind);
        let input = sender.input_sender().clone();
        let toggle_telemetry =
            RelmAction::<ToggleTelemetryAction>::new_stateful(&false, move |_, visible| {
                *visible = !*visible;
                input.emit(WindowInput::SetTelemetryVisible(*visible));
            });
        window.add_action(toggle_telemetry.gio_action());
        add_action::<SetTitleAction>(&window, &sender, || WindowInput::SetTitle);
        add_action::<SetTabColorAction>(&window, &sender, || WindowInput::SetTabColor);
        let reset_color =
            add_action::<ResetTabColorAction>(&window, &sender, || WindowInput::ResetTabColor);
        // The tab menu acts on the selected tab, so select the one it was
        // opened for (`ptyxis_window_setup_menu_cb()`). Both the tab bar and
        // the overview open the menu through here, so it is also where
        // "Reset Color" is offered only for a tab that has a color; when
        // run otherwise (D-Bus), resetting a tab without one does nothing.
        reset_color.set_enabled(false);
        let weak_app = Rc::downgrade(&app);
        tab_view.connect_setup_menu(move |tab_view, page| {
            let Some(page) = page else {
                return;
            };
            tab_view.set_selected_page(page);
            let has_color = weak_app
                .upgrade()
                .and_then(|app| app.tab(&page.child()))
                .is_some_and(|tab| tab.color.get().is_some());
            reset_color.set_enabled(has_color);
        });
        let overview =
            add_action::<TabOverviewAction>(&window, &sender, || WindowInput::ToggleOverview);
        // A command window is about its one command, so it offers no way to
        // add, reorder or switch tabs (`ptyxis_window_new_for_command()`);
        // track_pinned() disables `win.pin-tab` and `win.unpin-tab` once the
        // command's tab is in.
        if !kind.has_tab_actions() {
            for action in [
                &new_tab,
                &new_window,
                new_tab_with_profile.gio_action(),
                new_tab_in_container.gio_action(),
                &overview,
                &move_left,
                &move_right,
                &close_others,
                &detach,
                &next,
                &previous,
                &undo_close,
            ] {
                action.set_enabled(false);
            }
        }
        track_fullscreen(&window, fullscreen_box.clone());
        add_shortcut_actions(
            &window,
            &app,
            &tab_view,
            &primary_menu_button,
            &new_tab_button,
        );
        track_set_profile(&window, &app, &tab_view, &sender);
        add_action::<PreferencesAction>(&window, &sender, || WindowInput::ShowPreferences);
        add_action::<ShowKeyboardShortcutsAction>(&window, &sender, || {
            WindowInput::ShowKeyboardShortcuts
        });

        let shortcuts = Shortcuts::new()
            .inspect(|shortcuts| window.add_controller(shortcuts.controller().clone()))
            .inspect_err(|e| glib::g_warning!("tty-rex", "Keyboard shortcuts unavailable: {e}"))
            .ok();

        let input = sender.input_sender().clone();
        tab_view
            .connect_selected_page_notify(move |_| input.emit(WindowInput::SelectedPageChanged));
        let input = sender.input_sender().clone();
        tab_view.connect_page_detached(move |tab_view, page, _| {
            input.emit(WindowInput::PageDetached {
                child: page.child(),
                transferring: tab_view.is_transferring_page(),
            })
        });
        let input = sender.input_sender().clone();
        tab_view.connect_is_transferring_page_notify(move |tab_view| {
            if !tab_view.is_transferring_page() {
                input.emit(WindowInput::TransferFinished);
            }
        });
        // The tab bar of an ordinary window hides itself while there is a
        // single tab; a command window's is shown and hidden here.
        let weak_window = window.downgrade();
        let weak_tab_bar = tab_bar.downgrade();
        tab_view.connect_n_pages_notify(move |tab_view| {
            let visible = kind.shows_tab_bar(tab_view.n_pages());
            if let Some(window) = weak_window.upgrade() {
                if visible {
                    window.add_css_class(HAS_TAB_BAR_CLASS);
                } else {
                    window.remove_css_class(HAS_TAB_BAR_CLASS);
                }
            }
            if !kind.has_tab_actions()
                && let Some(tab_bar) = weak_tab_bar.upgrade()
            {
                tab_bar.set_visible(visible);
            }
        });
        let weak_app = Rc::downgrade(&app);
        tab_view.connect_create_window(move |_| {
            let app = weak_app.upgrade()?;
            let window = app.new_window();
            window.window.present();
            Some(window.tab_view)
        });

        // Whether a tab may close is decided before libadwaita closes it,
        // so this cannot wait for a message either.
        let weak_app = Rc::downgrade(&app);
        tab_view.connect_close_page(move |tab_view, page| match weak_app.upgrade() {
            Some(app) => close_page(&app, tab_view, page),
            None => glib::Propagation::Proceed,
        });

        // Tabs must be released while the tab view still has its pages, so
        // this runs synchronously rather than through a message.
        let weak_app = Rc::downgrade(&app);
        let weak_tab_view = tab_view.downgrade();
        let in_close_request = Rc::new(Cell::new(false));
        let closing = Rc::clone(&in_close_request);
        let parked = Rc::new(RefCell::new(ParkingLot::default()));
        let closing_parked = Rc::clone(&parked);
        window.connect_close_request(move |window| {
            let (Some(app), Some(tab_view)) = (weak_app.upgrade(), weak_tab_view.upgrade()) else {
                closing.set(true);
                return glib::Propagation::Proceed;
            };
            Closing {
                app,
                window: window.clone(),
                tab_view,
                in_close_request: Rc::clone(&closing),
                parked: Rc::clone(&closing_parked),
            }
            .request(kind)
        });

        // Like ptyxis_window_notify_is_active_cb().
        window.connect_is_active_notify(|window| {
            if window.is_active() {
                tab_notify::set_urgency_hint(window.upcast_ref(), false);
            }
        });

        let model = Window {
            app,
            window,
            tab_view,
            window_title,
            title_bindings,
            find_bar,
            find_bar_revealer,
            telemetry_bar,
            telemetry_revealer,
            _shortcuts: shortcuts,
            copy_action,
            paste_action,
            zoom_controls,
            _pinned_signals: pinned_signals,
            terminal_handlers: Vec::new(),
            tab_css_class: None,
            input: sender.input_sender().clone(),
            visual_bell,
            visual_bell_serial: 0,
            tab_overview,
            overview_close_serial: 0,
            in_close_request,
            kind,
            parked,
            fullscreen_box,
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        use crate::tab::TabInput;
        match message {
            WindowInput::AddTab(request) => self.add_tab(*request),
            WindowInput::AddTabs(requests) => self.add_tabs(requests),
            WindowInput::NewTab {
                profile_uuid,
                container_id,
            } => {
                let mut request = self.inherited_request(profile_uuid);
                // An unknown container leaves the inherited or default one.
                if let Some(container) =
                    container_id.and_then(|id| self.app.containers().lookup(&id))
                {
                    request.container = Some(container);
                }
                self.add_tab(request);
            }
            WindowInput::NewWindow => {
                let request = self.inherited_request(None);
                let window = self.app.new_window();
                window.sender.emit(WindowInput::AddTab(Box::new(request)));
                window.window.present();
            }
            WindowInput::CloseTab => {
                if let Some(page) = self.tab_view.selected_page() {
                    self.tab_view.close_page(&page);
                }
            }
            WindowInput::CloseOtherTabs => {
                if let Some(page) = self.tab_view.selected_page() {
                    self.tab_view.close_other_pages(&page);
                }
            }
            WindowInput::UndoCloseTab => self.undo_close_tab(),
            WindowInput::ParkedTabExpired(serial) => {
                let expired = self.parked.borrow_mut().expire(serial);
                if let Some(child) = expired {
                    self.app.force_quit_tab(&child);
                }
            }
            WindowInput::MoveTabLeft => self.move_selected_tab(adw::TabView::reorder_backward),
            WindowInput::MoveTabRight => self.move_selected_tab(adw::TabView::reorder_forward),
            WindowInput::DetachTab => {
                // A transfer keeps the tab registered with the App (see
                // `PageDetached`), so the tab survives the move.
                if let Some(page) = self.tab_view.selected_page() {
                    let window = self.app.new_window();
                    self.tab_view.transfer_page(&page, &window.tab_view, 0);
                    window.window.present();
                }
            }
            WindowInput::SetTabPinned(pinned) => {
                if let Some(page) = self.tab_view.selected_page() {
                    self.tab_view.set_page_pinned(&page, pinned);
                }
            }
            WindowInput::NextTab => {
                self.tab_view.select_next_page();
            }
            WindowInput::PreviousTab => {
                self.tab_view.select_previous_page();
            }
            WindowInput::Copy => self.send_to_active_tab(TabInput::Copy),
            WindowInput::Paste => self.send_to_active_tab(TabInput::Paste),
            WindowInput::ZoomIn => self.send_to_active_tab(TabInput::ZoomIn),
            WindowInput::ZoomOut => self.send_to_active_tab(TabInput::ZoomOut),
            WindowInput::ZoomReset => self.send_to_active_tab(TabInput::ZoomReset),
            WindowInput::ShowFind => {
                if self.tab_view.selected_page().is_some() {
                    self.find_bar_revealer.set_reveal_child(true);
                    self.find_bar.emit(FindBarInput::Focus);
                }
            }
            WindowInput::SetProfile => {
                self.send_to_active_tab(crate::tab::TabInput::ShowProfileDialog(
                    self.app.profile_list(),
                ));
            }
            WindowInput::SetTitle => self.send_to_active_tab(TabInput::ShowTitleDialog),
            WindowInput::SetTabColor => self.send_to_active_tab(TabInput::ShowColorDialog),
            WindowInput::ResetTabColor => self.send_to_active_tab(TabInput::ResetColor),
            WindowInput::ShowPreferences => {
                self.app.show_preferences(Some(self.window.upcast_ref()));
            }
            WindowInput::ShowKeyboardShortcuts => {
                self.app.show_shortcuts(Some(self.window.upcast_ref()));
            }
            WindowInput::SetTelemetryVisible(visible) => {
                self.telemetry_revealer.set_reveal_child(visible);
                self.telemetry_bar
                    .emit(TelemetryBarInput::SetVisible(visible));
            }
            WindowInput::FindDismissed => {
                self.find_bar_revealer.set_reveal_child(false);
                self.send_to_active_tab(TabInput::Focus);
            }
            WindowInput::SelectedPageChanged => self.on_selected_page_changed(),
            WindowInput::Bell => self.visual_bell(),
            WindowInput::ToggleOverview => {
                self.tab_overview.set_open(!self.tab_overview.is_open());
            }
            WindowInput::OverviewClosed => self.overview_closed(),
            WindowInput::OverviewCloseFinished(serial) => {
                if serial == self.overview_close_serial && !self.tab_overview.is_open() {
                    self.send_to_active_tab(TabInput::Focus);
                }
            }
            WindowInput::VisualBellDone(serial) => {
                if serial == self.visual_bell_serial {
                    self.visual_bell.remove_css_class(VISUAL_BELL_CLASS);
                }
            }
            WindowInput::PageDetached {
                child,
                transferring,
            } => {
                if let Some(tab) = self.app.tab(&child) {
                    tab.sender.emit(TabInput::Detached);
                }
                if !transferring {
                    self.park_tab(child);
                    self.close_if_empty();
                }
            }
            WindowInput::TransferFinished => self.close_if_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_menu_custom_items() {
        // Ptyxis's primary menu opens with the theme selector, then the
        // zoom controls, each in its own section.
        let menu = build_primary_menu();
        for (index, id) in [(0, INTERFACE_STYLE_ITEM), (1, ZOOM_ITEM)] {
            let section = menu
                .item_link(index, gio::MENU_LINK_SECTION)
                .expect("section");
            assert_eq!(section.n_items(), 1, "{id}");
            let custom = section
                .item_attribute_value(0, "custom", Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>());
            assert_eq!(custom.as_deref(), Some(id));
        }
    }

    #[test]
    fn window_kind_for_command_line() {
        use WindowKind::*;
        for (plain_execute, standalone, expected) in [
            (false, false, Tabs),
            (false, true, Tabs),
            (true, false, Command),
            (true, true, SingleTerminal),
        ] {
            assert_eq!(
                WindowKind::for_command_line(plain_execute, standalone),
                expected,
                "plain_execute={plain_execute} standalone={standalone}"
            );
        }
    }

    #[test]
    fn window_kind_behavior() {
        use WindowKind::*;
        // (kind, tab actions, saves session, tab bar with 1 / 2 tabs)
        for (kind, actions, saves, bar1, bar2) in [
            (Tabs, true, true, false, true),
            (Command, false, true, false, true),
            (SingleTerminal, false, false, false, false),
        ] {
            assert_eq!(kind.has_tab_actions(), actions, "{kind:?}");
            assert_eq!(kind.saves_session(), saves, "{kind:?}");
            assert_eq!(kind.shows_tab_bar(1), bar1, "{kind:?}");
            assert_eq!(kind.shows_tab_bar(2), bar2, "{kind:?}");
        }
    }
}
