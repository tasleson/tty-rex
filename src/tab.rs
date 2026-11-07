// tab.rs
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

//! A single terminal tab: a VTE widget, its process and the banner shown
//! when the process exits (`src/ptyxis-tab.c`).
//!
//! Tabs locate their `adw::TabPage` through the widget hierarchy instead of
//! reporting to a particular window, because pages can be dragged between
//! windows; whichever window currently hosts the tab is the right one.
//!
//! A tab can be given a color, shown as a dot on the tab and painted on the
//! window's top bar while the tab is selected. This is a tty-rex addition
//! with no C equivalent; the color lasts as long as the tab.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use relm4::actions::RelmAction;
use relm4::prelude::*;
use vte::prelude::*;

use crate::color_dialog;
use crate::container::{Container, Containers};
use crate::dressing::{Dressing, DressingStyle};
use crate::inspector::{Inspector, InspectorInit};
use crate::leader::{self, LeaderKind};
use crate::links::{self, Links};
use crate::palette;
use crate::profile::Profile;
use crate::profile_dialog::{CHANGE_PROFILE_LABEL, ProfileDialog};
use crate::settings::{self, AppSettings, ExitAction};
use crate::spawn::{self, ChildExit, SpawnError, SpawnRequest, Spawner};
use crate::tab_notify::{self, TabNotify};
use crate::telemetry::Pid;
use crate::terminal::{self, ZoomLevel};
use crate::title_dialog::{SET_TITLE_LABEL, TitleDialog};
use crate::window::{
    READ_ONLY_ACTION, SetProfileAction, SetTitleAction, TOGGLE_TELEMETRY_LABEL,
    ToggleTelemetryAction, UnfullscreenAction, hide_when_disabled,
};

/// Exits faster than this without user input are treated as failures so the
/// error stays visible (see `ptyxis_tab_wait_cb()`).
const QUICK_EXIT_THRESHOLD: Duration = Duration::from_millis(500);

const DEFAULT_TITLE: &str = "Terminal";
/// Banner title after the process ends normally.
const PROCESS_EXITED_TITLE: &str = "Process Exited";
const RESTART_LABEL: &str = "_Restart";
const EDIT_PROFILE_LABEL: &str = "Edit Profile";
/// Application action opening the profile editor (`app.edit-profile`).
pub const EDIT_PROFILE_ACTION: &str = "app.edit-profile";

const COPY_AS_HTML_ACTION: &str = "clipboard.copy-as-html";
const COPY_LINK_ACTION: &str = "clipboard.copy-link";
const OPEN_LINK_ACTION: &str = "terminal.open-link";
const RESET_ACTION: &str = "tab.reset";
/// VTE's signal around showing the context menu (`VteTerminal::setup-context-menu`).
const SETUP_CONTEXT_MENU_SIGNAL: &str = "setup-context-menu";
/// The toast after a copy, and how many seconds it stays
/// (`copy_clipboard_action()`, `copy_link_address_action()`).
const COPIED_TOAST_TITLE: &str = "Copied to clipboard";
const COPIED_TOAST_TIMEOUT_SECS: u32 = 1;
/// The toast when no handler could open a link (`ptyxis_tab_open_uri_cb()`).
const OPEN_LINK_FAILED_TOAST_TITLE: &str = "Failed to open link";
const OPEN_LINK_FAILED_TOAST_TIMEOUT_SECS: u32 = 3;

/// Title of the color dialog and tooltip of the color dot.
const TAB_COLOR_TITLE: &str = "Tab Color";
/// The color dot on the tab: a circle of `COLOR_DOT_RADIUS` in a square
/// of `COLOR_DOT_SIZE`, outlined so it stays visible on a top bar painted
/// in the same color.
const COLOR_DOT_SIZE: u32 = 16;
const COLOR_DOT_RADIUS: f64 = 6.0;
const COLOR_DOT_OUTLINE_WIDTH: f64 = 1.0;

/// Poll delays of `src/ptyxis-tab-monitor.c`: soon after keys that likely
/// start or end a program, otherwise backing off while nothing changes.
const POLL_DELAY_INTERACTIVE: Duration = Duration::from_millis(100);
const POLL_DELAY_MIN: Duration = Duration::from_millis(500);
const POLL_DELAY_MAX: Duration = Duration::from_secs(10);

/// Why the session should hold off logging out, as Ptyxis tells it.
const INHIBIT_LOGOUT_REASON: &str = "A foreground process is running";

/// What the banner button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerAction {
    Restart,
    EditProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabState {
    Initial,
    Spawning,
    Running,
    Exited,
    Failed,
}

/// What to do after the child process terminates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitOutcome {
    ShowBanner(String),
    Restart,
    Close,
}

/// Decides how to react to a child exit, following `ptyxis_tab_wait_cb()`.
///
/// `keep_open` is the tab's `keep-terminal-open` request. `quick_exit` is
/// true when the process exited soon after spawning and no key was pressed.
pub fn exit_outcome(
    exit: ChildExit,
    profile_action: ExitAction,
    has_command: bool,
    keep_open: bool,
    quick_exit: bool,
) -> ExitOutcome {
    if let ChildExit::Signaled(signal) = exit {
        return ExitOutcome::ShowBanner(format!("Process Exited from Signal {signal}"));
    }

    // A tab started for an explicit command (ptyxis -x) closes when done.
    let mut action = if has_command {
        ExitAction::Close
    } else {
        profile_action
    };
    if keep_open {
        action = ExitAction::None;
    }
    if (!has_command || !exit.is_success()) && quick_exit {
        action = ExitAction::None;
    }

    match action {
        ExitAction::Restart => ExitOutcome::Restart,
        ExitAction::Close => ExitOutcome::Close,
        ExitAction::None => ExitOutcome::ShowBanner(PROCESS_EXITED_TITLE.to_owned()),
    }
}

/// An explicit title (`ptyxis --title`) replaces the dynamic one, as in
/// `should_ignore_osc_title()` in `src/ptyxis-application.c`.
pub fn should_ignore_osc_title(ignore_setting: bool, title_prefix: Option<&str>) -> bool {
    ignore_setting || title_prefix.is_some_and(|prefix| !prefix.is_empty())
}

/// An empty prefix means none, as in `ptyxis_tab_set_title_prefix()`.
pub fn normalize_title_prefix(prefix: Option<String>) -> Option<String> {
    prefix.filter(|prefix| !prefix.is_empty())
}

/// Builds the tab title like `ptyxis_tab_dup_title()`. `fallback` stands
/// in for a missing window title: the command, else the initial title.
pub fn compose_title(
    prefix: Option<&str>,
    ignore_osc_title: bool,
    window_title: Option<&str>,
    fallback: Option<&str>,
    state: TabState,
) -> String {
    let mut title = prefix.unwrap_or_default().to_owned();
    if !ignore_osc_title {
        match window_title.filter(|t| !t.is_empty()) {
            Some(window_title) => title.push_str(window_title),
            None => title.push_str(fallback.unwrap_or_default()),
        }
    }
    if title.is_empty() {
        title.push_str(DEFAULT_TITLE);
    }
    match state {
        TabState::Exited => title.push_str(" (Exited)"),
        TabState::Failed => title.push_str(" (Failed)"),
        TabState::Initial | TabState::Spawning | TabState::Running => {}
    }
    title
}

/// `#rrggbb` for the opaque part of `color`.
fn hex_color(color: &gdk::RGBA) -> String {
    // Clamped first, so the cast cannot saturate.
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.red()),
        channel(color.green()),
        channel(color.blue())
    )
}

/// An SVG of the dot marking a tab colored `color`, outlined in the text
/// color used on it.
pub fn color_dot_svg(color: &gdk::RGBA) -> String {
    let outline = palette::contrast_foreground(color);
    let center = f64::from(COLOR_DOT_SIZE) / 2.0;
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{size}\" height=\"{size}\" \
         viewBox=\"0 0 {size} {size}\"><circle cx=\"{center}\" cy=\"{center}\" \
         r=\"{COLOR_DOT_RADIUS}\" fill=\"{fill}\" stroke=\"{stroke}\" \
         stroke-opacity=\"{opacity}\" stroke-width=\"{COLOR_DOT_OUTLINE_WIDTH}\"/></svg>",
        size = COLOR_DOT_SIZE,
        fill = hex_color(color),
        stroke = hex_color(&outline),
        opacity = outline.alpha(),
    )
}

/// The dot as an icon. It is not symbolic, so the tab bar shows it in
/// color rather than recoloring it like its other icons.
fn color_dot_icon(color: &gdk::RGBA) -> gio::Icon {
    gio::BytesIcon::new(&glib::Bytes::from_owned(color_dot_svg(color).into_bytes())).upcast()
}

/// Capture-phase scroll controller that turns Ctrl+scroll into zoom
/// (`on_scroll_scrolled_cb()` and friends, set up in `ptyxis_tab_init()`).
///
/// Ctrl+scroll is swallowed even when `enable-zoom-scroll-ctrl` is off, so
/// the terminal never scrolls its scrollback with Ctrl held.
fn zoom_scroll_controller(
    app_settings: AppSettings,
    input: relm4::Sender<TabInput>,
) -> gtk::EventControllerScroll {
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroll.connect_scroll(move |scroll, _dx, dy| {
        if !scroll
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
        {
            return glib::Propagation::Proceed;
        }
        if app_settings.enable_zoom_scroll_ctrl() {
            input.emit(TabInput::ScrollZoom(dy));
        }
        glib::Propagation::Stop
    });
    // Smooth (touchpad) scrolls started with Ctrl held zoom one step per
    // discrete notch instead of per pixel (`on_scroll_begin_cb()`).
    scroll.connect_scroll_begin(|scroll| {
        if scroll
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
        {
            scroll.set_flags(
                gtk::EventControllerScrollFlags::VERTICAL
                    | gtk::EventControllerScrollFlags::DISCRETE,
            );
        }
    });
    // `on_scroll_end_cb()`.
    scroll.connect_scroll_end(|scroll| {
        scroll.set_flags(gtk::EventControllerScrollFlags::VERTICAL);
    });
    scroll
}

/// Whether a key press likely starts or ends a program, so the leader
/// should be checked soon (`ptyxis_tab_monitor_key_pressed_cb()`).
fn is_interactive_key(key: gdk::Key, state: gdk::ModifierType) -> bool {
    let control = state.contains(gdk::ModifierType::CONTROL_MASK);
    match key {
        gdk::Key::Return | gdk::Key::ISO_Enter | gdk::Key::KP_Enter => true,
        gdk::Key::c | gdk::Key::d => control,
        _ => false,
    }
}

/// Schedules checks of the foreground process like `PtyxisTabMonitor`:
/// output and interactive keys bring the next check closer, while checks
/// that find nothing new back off.
struct LeaderMonitor {
    delay: Cell<Duration>,
    /// The scheduled check and when it is due.
    pending: RefCell<Option<(glib::SourceId, Instant)>>,
    stopped: Cell<bool>,
    sender: relm4::Sender<TabInput>,
}

impl LeaderMonitor {
    fn new(sender: relm4::Sender<TabInput>) -> Rc<Self> {
        Rc::new(Self {
            delay: Cell::new(POLL_DELAY_MIN),
            pending: RefCell::default(),
            stopped: Cell::new(false),
            sender,
        })
    }

    /// Checks after `delay`, unless a check is already due by then.
    fn schedule(self: &Rc<Self>, delay: Duration) {
        if self.stopped.get() {
            return;
        }
        let due = Instant::now() + delay;
        let mut pending = self.pending.borrow_mut();
        if pending.as_ref().is_some_and(|(_, at)| *at <= due) {
            return;
        }
        if let Some((id, _)) = pending.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(delay, move || {
            if let Some(monitor) = weak.upgrade() {
                // The source is gone once it fires; forget it unremoved.
                monitor.pending.borrow_mut().take();
                monitor.sender.emit(TabInput::PollLeader);
            }
        });
        *pending = Some((id, due));
    }

    fn contents_changed(self: &Rc<Self>) {
        if self.delay.get() > POLL_DELAY_MIN {
            self.delay.set(POLL_DELAY_MIN);
        }
        self.schedule(self.delay.get());
    }

    fn interactive_key(self: &Rc<Self>) {
        self.delay.set(POLL_DELAY_INTERACTIVE);
        self.schedule(POLL_DELAY_INTERACTIVE);
    }

    /// Plans the next check after one finished.
    fn polled(self: &Rc<Self>, changed: bool) {
        let delay = if changed {
            POLL_DELAY_MIN
        } else {
            (self.delay.get() * 2).clamp(POLL_DELAY_MIN, POLL_DELAY_MAX)
        };
        self.delay.set(delay);
        self.schedule(delay);
    }

    fn stop(&self) {
        self.stopped.set(true);
        if let Some((id, _)) = self.pending.borrow_mut().take() {
            id.remove();
        }
    }
}

/// What the last check found in the foreground, as far as logout
/// inhibition cares (`has_foreground_process` and `program_name`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ForegroundState {
    /// A process other than the tab's own child leads the terminal.
    pub has_foreground: bool,
    /// That process's program, when its command line names one.
    pub program_name: Option<String>,
}

impl ForegroundState {
    pub fn new(foreground: Option<&leader::Foreground>) -> Self {
        Self {
            has_foreground: foreground.is_some(),
            program_name: foreground.and_then(|f| f.program_name().map(str::to_owned)),
        }
    }

    /// Whether to keep the session from logging out: only for a foreground
    /// program that is not a shell (`ptyxis_tab_update_inhibit()`).
    pub fn inhibits_logout(&self, is_shell: impl Fn(&str) -> bool) -> bool {
        self.has_foreground
            && self
                .program_name
                .as_deref()
                .is_some_and(|name| !is_shell(name))
    }
}

/// A logout inhibition held for a tab, lifted when dropped
/// (`inhibit_cookie` and `gtk_application_uninhibit()`).
struct LogoutInhibit {
    application: gtk::Application,
    cookie: u32,
}

impl LogoutInhibit {
    /// Asks the session to hold off logging out on behalf of `window`.
    /// GTK returns no cookie when the request failed.
    fn new(window: &gtk::Window) -> Option<Self> {
        let application = gio::Application::default().and_downcast::<gtk::Application>()?;
        let cookie = application.inhibit(
            Some(window),
            gtk::ApplicationInhibitFlags::LOGOUT,
            Some(INHIBIT_LOGOUT_REASON),
        );
        (cookie != 0).then_some(Self {
            application,
            cookie,
        })
    }
}

impl Drop for LogoutInhibit {
    fn drop(&mut self) {
        self.application.uninhibit(self.cookie);
    }
}

/// The terminal context menu: the items of `src/ptyxis-terminal.ui` whose
/// actions the port implements, plus tty-rex's telemetry toggle.
fn context_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("Search…"), Some("win.search"));
    // A check item, as the action's state is the footer's visibility.
    section.append_item(&RelmAction::<ToggleTelemetryAction>::to_menu_item(
        TOGGLE_TELEMETRY_LABEL,
    ));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    for (label, action) in [
        ("_Open Link", OPEN_LINK_ACTION),
        ("Copy _Link", COPY_LINK_ACTION),
    ] {
        let item = gio::MenuItem::new(Some(label), Some(action));
        hide_when_disabled(&item);
        section.append_item(&item);
    }
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("_Copy"), Some("win.copy-clipboard"));
    let copy_as_html = gio::MenuItem::new(Some("Copy as _HTML"), Some(COPY_AS_HTML_ACTION));
    hide_when_disabled(&copy_as_html);
    section.append_item(&copy_as_html);
    section.append(Some("_Paste"), Some("win.paste-clipboard"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    for (label, select) in [("Select _All", true), ("Select _None", false)] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some("terminal.select-all"), Some(&select.to_variant()));
        section.append_item(&item);
    }
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("Read-Only"), Some(READ_ONLY_ACTION));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    for (label, clear) in [("Reset", false), ("Reset and Clear", true)] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(RESET_ACTION), Some(&clear.to_variant()));
        section.append_item(&item);
    }
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<SetTitleAction>::to_menu_item(SET_TITLE_LABEL));
    let change_profile = RelmAction::<SetProfileAction>::to_menu_item(CHANGE_PROFILE_LABEL);
    hide_when_disabled(&change_profile);
    section.append_item(&change_profile);
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    let leave_fullscreen = RelmAction::<UnfullscreenAction>::to_menu_item("Leave Fullscreen");
    hide_when_disabled(&leave_fullscreen);
    section.append_item(&leave_fullscreen);
    section.append(Some("_Inspect Terminal"), Some("tab.inspect"));
    menu.append_section(None, &section);
    menu
}

/// The terminal's own actions: `terminal.select-all(b)` selects
/// everything, or nothing when false, and `clipboard.copy-as-html` copies
/// the selection as HTML, available only while there is one
/// (`ptyxis_terminal_update_clipboard_actions()`).
fn install_terminal_actions(terminal: &vte::Terminal, links: &Links, settings: &AppSettings) {
    let select_all = gio::SimpleAction::new("select-all", Some(glib::VariantTy::BOOLEAN));
    let weak = terminal.downgrade();
    select_all.connect_activate(move |_, select| {
        let Some(terminal) = weak.upgrade() else {
            return;
        };
        if select.and_then(|v| v.get::<bool>()).unwrap_or(true) {
            terminal.select_all();
        } else {
            terminal.unselect_all();
        }
    });
    let open_link = gio::SimpleAction::new("open-link", None);
    let copy_link = gio::SimpleAction::new("copy-link", None);
    open_link.set_enabled(false);
    copy_link.set_enabled(false);
    // The link under the pointer when the menu was last asked for
    // (`PtyxisTerminal.url`).
    let url: Rc<RefCell<Option<String>>> = Rc::default();
    let weak = terminal.downgrade();
    let target = Rc::clone(&url);
    // Both skip an empty link, which a custom link can substitute to
    // (`open_link_action()`, `copy_link_address_action()`).
    open_link.connect_activate(move |_, _| {
        if let (Some(terminal), Some(url)) = (weak.upgrade(), target.borrow().as_deref())
            && !url.is_empty()
        {
            open_uri(&terminal, url);
        }
    });
    let weak = terminal.downgrade();
    let target = Rc::clone(&url);
    copy_link.connect_activate(move |_, _| {
        if let (Some(terminal), Some(url)) = (weak.upgrade(), target.borrow().as_deref())
            && !url.is_empty()
        {
            terminal.clipboard().set_text(url);
            toast(&terminal, COPIED_TOAST_TIMEOUT_SECS, COPIED_TOAST_TITLE);
        }
    });
    // Before VTE shows the menu, enables the link actions only over a link,
    // and disables them again once it has gone, so a menu opened from the
    // keyboard never offers a stale link (`ptyxis_terminal_setup_context_menu()`).
    let links_at = links.clone();
    let (open, copy) = (open_link.clone(), copy_link.clone());
    let target = Rc::clone(&url);
    terminal.connect_local(SETUP_CONTEXT_MENU_SIGNAL, false, move |values| {
        let terminal = values.first()?.get::<vte::Terminal>().ok()?;
        let found = values
            .get(1)
            .and_then(links::event_context_coordinates)
            .and_then(|(x, y)| links_at.at(&terminal, x, y));
        open.set_enabled(found.is_some());
        copy.set_enabled(found.is_some());
        *target.borrow_mut() = found;
        None
    });
    // Control-click opens the link (`ptyxis_terminal_click_pressed_cb()`).
    let open_click = gtk::GestureClick::builder()
        .button(0)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let links_at = links.clone();
    open_click.connect_pressed(move |click, n_press, x, y| {
        let Some(terminal) = click.widget().and_downcast::<vte::Terminal>() else {
            return;
        };
        let button = click.current_button();
        let control = click
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK);
        if n_press == 1
            && (button == gdk::BUTTON_PRIMARY || button == gdk::BUTTON_MIDDLE)
            && control
            && let Some(url) = links_at.at(&terminal, x, y).filter(|url| !url.is_empty())
        {
            open_uri(&terminal, &url);
            click.set_state(gtk::EventSequenceState::Claimed);
        } else {
            click.set_state(gtk::EventSequenceState::Denied);
        }
    });
    terminal.add_controller(open_click);
    let actions = gio::SimpleActionGroup::new();
    actions.add_action(&select_all);
    actions.add_action(&open_link);
    terminal.insert_action_group("terminal", Some(&actions));

    let copy_as_html = gio::SimpleAction::new("copy-as-html", None);
    copy_as_html.set_enabled(terminal.has_selection());
    let weak = terminal.downgrade();
    let settings = settings.clone();
    copy_as_html.connect_activate(move |_, _| {
        if let Some(terminal) = weak.upgrade() {
            copy_selection(&terminal, vte::Format::Html, &settings);
        }
    });
    let action = copy_as_html.clone();
    terminal
        .connect_selection_changed(move |terminal| action.set_enabled(terminal.has_selection()));
    let actions = gio::SimpleActionGroup::new();
    actions.add_action(&copy_as_html);
    actions.add_action(&copy_link);
    terminal.insert_action_group("clipboard", Some(&actions));
}

/// Opens `uri` with the desktop's handler, saying so in a toast when that
/// fails (`ptyxis_tab_open_uri()`, `ptyxis_tab_open_uri_cb()`).
fn open_uri(terminal: &vte::Terminal, uri: &str) {
    let weak = terminal.downgrade();
    gtk::UriLauncher::new(uri).launch(
        terminal.root().and_downcast_ref::<gtk::Window>(),
        None::<&gio::Cancellable>,
        move |result| {
            if let Err(e) = result
                && !e.matches(gio::IOErrorEnum::Cancelled)
            {
                glib::g_warning!("tty-rex", "Failed to open link: {e}");
                if let Some(terminal) = weak.upgrade() {
                    toast(
                        &terminal,
                        OPEN_LINK_FAILED_TOAST_TIMEOUT_SECS,
                        OPEN_LINK_FAILED_TOAST_TITLE,
                    );
                }
            }
        },
    );
}

/// Puts the selection on the clipboard as plain text in `format`, which
/// for HTML is the markup itself, and says so in a toast when
/// `toast-on-copy-clipboard` asks for it (`copy_clipboard_action()`).
fn copy_selection(terminal: &vte::Terminal, format: vte::Format, settings: &AppSettings) {
    if let Some(text) = terminal
        .text_selected(format)
        .filter(|text| !text.is_empty())
    {
        terminal.clipboard().set_text(&text);
        if settings.toast_on_copy_clipboard() {
            toast(terminal, COPIED_TOAST_TIMEOUT_SECS, COPIED_TOAST_TITLE);
        }
    }
}

/// Shows `title` for `timeout` seconds in the window's toast overlay
/// around `terminal`, if it has one (`ptyxis_terminal_toast()`).
fn toast(terminal: &vte::Terminal, timeout: u32, title: &str) {
    let Some(overlay) = terminal
        .ancestor(adw::ToastOverlay::static_type())
        .and_downcast::<adw::ToastOverlay>()
    else {
        return;
    };
    let toast = adw::Toast::new(title);
    toast.set_timeout(timeout);
    overlay.add_toast(toast);
}

/// `tab.reset(b)`: resets the terminal, also clearing the scrollback when
/// true (`ptyxis_window_tab_reset_action()`). Ptyxis installs it on the
/// window for the active tab; it lives on the tab here so the tab's own
/// `tab` actions do not hide it from the context menu.
fn reset_action(terminal: &vte::Terminal) -> gio::SimpleAction {
    let reset = gio::SimpleAction::new("reset", Some(glib::VariantTy::BOOLEAN));
    let weak = terminal.downgrade();
    reset.connect_activate(move |_, clear| {
        if let Some(terminal) = weak.upgrade() {
            let clear = clear.and_then(|v| v.get::<bool>()).unwrap_or(false);
            terminal.reset(true, clear);
        }
    });
    reset
}

/// Finds the tab view and page currently hosting `child`.
pub fn find_page(child: &impl IsA<gtk::Widget>) -> Option<(adw::TabView, adw::TabPage)> {
    let tab_view = child
        .ancestor(adw::TabView::static_type())?
        .downcast::<adw::TabView>()
        .ok()?;
    let child = child.as_ref();
    let page = (0..tab_view.n_pages())
        .map(|i| tab_view.nth_page(i))
        .find(|page| page.child() == *child)?;
    Some((tab_view, page))
}

/// Redraws the tab overview's thumbnail after the colors changed
/// (`ptyxis_tab_invalidate_thumbnail()`).
fn invalidate_thumbnail(root: &gtk::Box) {
    if let Some((_, page)) = find_page(root) {
        page.invalidate_thumbnail();
    }
}

/// Everything needed to create a tab.
pub struct TabInit {
    /// Identifies the tab, e.g. for its notifications (`uuid`).
    pub uuid: String,
    pub profile: Profile,
    /// The uuid of the tab's profile, shared with the application for
    /// new tabs and the session; kept current by "Change Profile".
    pub profile_uuid: Rc<RefCell<String>>,
    pub app_settings: AppSettings,
    pub spawner: Rc<dyn Spawner>,
    pub containers: Containers,
    /// The container to run in instead of the profile's default one,
    /// shared with the application for saving the session.
    pub container: Rc<RefCell<Option<Container>>>,
    /// Id of the container a restored tab ran in. Containers may still be
    /// being discovered, so it is looked up when the tab first spawns.
    pub restored_container_id: Option<String>,
    /// Explicit command (`ptyxis -x`); the tab closes when it exits.
    pub command: Option<Vec<String>>,
    /// `KEY=VALUE` pairs for the child (`ptyxis_tab_set_environment()`).
    pub environment: Vec<String>,
    /// Shows the exit banner instead of closing or restarting when the
    /// child exits (`ptyxis_tab_set_keep_terminal_open()`).
    pub keep_terminal_open: bool,
    /// Directory requested for this tab, e.g. via `--working-directory`.
    pub initial_cwd_uri: Option<String>,
    /// Directory of the tab this one was opened from.
    pub previous_cwd_uri: Option<String>,
    pub title_prefix: Option<String>,
    /// Shows only the title prefix, whatever the program sets
    /// (`ptyxis_tab_set_ignore_osc_title()`).
    pub ignore_osc_title: bool,
    /// Title shown until the program sets one, e.g. the title the tab had
    /// when the session was saved (`ptyxis_tab_set_initial_title()`).
    pub initial_title: Option<String>,
    pub zoom: Rc<Cell<ZoomLevel>>,
    /// The tab's color, shared with windows for the tab menu.
    pub color: Rc<Cell<Option<gdk::RGBA>>>,
    /// The running child's pid, shared with windows for telemetry.
    pub child_pid: Rc<Cell<Option<Pid>>>,
    pub grid_size: Option<(u32, u32)>,
}

#[derive(Debug)]
pub enum TabInput {
    Respawn,
    BannerClicked,
    /// The container to spawn in was found, or the id that was not.
    SpawnIn(Result<Container, String>),
    Spawned(Result<glib::Pid, SpawnError>),
    Exited(ChildExit),
    /// Time to check which process leads the terminal.
    PollLeader,
    /// The shell is about to run a command (`shell-preexec`).
    ShellPreexec,
    /// Time to look up the command the shell just started.
    PollPreexec,
    /// The shell is about to show its prompt (`shell-precmd`).
    ShellPrecmd,
    /// Keyboard focus moved into the tab.
    FocusEntered,
    /// The `inhibit-logout` setting changed.
    InhibitLogoutChanged,
    UpdateTitle,
    AppSettingsChanged,
    /// A highlight color in tty-rex's own settings changed.
    HighlightColorsChanged,
    ProfileChanged,
    /// Opens "Change Profile" listing these profiles.
    ShowProfileDialog(Vec<Profile>),
    /// Switches the tab to another profile (`ptyxis_tab_apply_profile()`).
    ApplyProfile(Profile),
    ProfileDialogClosed(u32),
    AppearanceChanged,
    Bell,
    ZoomIn,
    ZoomOut,
    /// A Ctrl+scroll by this vertical delta.
    ScrollZoom(f64),
    ZoomReset,
    Copy,
    Paste,
    Focus,
    Inspect,
    InspectorClosed,
    ShowTitleDialog,
    SetTitlePrefix(String),
    SetIncludeProcessTitle(bool),
    /// Carries the serial of the dialog that closed.
    TitleDialogClosed(u32),
    ShowColorDialog,
    /// The color dialog with this serial closed, with the picked color
    /// unless it was dismissed or cancelled.
    ColorDialogClosed(u32, Option<gdk::RGBA>),
    ResetColor,
    /// The tab left its window, e.g. to move to another one.
    Unrealized,
    /// The tab's page was taken out of its tab view, to be closed (and
    /// parked) or moved to another window. Unlike [`TabInput::Unrealized`]
    /// this also covers a background tab that was never shown, so it
    /// stands in for `ptyxis_tab_unroot()`.
    Detached,
    /// The tab was reopened after being closed (`win.undo-close-tab`).
    Restored,
    /// End the program because the user confirmed closing the tab
    /// (`ptyxis_tab_force_quit()`).
    ForceQuit,
}

pub struct Tab {
    uuid: String,
    profile: Profile,
    app_settings: AppSettings,
    spawner: Rc<dyn Spawner>,
    containers: Containers,
    /// The container the tab was opened in, if not the profile's default
    /// (`container_at_creation`).
    container: Rc<RefCell<Option<Container>>>,
    restored_container_id: Option<String>,
    command: Option<Vec<String>>,
    environment: Vec<String>,
    keep_terminal_open: bool,
    initial_cwd_uri: Option<String>,
    previous_cwd_uri: Option<String>,
    title_prefix: Option<String>,
    initial_title: Option<String>,
    /// Per tab like the C `ignore-osc-title` property, so the title dialog
    /// can change it without touching the setting.
    ignore_osc_title: bool,
    /// The `ignore-osc-title` setting last applied to `ignore_osc_title`.
    ignore_osc_title_setting: bool,
    zoom: Rc<Cell<ZoomLevel>>,
    state: TabState,
    spawned_at: Instant,
    has_pressed_key: Rc<Cell<bool>>,
    terminal: vte::Terminal,
    banner: adw::Banner,
    banner_action: BannerAction,
    /// Handlers on objects shared with other tabs, disconnected on shutdown.
    shared_handlers: Vec<(glib::Object, glib::SignalHandlerId)>,
    inspector: Option<Controller<Inspector>>,
    title_dialog: Option<TitleDialog>,
    links: Links,
    profile_dialog: Option<ProfileDialog>,
    profile_dialog_serial: u32,
    profile_uuid: Rc<RefCell<String>>,
    /// The handler reporting changes of the current profile's settings.
    profile_handler: Option<glib::SignalHandlerId>,
    /// Identifies the current title dialog, so a late "closed" message from
    /// a dialog that was force-closed cannot clear its successor.
    title_dialog_serial: u32,
    /// The color given to the tab, and the dot showing it on the page.
    color: Rc<Cell<Option<gdk::RGBA>>>,
    color_icon: Option<gio::Icon>,
    /// Cancels the open color dialog.
    color_dialog: Option<gio::Cancellable>,
    /// The running child's pid; `None` while spawning or after it exited.
    child_pid: Rc<Cell<Option<Pid>>>,
    /// Set once the program was ended on purpose, whose exit then needs no
    /// banner or exit action (`forced_exit`).
    forced_exit: bool,
    /// Identifies the current color dialog, like `title_dialog_serial`.
    color_dialog_serial: u32,
    leader_kind: LeaderKind,
    /// Foreground process group seen by the last check.
    leader_pgrp: Option<i32>,
    leader_monitor: Rc<LeaderMonitor>,
    /// The foreground program seen by the last check.
    foreground: ForegroundState,
    /// Command line of the process leading the terminal at the last check,
    /// empty if it could not be read (`command_line`).
    command_line: String,
    /// The command the shell is running, for its notification.
    notify: TabNotify,
    /// Held while that program keeps the session from logging out.
    logout_inhibit: Option<LogoutInhibit>,
    dressing: Dressing,
}

impl Tab {
    pub fn terminal(&self) -> &vte::Terminal {
        &self.terminal
    }

    /// The CSS class a window carries while this tab is selected in it.
    pub fn css_class(&self) -> &str {
        self.dressing.css_class()
    }

    /// Restyles the window for the current palette, appearance, opacity,
    /// leader, `visual-process-leader` setting and highlight colors.
    fn update_dressing(&mut self) {
        let palette_id = self.profile.palette_id();
        let Some(palette) = palette::lookup_or_default(&palette_id) else {
            return;
        };
        self.restyle(&palette);
    }

    /// Applies the profile's palette to the terminal and restyles the
    /// window, resolving the palette once for both.
    fn update_palette_and_dressing(&mut self) {
        let palette_id = self.profile.palette_id();
        let Some(palette) = palette::lookup_or_default(&palette_id) else {
            glib::g_warning!("tty-rex", "No palette available for “{palette_id}”");
            return;
        };
        terminal::apply_palette(
            &self.terminal,
            &palette,
            adw::StyleManager::default().is_dark(),
        );
        self.restyle(&palette);
    }

    fn restyle(&mut self, palette: &palette::Palette) {
        let dark = adw::StyleManager::default().is_dark();
        self.dressing.update(&DressingStyle {
            face: palette.face(dark),
            dark,
            opacity: self.profile.opacity(),
            leader: self.leader_kind,
            visual_process_leader: self.app_settings.visual_process_leader(),
            highlight_colors: self.app_settings.highlight_colors(),
            use_system_accent: palette.use_system_accent(),
            tab_color: self.color.get(),
        });
    }

    /// Shows the color dot on the page.
    fn update_color_indicator(&self, root: &gtk::Box) {
        let Some((_, page)) = find_page(root) else {
            return;
        };
        let wanted = self.color_icon.as_ref();
        if page.indicator_icon().as_ref() != wanted {
            page.set_indicator_icon(wanted);
            page.set_indicator_tooltip(if wanted.is_some() {
                TAB_COLOR_TITLE
            } else {
                ""
            });
        }
    }

    fn set_color(&mut self, color: Option<gdk::RGBA>) {
        if self.color.get() == color {
            return;
        }
        self.color.set(color);
        self.color_icon = color.as_ref().map(color_dot_icon);
        self.update_dressing();
    }

    /// Opens the color dialog, unless it is open already.
    fn show_color_dialog(&mut self, root: &gtk::Box, sender: &ComponentSender<Self>) {
        if self.color_dialog.is_some() {
            return;
        }
        self.color_dialog_serial = self.color_dialog_serial.wrapping_add(1);
        let serial = self.color_dialog_serial;
        let cancellable = gio::Cancellable::new();
        let closed = sender.input_sender().clone();
        color_dialog::choose(
            &color_dialog::new(TAB_COLOR_TITLE),
            root.root().and_downcast_ref::<gtk::Window>(),
            self.color.get().as_ref(),
            Some(&cancellable),
            "tab color",
            // The tab may be gone by now, in which case this is dropped.
            move |color| closed.emit(TabInput::ColorDialogClosed(serial, color)),
        );
        self.color_dialog = Some(cancellable);
    }

    /// Closes the color dialog without changing the color.
    fn cancel_color_dialog(&mut self) {
        if let Some(cancellable) = self.color_dialog.take() {
            cancellable.cancel();
        }
    }

    /// Shows the leader's or the container's icon on the page, as
    /// `ptyxis_tab_dup_icon()` does.
    fn update_icon(&self, root: &gtk::Box) {
        let Some((_, page)) = find_page(root) else {
            return;
        };
        // Without a container reported by the shell, the one the tab was
        // started in decides.
        let runtime = terminal::container_runtime(&self.terminal).or_else(|| {
            self.container
                .borrow()
                .clone()
                .or_else(|| self.containers.lookup(&self.profile.default_container()))
                .filter(|container| !container.is_session())
                .map(|container| container.provider().to_owned())
        });
        let wanted = leader::tab_icon_name(self.leader_kind, runtime.as_deref());
        let current = page
            .icon()
            .and_downcast::<gio::ThemedIcon>()
            .and_then(|icon| icon.names().first().map(|name| name.to_string()));
        // Setting an equal icon would still redraw the tab bar.
        if current.as_deref() != wanted {
            page.set_icon(wanted.map(gio::ThemedIcon::new).as_ref());
        }
    }

    /// Records a new leader; returns whether it changed.
    fn set_leader_kind(&mut self, kind: LeaderKind, root: &gtk::Box) -> bool {
        if kind == self.leader_kind {
            return false;
        }
        self.leader_kind = kind;
        if let Some((tab_view, page)) = find_page(root) {
            // Like ptyxis_tab_poll_agent_cb(), flag background tabs.
            if tab_view.selected_page().as_ref() != Some(&page) {
                page.set_needs_attention(true);
            }
        }
        self.update_dressing();
        true
    }

    fn poll_leader(&mut self, root: &gtk::Box) {
        let running = self.state == TabState::Running;
        let leader = running
            .then(|| self.terminal.pty())
            .flatten()
            .and_then(|pty| leader::foreground_leader(pty.fd()));
        let (pgrp, kind) = leader.map_or((None, LeaderKind::Unknown), |(pgrp, kind)| {
            (Some(pgrp), kind)
        });
        // As in ptyxis_tab_poll_agent_cb(), a new foreground process counts
        // as a change even when its kind is the same, so checks stay quick.
        let new_process = std::mem::replace(&mut self.leader_pgrp, pgrp) != pgrp;
        let changed = self.set_leader_kind(kind, root) | new_process;
        if running {
            // Read once for both the tab's command line and the foreground.
            let mut cmdline = pgrp.and_then(leader::process_cmdline);
            let foreground = pgrp.and_then(|pgrp| {
                leader::foreground_with_cmdline(pgrp, self.child_pid.get(), &mut cmdline)
            });
            let state = ForegroundState::new(foreground.as_ref());
            let cmdline = foreground
                .map_or(cmdline, |f| f.cmdline)
                .unwrap_or_default();
            if self.command_line != cmdline {
                self.command_line = cmdline;
            }
            // Like ptyxis_tab_poll_agent_cb(), only a different program
            // revisits the inhibition.
            if state != self.foreground {
                self.foreground = state;
                self.update_logout_inhibit(root);
            }
            self.leader_monitor.polled(changed);
        } else {
            // Like ptyxis_tab_poll_agent_async() without a process; the
            // inhibition is left to the exit handling.
            self.foreground.has_foreground = false;
        }
    }

    /// Holds off logging out while a program other than a shell runs in
    /// the foreground, if the `inhibit-logout` setting allows it
    /// (`ptyxis_tab_update_inhibit()`).
    fn update_logout_inhibit(&mut self, root: &gtk::Box) {
        if !self.app_settings.inhibit_logout() {
            self.logout_inhibit = None;
            return;
        }
        let inhibit = self.foreground.inhibits_logout(spawn::is_shell);
        if inhibit == self.logout_inhibit.is_some() {
            return;
        }
        self.logout_inhibit = if inhibit {
            // GTK needs the window the request is made for.
            root.root()
                .and_downcast::<gtk::Window>()
                .and_then(|window| LogoutInhibit::new(&window))
        } else {
            None
        };
    }

    fn title(&self) -> String {
        compose_title(
            self.title_prefix.as_deref(),
            self.ignore_osc_title,
            terminal::window_title(&self.terminal).as_deref(),
            self.command
                .as_ref()
                .and_then(|c| c.first())
                .map(String::as_str)
                .or(self.initial_title.as_deref()),
            self.state,
        )
    }

    fn update_title(&self, root: &gtk::Box) {
        if let Some((_, page)) = find_page(root) {
            page.set_title(&self.title());
        }
    }

    /// Sets up the banner without showing it.
    fn prepare_banner(&mut self, title: &str, action: BannerAction) {
        self.banner.set_title(title);
        self.banner.set_button_label(Some(match action {
            BannerAction::Restart => RESTART_LABEL,
            BannerAction::EditProfile => EDIT_PROFILE_LABEL,
        }));
        self.banner_action = action;
    }

    fn show_banner(&mut self, title: &str, action: BannerAction) {
        self.prepare_banner(title, action);
        self.banner.set_visible(true);
    }

    /// Starts the process again, in the tab's container or else the
    /// profile's default one (`ptyxis_tab_respawn()`). Containers are
    /// still being discovered right after startup, so the lookup waits
    /// for that.
    fn respawn(&mut self, sender: &ComponentSender<Self>) {
        self.banner.set_visible(false);
        self.state = TabState::Spawning;
        if let Some(container) = self.container.borrow().clone() {
            sender.input(TabInput::SpawnIn(Ok(container)));
            return;
        }
        let restored = self.restored_container_id.take();
        let shared = Rc::clone(&self.container);
        let id = self.profile.default_container();
        let containers = self.containers.clone();
        let input = sender.input_sender().clone();
        self.containers.when_loaded(move || {
            // Like ptyxis_session_restore(), a restored container that is
            // gone leaves the profile's default one.
            if let Some(container) = restored.and_then(|id| containers.lookup(&id)) {
                *shared.borrow_mut() = Some(container.clone());
                let _ = input.send(TabInput::SpawnIn(Ok(container)));
                return;
            }
            // The tab may have closed meanwhile; then nobody is listening.
            let _ = input.send(TabInput::SpawnIn(containers.lookup(&id).ok_or(id)));
        });
    }

    fn spawn_in(&mut self, container: Container, sender: &ComponentSender<Self>) {
        self.has_pressed_key.set(false);

        let request = SpawnRequest {
            command: self.command.clone(),
            custom_command: self.profile.custom_command(),
            login_shell: self.profile.login_shell(),
            preserve_directory: self.profile.preserve_directory(),
            cwd_uri: self
                .initial_cwd_uri
                .clone()
                .or_else(|| self.previous_cwd_uri.clone()),
            profile_uuid: self.profile.uuid().to_owned(),
            use_proxy: self.profile.use_proxy(),
            container,
            environment: self.environment.clone(),
        };

        let spawned = sender.input_sender().clone();
        let exited = sender.input_sender().clone();
        self.spawner.spawn(
            &self.terminal,
            request,
            Box::new(move |result| spawned.emit(TabInput::Spawned(result))),
            Box::new(move |exit| exited.emit(TabInput::Exited(exit))),
        );
    }

    fn handle_exit(&mut self, exit: ChildExit, root: &gtk::Box, sender: &ComponentSender<Self>) {
        self.child_pid.set(None);
        // Like ptyxis_tab_wait_cb(), before anything else.
        self.update_logout_inhibit(root);
        self.state = if exit.is_success() {
            TabState::Exited
        } else {
            TabState::Failed
        };
        self.set_leader_kind(LeaderKind::Unknown, root);
        if self.forced_exit {
            return;
        }
        let quick_exit =
            self.spawned_at.elapsed() < QUICK_EXIT_THRESHOLD && !self.has_pressed_key.get();
        match exit_outcome(
            exit,
            self.profile.exit_action(),
            self.command.is_some(),
            self.keep_terminal_open,
            quick_exit,
        ) {
            ExitOutcome::Restart => self.respawn(sender),
            ExitOutcome::Close => {
                // Like ptyxis_tab_wait_cb(), prepare the banner anyway: the
                // tab can come back through `win.undo-close-tab`.
                self.prepare_banner(PROCESS_EXITED_TITLE, BannerAction::Restart);
                if let Some((tab_view, page)) = find_page(root) {
                    tab_view.set_page_pinned(&page, false);
                    tab_view.close_page(&page);
                }
            }
            ExitOutcome::ShowBanner(title) => self.show_banner(&title, BannerAction::Restart),
        }
    }

    fn set_zoom(&self, zoom: ZoomLevel) {
        self.zoom.set(zoom);
        self.terminal.set_font_scale(zoom.font_scale());
    }
}

impl Component for Tab {
    type Init = TabInit;
    type Input = TabInput;
    type Output = ();
    type CommandOutput = ();
    type Root = gtk::Box;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Box::new(gtk::Orientation::Vertical, 0)
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let terminal = terminal::create();
        let banner = adw::Banner::builder().revealed(true).visible(false).build();
        let scrolled_window = gtk::ScrolledWindow::builder()
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&terminal)
            .build();
        root.append(&banner);
        root.append(&scrolled_window);

        let input = sender.input_sender().clone();
        banner.connect_button_clicked(move |_| input.emit(TabInput::BannerClicked));

        let input = sender.input_sender().clone();
        terminal::connect_window_title_changed(&terminal, move |_| {
            input.emit(TabInput::UpdateTitle)
        });
        let input = sender.input_sender().clone();
        terminal.connect_bell(move |_| input.emit(TabInput::Bell));
        // The update after any message refreshes the icon.
        let input = sender.input_sender().clone();
        terminal::connect_container_changed(&terminal, move |_| input.emit(TabInput::UpdateTitle));
        let input = sender.input_sender().clone();
        terminal.connect_increase_font_size(move |_| input.emit(TabInput::ZoomIn));
        let input = sender.input_sender().clone();
        terminal.connect_decrease_font_size(move |_| input.emit(TabInput::ZoomOut));
        // Like ptyxis_tab_notify_init().
        let input = sender.input_sender().clone();
        terminal::connect_shell_preexec(&terminal, move |_| input.emit(TabInput::ShellPreexec));
        let input = sender.input_sender().clone();
        terminal::connect_shell_precmd(&terminal, move |_| input.emit(TabInput::ShellPrecmd));
        let has_pressed_key = Rc::new(Cell::new(false));
        let pressed = Rc::clone(&has_pressed_key);
        // Not `connect_commit()`: see `terminal::connect_commit_bytes()`.
        terminal::connect_commit_bytes(&terminal, move || pressed.set(true));

        let leader_monitor = LeaderMonitor::new(sender.input_sender().clone());
        let monitor = Rc::clone(&leader_monitor);
        terminal.connect_contents_changed(move |_| monitor.contents_changed());
        // Capture phase sees keys before the terminal consumes them.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let monitor = Rc::clone(&leader_monitor);
        keys.connect_key_pressed(move |_, key, _, state| {
            if is_interactive_key(key, state) {
                monitor.interactive_key();
            }
            glib::Propagation::Proceed
        });
        root.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let input = sender.input_sender().clone();
        focus.connect_contains_focus_notify(move |focus| {
            if focus.contains_focus() {
                input.emit(TabInput::FocusEntered);
            }
        });
        root.add_controller(focus);
        root.add_controller(zoom_scroll_controller(
            init.app_settings.clone(),
            sender.input_sender().clone(),
        ));

        terminal.set_context_menu_model(Some(&context_menu()));
        let links = Links::default();
        links.update(&terminal, &init.profile.custom_links());
        install_terminal_actions(&terminal, &links, &init.app_settings);
        let input = sender.input_sender().clone();
        let inspect = gio::SimpleAction::new("inspect", None);
        inspect.connect_activate(move |_, _| input.emit(TabInput::Inspect));
        let tab_actions = gio::SimpleActionGroup::new();
        tab_actions.add_action(&inspect);
        tab_actions.add_action(&reset_action(&terminal));
        root.insert_action_group("tab", Some(&tab_actions));

        // The page only exists once the window has inserted us.
        let input = sender.input_sender().clone();
        root.connect_parent_notify(move |_| input.emit(TabInput::UpdateTitle));
        // Moving to another window unrealizes the tab; libadwaita reparents
        // an internal wrapper, so the tab's own parent does not change.
        let input = sender.input_sender().clone();
        root.connect_unrealize(move |_| input.emit(TabInput::Unrealized));

        let input = sender.input_sender().clone();
        let profile_handler = init
            .profile
            .gsettings()
            .connect_changed(None, move |_, _| input.emit(TabInput::ProfileChanged));

        let mut shared_handlers = Vec::new();
        let input = sender.input_sender().clone();
        let id = init
            .app_settings
            .gsettings()
            .connect_changed(None, move |_, _| input.emit(TabInput::AppSettingsChanged));
        shared_handlers.push((init.app_settings.gsettings().clone().upcast(), id));
        let input = sender.input_sender().clone();
        let id = init
            .app_settings
            .gsettings()
            .connect_changed(Some(settings::key::INHIBIT_LOGOUT), move |_, _| {
                input.emit(TabInput::InhibitLogoutChanged)
            });
        shared_handlers.push((init.app_settings.gsettings().clone().upcast(), id));
        let input = sender.input_sender().clone();
        let tty_rex_settings = init.app_settings.tty_rex_gsettings();
        let id = tty_rex_settings.connect_changed(None, move |_, key| {
            if settings::tty_rex_key::COLOR_KEYS.contains(&key) {
                input.emit(TabInput::HighlightColorsChanged)
            }
        });
        shared_handlers.push((tty_rex_settings.clone().upcast(), id));
        if let Some(desktop) = init.app_settings.desktop_settings() {
            let input = sender.input_sender().clone();
            let id = desktop
                .connect_changed(Some(settings::DESKTOP_MONOSPACE_FONT_KEY), move |_, _| {
                    input.emit(TabInput::AppSettingsChanged)
                });
            shared_handlers.push((desktop.clone().upcast(), id));
        }
        let style_manager = adw::StyleManager::default();
        let input = sender.input_sender().clone();
        let id =
            style_manager.connect_dark_notify(move |_| input.emit(TabInput::AppearanceChanged));
        shared_handlers.push((style_manager.upcast(), id));

        terminal::apply_app_settings(&terminal, &init.app_settings);
        terminal::apply_profile(&terminal, &init.profile);
        terminal::apply_profile_palette(&terminal, &init.profile);
        terminal.set_font_scale(init.zoom.get().font_scale());
        if let Some((columns, rows)) = init.grid_size {
            terminal.set_size(columns.into(), rows.into());
        }

        let title_prefix = normalize_title_prefix(init.title_prefix);
        let ignore_osc_title_setting = init.app_settings.ignore_osc_title();
        let mut model = Tab {
            uuid: init.uuid,
            profile: init.profile,
            profile_uuid: init.profile_uuid,
            profile_handler: Some(profile_handler),
            links,
            profile_dialog: None,
            profile_dialog_serial: 0,
            app_settings: init.app_settings,
            spawner: init.spawner,
            containers: init.containers,
            container: init.container,
            restored_container_id: init.restored_container_id,
            command: init.command,
            environment: init.environment,
            keep_terminal_open: init.keep_terminal_open,
            initial_cwd_uri: init.initial_cwd_uri,
            previous_cwd_uri: init.previous_cwd_uri,
            ignore_osc_title: init.ignore_osc_title
                || should_ignore_osc_title(ignore_osc_title_setting, title_prefix.as_deref()),
            ignore_osc_title_setting,
            title_prefix,
            initial_title: init.initial_title,
            zoom: init.zoom,
            state: TabState::Initial,
            spawned_at: Instant::now(),
            has_pressed_key,
            terminal,
            banner,
            banner_action: BannerAction::Restart,
            inspector: None,
            title_dialog: None,
            title_dialog_serial: 0,
            color: init.color,
            child_pid: init.child_pid,
            forced_exit: false,
            color_icon: None,
            color_dialog: None,
            color_dialog_serial: 0,
            leader_kind: LeaderKind::Unknown,
            leader_pgrp: None,
            leader_monitor,
            foreground: ForegroundState::default(),
            command_line: String::new(),
            notify: TabNotify::default(),
            logout_inhibit: None,
            dressing: Dressing::new(),
            shared_handlers,
        };
        model.update_dressing();
        model.respawn(&sender);

        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            TabInput::BannerClicked => match self.banner_action {
                BannerAction::Restart => sender.input(TabInput::Respawn),
                // As in ptyxis_tab_spawn_cb(), a failed spawn offers to fix
                // the profile instead of retrying the same command.
                BannerAction::EditProfile => {
                    let uuid = self.profile.uuid().to_variant();
                    if let Err(e) = root.activate_action(EDIT_PROFILE_ACTION, Some(&uuid)) {
                        glib::g_warning!("tty-rex", "Cannot open the profile editor: {e}");
                    }
                }
            },
            TabInput::Respawn => {
                if matches!(self.state, TabState::Exited | TabState::Failed) {
                    self.respawn(&sender);
                }
            }
            TabInput::SpawnIn(Ok(container)) => self.spawn_in(container, &sender),
            TabInput::SpawnIn(Err(id)) => {
                self.state = TabState::Failed;
                self.show_banner(
                    &format!("Cannot locate container “{id}”"),
                    BannerAction::EditProfile,
                );
            }
            TabInput::Spawned(Ok(pid)) => {
                self.child_pid.set(Some(pid.0));
                self.state = TabState::Running;
                self.spawned_at = Instant::now();
            }
            TabInput::Spawned(Err(e)) => {
                self.state = TabState::Failed;
                self.terminal.feed(format!("{e}\r\n").as_bytes());
                self.show_banner("Failed to launch terminal", BannerAction::EditProfile);
            }
            TabInput::Exited(exit) => self.handle_exit(exit, root, &sender),
            TabInput::PollLeader => self.poll_leader(root),
            TabInput::ShellPreexec => {
                self.notify.preexec(Instant::now());
                // Like ptyxis_tab_notify_shell_preexec_cb(), look the
                // command up asynchronously; the prompt may come back first.
                sender.input(TabInput::PollPreexec);
            }
            TabInput::PollPreexec => {
                self.poll_leader(root);
                self.notify.polled(self.command_line.clone());
            }
            TabInput::ShellPrecmd => {
                if let Some(cmdline) = self.notify.precmd(Instant::now()) {
                    tab_notify::show(root, &self.uuid, &cmdline);
                }
            }
            TabInput::FocusEntered => {
                // Like ptyxis_tab_notify_contains_focus_cb().
                if let Some((_, page)) = find_page(root) {
                    page.set_needs_attention(false);
                }
                tab_notify::withdraw(&self.uuid);
            }
            TabInput::InhibitLogoutChanged => self.update_logout_inhibit(root),
            TabInput::UpdateTitle => {}
            TabInput::AppSettingsChanged => {
                terminal::apply_app_settings(&self.terminal, &self.app_settings);
                self.update_dressing();
                // Like the C property binding, only a change of the setting
                // itself overrides the tab's choice.
                let setting = self.app_settings.ignore_osc_title();
                if setting != self.ignore_osc_title_setting {
                    self.ignore_osc_title_setting = setting;
                    self.ignore_osc_title = setting;
                    if let Some(dialog) = &self.title_dialog {
                        dialog.set_include_process_title(!setting);
                    }
                }
            }
            TabInput::HighlightColorsChanged => self.update_dressing(),
            TabInput::ProfileChanged => {
                self.links
                    .update(&self.terminal, &self.profile.custom_links());
                terminal::apply_profile(&self.terminal, &self.profile);
                self.update_palette_and_dressing();
                invalidate_thumbnail(root);
            }
            TabInput::ShowProfileDialog(profiles) => {
                if self.profile_dialog.is_some() {
                    return;
                }
                self.profile_dialog_serial = self.profile_dialog_serial.wrapping_add(1);
                let serial = self.profile_dialog_serial;
                let selected = sender.input_sender().clone();
                let closed = sender.input_sender().clone();
                let dialog = ProfileDialog::new(
                    profiles,
                    self.profile.uuid(),
                    move |profile| selected.emit(TabInput::ApplyProfile(profile)),
                    move || closed.emit(TabInput::ProfileDialogClosed(serial)),
                );
                dialog.present(root);
                self.profile_dialog = Some(dialog);
            }
            TabInput::ProfileDialogClosed(serial) => {
                if serial == self.profile_dialog_serial {
                    self.profile_dialog = None;
                }
            }
            TabInput::ApplyProfile(profile) => {
                if profile.uuid() == self.profile.uuid() {
                    return;
                }
                if let Some(id) = self.profile_handler.take() {
                    self.profile.gsettings().disconnect(id);
                }
                let input = sender.input_sender().clone();
                self.profile_handler = Some(
                    profile
                        .gsettings()
                        .connect_changed(None, move |_, _| input.emit(TabInput::ProfileChanged)),
                );
                *self.profile_uuid.borrow_mut() = profile.uuid().to_owned();
                self.profile = profile;
                self.links
                    .update(&self.terminal, &self.profile.custom_links());
                terminal::apply_profile(&self.terminal, &self.profile);
                self.update_palette_and_dressing();
                invalidate_thumbnail(root);
            }
            TabInput::AppearanceChanged => {
                self.update_palette_and_dressing();
                invalidate_thumbnail(root);
            }
            TabInput::Bell => {
                if let Some((tab_view, page)) = find_page(root)
                    && tab_view.selected_page().as_ref() != Some(&page)
                {
                    page.set_needs_attention(true);
                }
            }
            TabInput::ZoomIn => self.set_zoom(self.zoom.get().zoom_in()),
            TabInput::ZoomOut => self.set_zoom(self.zoom.get().zoom_out()),
            TabInput::ScrollZoom(dy) => self.set_zoom(self.zoom.get().scrolled(dy)),
            TabInput::ZoomReset => self.set_zoom(ZoomLevel::DEFAULT),
            TabInput::Copy => copy_selection(&self.terminal, vte::Format::Text, &self.app_settings),
            // Ptyxis keeps clipboard.paste disabled unless the clipboard
            // holds text (`ptyxis_terminal_update_clipboard_actions()`).
            TabInput::Paste => {
                if terminal::can_paste(&self.terminal) {
                    terminal::paste(&self.terminal);
                }
            }
            TabInput::Inspect => {
                // One inspector per tab: a second request raises it.
                if let Some(inspector) = &self.inspector {
                    inspector.widget().present();
                    return;
                }
                let inspector = Inspector::builder()
                    .launch(InspectorInit {
                        terminal: self.terminal.clone(),
                        profile: self.profile.clone(),
                        zoom: Rc::clone(&self.zoom),
                    })
                    .detach();
                let window = inspector.widget();
                window.set_transient_for(root.root().and_downcast_ref::<gtk::Window>());
                let input = sender.input_sender().clone();
                window.connect_close_request(move |_| {
                    input.emit(TabInput::InspectorClosed);
                    glib::Propagation::Proceed
                });
                window.present();
                self.inspector = Some(inspector);
            }
            TabInput::InspectorClosed => self.inspector = None,
            TabInput::ShowTitleDialog => {
                // One dialog per tab: while it is open a further request
                // (e.g. its shortcut) does nothing.
                if self.title_dialog.is_some() {
                    return;
                }
                self.title_dialog_serial = self.title_dialog_serial.wrapping_add(1);
                let serial = self.title_dialog_serial;
                let prefix = sender.input_sender().clone();
                let include = sender.input_sender().clone();
                let closed = sender.input_sender().clone();
                let dialog = TitleDialog::new(
                    self.title_prefix.as_deref().unwrap_or_default(),
                    !self.ignore_osc_title,
                    move |text| prefix.emit(TabInput::SetTitlePrefix(text)),
                    move |active| include.emit(TabInput::SetIncludeProcessTitle(active)),
                    move || closed.emit(TabInput::TitleDialogClosed(serial)),
                );
                dialog.present(root);
                self.title_dialog = Some(dialog);
            }
            TabInput::SetTitlePrefix(prefix) => {
                self.title_prefix = normalize_title_prefix(Some(prefix));
            }
            TabInput::SetIncludeProcessTitle(include) => self.ignore_osc_title = !include,
            TabInput::TitleDialogClosed(serial) => {
                if serial == self.title_dialog_serial {
                    self.title_dialog = None;
                }
            }
            TabInput::ShowColorDialog => self.show_color_dialog(root, &sender),
            TabInput::ColorDialogClosed(serial, color) => {
                if serial == self.color_dialog_serial {
                    self.color_dialog = None;
                    if color.is_some() {
                        self.set_color(color);
                    }
                }
            }
            TabInput::ResetColor => self.set_color(None),
            TabInput::Restored => {
                // A tab closed because its process ended shows why when it
                // comes back (ptyxis_window_undo_close_tab_action()).
                if matches!(self.state, TabState::Exited | TabState::Failed) {
                    self.banner.set_visible(true);
                }
            }
            TabInput::ForceQuit => {
                self.forced_exit = true;
                crate::app::hang_up(&self.child_pid, ());
            }
            TabInput::Unrealized => {
                // A tab moved to another window (e.g. by the detach-tab
                // shortcut, which the dialog does not block) would leave its
                // dialog on the old window, which may never report "closed"
                // once destroyed. The color dialog goes for the same reason.
                if let Some(dialog) = self.title_dialog.take() {
                    dialog.force_close();
                }
                self.cancel_color_dialog();
            }
            TabInput::Detached => {
                // Like ptyxis_tab_unroot(), the inhibition was made for the
                // old window. It is not taken again until the foreground
                // program changes.
                self.logout_inhibit = None;
            }
            TabInput::Focus => {
                self.terminal.grab_focus();
            }
        }
        // Title and icons depend on state and settings, so refresh after
        // any change.
        self.update_title(root);
        self.update_icon(root);
        self.update_color_indicator(root);
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.leader_monitor.stop();
        // As in ptyxis_tab_dispose().
        self.logout_inhibit = None;
        // The inspector describes this terminal, so it goes with the tab.
        if let Some(inspector) = self.inspector.take() {
            inspector.widget().destroy();
        }
        if let Some(dialog) = self.title_dialog.take() {
            dialog.force_close();
        }
        if let Some(dialog) = self.profile_dialog.take() {
            dialog.force_close();
        }
        if let Some(id) = self.profile_handler.take() {
            self.profile.gsettings().disconnect(id);
        }
        self.cancel_color_dialog();
        for (object, id) in self.shared_handlers.drain(..) {
            object.disconnect(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logout_inhibition() {
        let is_shell = |name: &str| matches!(name, "bash" | "/bin/zsh");
        let cases = [
            // The tab's own child leads the terminal.
            (None, false, None, false),
            (Some("vim file.txt "), true, Some("vim"), true),
            (Some("bash "), true, Some("bash"), false),
            (Some("/bin/zsh -l "), true, Some("/bin/zsh"), false),
            // Unreadable or cut short: no program to judge.
            (Some("truncated"), true, None, false),
        ];
        for (cmdline, has_foreground, program_name, inhibits) in cases {
            let foreground = cmdline.map(|cmdline| leader::Foreground {
                pid: 42,
                cmdline: Some(cmdline.to_owned()),
            });
            let state = ForegroundState::new(foreground.as_ref());
            assert_eq!(state.has_foreground, has_foreground, "{cmdline:?}");
            assert_eq!(state.program_name.as_deref(), program_name, "{cmdline:?}");
            assert_eq!(state.inhibits_logout(is_shell), inhibits, "{cmdline:?}");
        }
        // A name left over from an earlier check does not count once the
        // foreground is gone (a poll after the process ended).
        let stale = ForegroundState {
            has_foreground: false,
            program_name: Some("vim".to_owned()),
        };
        assert!(!stale.inhibits_logout(is_shell));
    }

    #[test]
    fn exit_outcomes() {
        use ChildExit::{Exited, Signaled};
        use ExitAction as A;
        let banner = |s: &str| ExitOutcome::ShowBanner(s.to_owned());
        let exited = banner("Process Exited");
        let cases = [
            // (exit, profile action, has command, keep open, quick exit,
            // expected)
            (Exited(0), A::Close, false, false, false, ExitOutcome::Close),
            (
                Exited(0),
                A::Restart,
                false,
                false,
                false,
                ExitOutcome::Restart,
            ),
            (Exited(0), A::None, false, false, false, exited.clone()),
            (Exited(1), A::Close, false, false, false, ExitOutcome::Close),
            (Exited(0), A::Close, false, false, true, exited.clone()),
            (Exited(0), A::Restart, false, false, true, exited.clone()),
            (Exited(0), A::None, true, false, false, ExitOutcome::Close),
            (Exited(0), A::None, true, false, true, ExitOutcome::Close),
            (Exited(2), A::Close, true, false, true, exited.clone()),
            (Exited(0), A::Restart, false, true, false, exited.clone()),
            (Exited(0), A::None, true, true, false, exited.clone()),
            (
                Signaled(9),
                A::Close,
                false,
                false,
                false,
                banner("Process Exited from Signal 9"),
            ),
        ];
        for (exit, action, has_command, keep_open, quick, expected) in cases {
            assert_eq!(
                exit_outcome(exit, action, has_command, keep_open, quick),
                expected,
                "{exit:?} {action:?} command={has_command} keep={keep_open} quick={quick}"
            );
        }
    }

    #[test]
    fn explicit_title_ignores_osc() {
        let cases = [
            (false, None, false),
            (false, Some(""), false),
            (false, Some("Logs"), true),
            (true, None, true),
        ];
        for (setting, prefix, expected) in cases {
            assert_eq!(should_ignore_osc_title(setting, prefix), expected);
        }
    }

    #[test]
    fn title_prefixes() {
        let cases = [
            (None, None),
            (Some(""), None),
            (Some("Logs: "), Some("Logs: ")),
        ];
        for (prefix, expected) in cases {
            assert_eq!(
                normalize_title_prefix(prefix.map(str::to_owned)).as_deref(),
                expected,
                "{prefix:?}"
            );
        }
    }

    #[test]
    fn color_dots() {
        let cases = [
            // (color, fill, outline, outline opacity)
            (gdk::RGBA::RED, "#ff0000", "#000006", "0.8"),
            (gdk::RGBA::BLACK, "#000000", "#ffffff", "1"),
            (gdk::RGBA::WHITE, "#ffffff", "#000006", "0.8"),
            (
                gdk::RGBA::new(0.2, 0.4, 0.6, 1.0),
                "#336699",
                "#ffffff",
                "1",
            ),
            // Out-of-range channels are clamped.
            (
                gdk::RGBA::new(1.5, -0.5, 0.5, 1.0),
                "#ff0080",
                "#000006",
                "0.8",
            ),
        ];
        for (color, fill, outline, opacity) in cases {
            let svg = color_dot_svg(&color);
            assert!(
                svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""),
                "{svg}"
            );
            assert!(svg.ends_with("/></svg>"), "{svg}");
            for expected in [
                "width=\"16\" height=\"16\" viewBox=\"0 0 16 16\"".to_owned(),
                "cx=\"8\" cy=\"8\" r=\"6\"".to_owned(),
                format!("fill=\"{fill}\""),
                format!("stroke=\"{outline}\""),
                format!("stroke-opacity=\"{opacity}\""),
            ] {
                assert!(
                    svg.contains(&expected),
                    "{color}: missing {expected} in {svg}"
                );
            }
        }
    }

    #[test]
    fn interactive_keys() {
        use gdk::{Key, ModifierType as M};
        let cases = [
            (Key::Return, M::empty(), true),
            (Key::KP_Enter, M::SHIFT_MASK, true),
            (Key::c, M::CONTROL_MASK, true),
            (Key::d, M::CONTROL_MASK | M::SHIFT_MASK, true),
            (Key::c, M::empty(), false),
            (Key::x, M::CONTROL_MASK, false),
        ];
        for (key, state, expected) in cases {
            assert_eq!(
                is_interactive_key(key, state),
                expected,
                "{key:?} {state:?}"
            );
        }
    }

    #[test]
    fn titles() {
        use TabState::*;
        let cases = [
            (None, false, Some("vim"), None, Running, "vim"),
            (
                Some("Build: "),
                false,
                Some("make"),
                None,
                Running,
                "Build: make",
            ),
            (None, false, None, Some("htop"), Running, "htop"),
            (None, false, Some(""), Some("htop"), Running, "htop"),
            (None, false, None, None, Running, "Terminal"),
            (None, true, Some("vim"), None, Running, "Terminal"),
            (Some("Logs"), true, Some("vim"), None, Running, "Logs"),
            (None, false, Some("bash"), None, Exited, "bash (Exited)"),
            (None, false, None, None, Failed, "Terminal (Failed)"),
        ];
        for (prefix, ignore, window_title, command0, state, expected) in cases {
            assert_eq!(
                compose_title(prefix, ignore, window_title, command0, state),
                expected
            );
        }
    }
}
