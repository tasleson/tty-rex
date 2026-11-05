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

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;
use vte::prelude::*;

use crate::profile::Profile;
use crate::settings::{self, AppSettings, ExitAction};
use crate::spawn::{ChildExit, SpawnError, SpawnRequest, Spawner};
use crate::terminal::{self, ZoomLevel};

/// Exits faster than this without user input are treated as failures so the
/// error stays visible (see `ptyxis_tab_wait_cb()`).
const QUICK_EXIT_THRESHOLD: Duration = Duration::from_millis(500);

const DEFAULT_TITLE: &str = "Terminal";
const RESTART_LABEL: &str = "_Restart";
const EDIT_PROFILE_LABEL: &str = "Edit Profile";
/// Application action opening the profile editor (`app.edit-profile`).
pub const EDIT_PROFILE_ACTION: &str = "app.edit-profile";

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
}

pub struct Tab {
    profile: Profile,
    app_settings: AppSettings,
    spawner: Rc<dyn Spawner>,
    command: Option<Vec<String>>,
    initial_cwd_uri: Option<String>,
    previous_cwd_uri: Option<String>,
    title_prefix: Option<String>,
    zoom: Rc<Cell<ZoomLevel>>,
    state: TabState,
    spawned_at: Instant,
    has_pressed_key: Rc<Cell<bool>>,
    terminal: vte::Terminal,
    banner: adw::Banner,
    banner_action: BannerAction,
    /// Handlers on objects shared with other tabs, disconnected on shutdown.
    shared_handlers: Vec<(glib::Object, glib::SignalHandlerId)>,
}

impl Tab {
    pub fn terminal(&self) -> &vte::Terminal {
        &self.terminal
    }

    fn title(&self) -> String {
        compose_title(
            self.title_prefix.as_deref(),
            should_ignore_osc_title(
                self.app_settings.ignore_osc_title(),
                self.title_prefix.as_deref(),
            ),
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
        let input = sender.input_sender().clone();
        terminal.connect_increase_font_size(move |_| input.emit(TabInput::ZoomIn));
        let input = sender.input_sender().clone();
        terminal.connect_decrease_font_size(move |_| input.emit(TabInput::ZoomOut));
        let has_pressed_key = Rc::new(Cell::new(false));
        let pressed = Rc::clone(&has_pressed_key);
        terminal.connect_commit(move |_, _, _| pressed.set(true));

        // The page only exists once the window has inserted us.
        let input = sender.input_sender().clone();
        root.connect_parent_notify(move |_| input.emit(TabInput::UpdateTitle));

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

        let mut model = Tab {
            profile: init.profile,
            app_settings: init.app_settings,
            spawner: init.spawner,
            command: init.command,
            initial_cwd_uri: init.initial_cwd_uri,
            previous_cwd_uri: init.previous_cwd_uri,
            title_prefix: init.title_prefix.filter(|p| !p.is_empty()),
            zoom: init.zoom,
            state: TabState::Initial,
            spawned_at: Instant::now(),
            has_pressed_key,
            terminal,
            banner,
            banner_action: BannerAction::Restart,
            shared_handlers,
        };
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
            TabInput::UpdateTitle => {}
            TabInput::AppSettingsChanged => {
                terminal::apply_app_settings(&self.terminal, &self.app_settings);
            }
            TabInput::ProfileChanged => {
                terminal::apply_profile(&self.terminal, &self.profile);
                terminal::apply_profile_palette(&self.terminal, &self.profile);
            }
            TabInput::AppearanceChanged => {
                terminal::apply_profile_palette(&self.terminal, &self.profile);
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
            TabInput::Focus => {
                self.terminal.grab_focus();
            }
        }
        // Title depends on state and settings, so refresh after any change.
        self.update_title(root);
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
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
