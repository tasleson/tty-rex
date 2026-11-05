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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use relm4::actions::RelmAction;
use relm4::prelude::*;
use vte::prelude::*;

use crate::dressing::{Dressing, DressingStyle};
use crate::inspector::{Inspector, InspectorInit};
use crate::leader::{self, LeaderKind};
use crate::palette;
use crate::profile::Profile;
use crate::settings::{self, AppSettings, ExitAction};
use crate::spawn::{ChildExit, SpawnError, SpawnRequest, Spawner};
use crate::terminal::{self, ZoomLevel};
use crate::title_dialog::{SET_TITLE_LABEL, TitleDialog};
use crate::window::SetTitleAction;

/// Exits faster than this without user input are treated as failures so the
/// error stays visible (see `ptyxis_tab_wait_cb()`).
const QUICK_EXIT_THRESHOLD: Duration = Duration::from_millis(500);

const DEFAULT_TITLE: &str = "Terminal";
const RESTART_LABEL: &str = "_Restart";
const EDIT_PROFILE_LABEL: &str = "Edit Profile";
/// Application action opening the profile editor (`app.edit-profile`).
pub const EDIT_PROFILE_ACTION: &str = "app.edit-profile";

/// Poll delays of `src/ptyxis-tab-monitor.c`: soon after keys that likely
/// start or end a program, otherwise backing off while nothing changes.
const POLL_DELAY_INTERACTIVE: Duration = Duration::from_millis(100);
const POLL_DELAY_MIN: Duration = Duration::from_millis(500);
const POLL_DELAY_MAX: Duration = Duration::from_secs(10);

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
/// `quick_exit` is true when the process exited soon after spawning and no
/// key was pressed.
pub fn exit_outcome(
    exit: ChildExit,
    profile_action: ExitAction,
    has_command: bool,
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
    if (!has_command || !exit.is_success()) && quick_exit {
        action = ExitAction::None;
    }

    match action {
        ExitAction::Restart => ExitOutcome::Restart,
        ExitAction::Close => ExitOutcome::Close,
        ExitAction::None => ExitOutcome::ShowBanner("Process Exited".to_owned()),
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

/// Builds the tab title like `ptyxis_tab_dup_title()`.
pub fn compose_title(
    prefix: Option<&str>,
    ignore_osc_title: bool,
    window_title: Option<&str>,
    command0: Option<&str>,
    state: TabState,
) -> String {
    let mut title = prefix.unwrap_or_default().to_owned();
    if !ignore_osc_title {
        match window_title.filter(|t| !t.is_empty()) {
            Some(window_title) => title.push_str(window_title),
            None => title.push_str(command0.unwrap_or_default()),
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

/// The terminal context menu: the items of `src/ptyxis-terminal.ui` whose
/// actions the port implements.
fn context_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("Search…"), Some("win.search"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("_Copy"), Some("win.copy-clipboard"));
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
    section.append_item(&RelmAction::<SetTitleAction>::to_menu_item(SET_TITLE_LABEL));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("_Inspect Terminal"), Some("tab.inspect"));
    menu.append_section(None, &section);
    menu
}

/// `terminal.select-all(b)`: select everything, or nothing when false.
fn install_terminal_actions(terminal: &vte::Terminal) {
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
    let actions = gio::SimpleActionGroup::new();
    actions.add_action(&select_all);
    terminal.insert_action_group("terminal", Some(&actions));
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
    pub profile: Profile,
    pub app_settings: AppSettings,
    pub spawner: Rc<dyn Spawner>,
    /// Explicit command (`ptyxis -x`); the tab closes when it exits.
    pub command: Option<Vec<String>>,
    /// Directory requested for this tab, e.g. via `--working-directory`.
    pub initial_cwd_uri: Option<String>,
    /// Directory of the tab this one was opened from.
    pub previous_cwd_uri: Option<String>,
    pub title_prefix: Option<String>,
    pub zoom: Rc<Cell<ZoomLevel>>,
    pub grid_size: Option<(u32, u32)>,
}

#[derive(Debug)]
pub enum TabInput {
    Respawn,
    BannerClicked,
    Spawned(Result<glib::Pid, SpawnError>),
    Exited(ChildExit),
    /// Time to check which process leads the terminal.
    PollLeader,
    UpdateTitle,
    AppSettingsChanged,
    ProfileChanged,
    AppearanceChanged,
    Bell,
    ZoomIn,
    ZoomOut,
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
    /// The tab left its window, e.g. to move to another one.
    Unrealized,
}

pub struct Tab {
    profile: Profile,
    app_settings: AppSettings,
    spawner: Rc<dyn Spawner>,
    command: Option<Vec<String>>,
    initial_cwd_uri: Option<String>,
    previous_cwd_uri: Option<String>,
    title_prefix: Option<String>,
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
    /// Identifies the current title dialog, so a late "closed" message from
    /// a dialog that was force-closed cannot clear its successor.
    title_dialog_serial: u32,
    leader_kind: LeaderKind,
    /// Foreground process group seen by the last check.
    leader_pgrp: Option<i32>,
    leader_monitor: Rc<LeaderMonitor>,
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
    /// leader and `visual-process-leader` setting.
    fn update_dressing(&mut self) {
        let palette_id = self.profile.palette_id();
        let Some(palette) = palette::lookup_or_default(&palette_id) else {
            return;
        };
        let dark = adw::StyleManager::default().is_dark();
        self.dressing.update(&DressingStyle {
            face: palette.face(dark),
            dark,
            opacity: self.profile.opacity(),
            leader: self.leader_kind,
            visual_process_leader: self.app_settings.visual_process_leader(),
            use_system_accent: palette.use_system_accent(),
        });
    }

    /// Shows the leader's or the container's icon on the page, as
    /// `ptyxis_tab_dup_icon()` does.
    fn update_icon(&self, root: &gtk::Box) {
        let Some((_, page)) = find_page(root) else {
            return;
        };
        let runtime = terminal::container_runtime(&self.terminal);
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
        // Without a process there is nothing to watch until output resumes.
        if running {
            self.leader_monitor.polled(changed);
        }
    }

    fn title(&self) -> String {
        compose_title(
            self.title_prefix.as_deref(),
            self.ignore_osc_title,
            terminal::window_title(&self.terminal).as_deref(),
            self.command
                .as_ref()
                .and_then(|c| c.first())
                .map(String::as_str),
            self.state,
        )
    }

    fn update_title(&self, root: &gtk::Box) {
        if let Some((_, page)) = find_page(root) {
            page.set_title(&self.title());
        }
    }

    fn show_banner(&mut self, title: &str, action: BannerAction) {
        self.banner.set_title(title);
        self.banner.set_button_label(Some(match action {
            BannerAction::Restart => RESTART_LABEL,
            BannerAction::EditProfile => EDIT_PROFILE_LABEL,
        }));
        self.banner_action = action;
        self.banner.set_visible(true);
    }

    fn respawn(&mut self, sender: &ComponentSender<Self>) {
        self.banner.set_visible(false);
        self.state = TabState::Spawning;
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
        };

        let spawned = sender.input_sender().clone();
        let exited = sender.input_sender().clone();
        self.spawner.spawn(
            &self.terminal,
            &request,
            Box::new(move |result| spawned.emit(TabInput::Spawned(result))),
            Box::new(move |exit| exited.emit(TabInput::Exited(exit))),
        );
    }

    fn handle_exit(&mut self, exit: ChildExit, root: &gtk::Box, sender: &ComponentSender<Self>) {
        self.state = if exit.is_success() {
            TabState::Exited
        } else {
            TabState::Failed
        };
        self.set_leader_kind(LeaderKind::Unknown, root);
        let quick_exit =
            self.spawned_at.elapsed() < QUICK_EXIT_THRESHOLD && !self.has_pressed_key.get();
        match exit_outcome(
            exit,
            self.profile.exit_action(),
            self.command.is_some(),
            quick_exit,
        ) {
            ExitOutcome::Restart => self.respawn(sender),
            ExitOutcome::Close => {
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
        let has_pressed_key = Rc::new(Cell::new(false));
        let pressed = Rc::clone(&has_pressed_key);
        terminal.connect_commit(move |_, _, _| pressed.set(true));

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

        terminal.set_context_menu_model(Some(&context_menu()));
        install_terminal_actions(&terminal);
        let input = sender.input_sender().clone();
        let inspect = gio::SimpleAction::new("inspect", None);
        inspect.connect_activate(move |_, _| input.emit(TabInput::Inspect));
        let tab_actions = gio::SimpleActionGroup::new();
        tab_actions.add_action(&inspect);
        root.insert_action_group("tab", Some(&tab_actions));

        // The page only exists once the window has inserted us.
        let input = sender.input_sender().clone();
        root.connect_parent_notify(move |_| input.emit(TabInput::UpdateTitle));
        // Moving to another window unrealizes the tab; libadwaita reparents
        // an internal wrapper, so the tab's own parent does not change.
        let input = sender.input_sender().clone();
        root.connect_unrealize(move |_| input.emit(TabInput::Unrealized));

        let input = sender.input_sender().clone();
        init.profile
            .gsettings()
            .connect_changed(None, move |_, _| input.emit(TabInput::ProfileChanged));

        let mut shared_handlers = Vec::new();
        let input = sender.input_sender().clone();
        let id = init
            .app_settings
            .gsettings()
            .connect_changed(None, move |_, _| input.emit(TabInput::AppSettingsChanged));
        shared_handlers.push((init.app_settings.gsettings().clone().upcast(), id));
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
            profile: init.profile,
            app_settings: init.app_settings,
            spawner: init.spawner,
            command: init.command,
            initial_cwd_uri: init.initial_cwd_uri,
            previous_cwd_uri: init.previous_cwd_uri,
            ignore_osc_title: should_ignore_osc_title(
                ignore_osc_title_setting,
                title_prefix.as_deref(),
            ),
            ignore_osc_title_setting,
            title_prefix,
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
            leader_kind: LeaderKind::Unknown,
            leader_pgrp: None,
            leader_monitor,
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
                        glib::g_warning!("Ptyxis", "Cannot open the profile editor: {e}");
                    }
                }
            },
            TabInput::Respawn => {
                if matches!(self.state, TabState::Exited | TabState::Failed) {
                    self.respawn(&sender);
                }
            }
            TabInput::Spawned(Ok(_pid)) => {
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
            TabInput::ProfileChanged => {
                terminal::apply_profile(&self.terminal, &self.profile);
                terminal::apply_profile_palette(&self.terminal, &self.profile);
                self.update_dressing();
                invalidate_thumbnail(root);
            }
            TabInput::AppearanceChanged => {
                terminal::apply_profile_palette(&self.terminal, &self.profile);
                self.update_dressing();
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
            TabInput::ZoomReset => self.set_zoom(ZoomLevel::DEFAULT),
            TabInput::Copy => self.terminal.copy_clipboard_format(vte::Format::Text),
            TabInput::Paste => self.terminal.paste_clipboard(),
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
            TabInput::Unrealized => {
                // A tab moved to another window (e.g. by the detach-tab
                // shortcut, which the dialog does not block) would leave its
                // dialog on the old window, which may never report "closed"
                // once destroyed.
                if let Some(dialog) = self.title_dialog.take() {
                    dialog.force_close();
                }
            }
            TabInput::Focus => {
                self.terminal.grab_focus();
            }
        }
        // Title and icon depend on state and settings, so refresh after
        // any change.
        self.update_title(root);
        self.update_icon(root);
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.leader_monitor.stop();
        // The inspector describes this terminal, so it goes with the tab.
        if let Some(inspector) = self.inspector.take() {
            inspector.widget().destroy();
        }
        if let Some(dialog) = self.title_dialog.take() {
            dialog.force_close();
        }
        for (object, id) in self.shared_handlers.drain(..) {
            object.disconnect(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_outcomes() {
        use ChildExit::{Exited, Signaled};
        use ExitAction as A;
        let banner = |s: &str| ExitOutcome::ShowBanner(s.to_owned());
        let exited = banner("Process Exited");
        let cases = [
            // (exit, profile action, has command, quick exit, expected)
            (Exited(0), A::Close, false, false, ExitOutcome::Close),
            (Exited(0), A::Restart, false, false, ExitOutcome::Restart),
            (Exited(0), A::None, false, false, exited.clone()),
            (Exited(1), A::Close, false, false, ExitOutcome::Close),
            (Exited(0), A::Close, false, true, exited.clone()),
            (Exited(0), A::Restart, false, true, exited.clone()),
            (Exited(0), A::None, true, false, ExitOutcome::Close),
            (Exited(0), A::None, true, true, ExitOutcome::Close),
            (Exited(2), A::Close, true, true, exited.clone()),
            (
                Signaled(9),
                A::Close,
                false,
                false,
                banner("Process Exited from Signal 9"),
            ),
        ];
        for (exit, action, has_command, quick, expected) in cases {
            assert_eq!(
                exit_outcome(exit, action, has_command, quick),
                expected,
                "{exit:?} {action:?} command={has_command} quick={quick}"
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
