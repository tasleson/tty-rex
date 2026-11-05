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

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use relm4::actions::{ActionName, EmptyType, RelmAction};
use relm4::prelude::*;
use vte::prelude::*;

use crate::app::{App, TabRequest};
use crate::config;
use crate::find_bar::{FindBar, FindBarInput, FindBarOutput};
use crate::settings::NewTabPosition;
use crate::shortcuts::Shortcuts;
use crate::terminal;

const DEFAULT_ICON_NAME: &str = "utilities-terminal-symbolic";
const DEVEL_STYLE_CLASS: &str = "devel";
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
relm4::new_stateless_action!(pub NewWindowAction, WindowActions, "new-window");
relm4::new_stateless_action!(pub CloseTabAction, WindowActions, "close-tab");
relm4::new_stateless_action!(pub NextTabAction, WindowActions, "next-tab");
relm4::new_stateless_action!(pub PreviousTabAction, WindowActions, "previous-tab");
relm4::new_stateless_action!(pub CopyAction, WindowActions, "copy-clipboard");
relm4::new_stateless_action!(pub PasteAction, WindowActions, "paste-clipboard");
relm4::new_stateless_action!(pub ZoomInAction, WindowActions, "zoom-in");
relm4::new_stateless_action!(pub ZoomOutAction, WindowActions, "zoom-out");
relm4::new_stateless_action!(pub ZoomOneAction, WindowActions, "zoom-one");
relm4::new_stateless_action!(pub SearchAction, WindowActions, "search");
relm4::new_stateless_action!(pub PreferencesAction, WindowActions, "preferences");
relm4::new_stateless_action!(
    pub ShowKeyboardShortcutsAction,
    WindowActions,
    "show-keyboard-shortcuts"
);

pub struct WindowInit {
    pub app: Rc<App>,
}

#[derive(Debug)]
pub enum WindowInput {
    /// Adds a fully specified tab (used by the command line).
    AddTab(TabRequest),
    /// Adds a tab inheriting directory, zoom and size from the active tab;
    /// `None` keeps the active tab's profile.
    NewTab {
        profile_uuid: Option<String>,
    },
    NewWindow,
    CloseTab,
    NextTab,
    PreviousTab,
    Copy,
    Paste,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ShowFind,
    FindDismissed,
    ShowPreferences,
    ShowKeyboardShortcuts,
    SelectedPageChanged,
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
    /// Kept alive so the controller follows settings changes.
    _shortcuts: Option<Shortcuts>,
    /// Disabled without a selection so the shortcut reaches the terminal.
    copy_action: gio::SimpleAction,
    /// Handlers on the selected tab's terminal, dropped on tab switch.
    terminal_handlers: Option<(vte::Terminal, Vec<glib::SignalHandlerId>)>,
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

    fn send_to_active_tab(&self, message: crate::tab::TabInput) {
        if let Some(tab) = self.selected_child().and_then(|c| self.app.tab(&c)) {
            tab.sender.emit(message);
        }
    }

    /// A request for a new tab carrying over state from the active tab, as
    /// `ptyxis_window_apply_current_settings()` does.
    fn inherited_request(&self, profile_uuid: Option<String>) -> TabRequest {
        let mut request = TabRequest {
            profile_uuid,
            ..Default::default()
        };
        if let Some(active) = self.selected_child().and_then(|c| self.app.tab(&c)) {
            request.profile_uuid = request.profile_uuid.or(Some(active.profile_uuid));
            request.previous_cwd_uri = terminal::current_directory_uri(&active.terminal);
            request.zoom = active.zoom.get();
            request.grid_size = u32::try_from(active.terminal.column_count())
                .ok()
                .zip(u32::try_from(active.terminal.row_count()).ok());
        }
        request
    }

    fn insert_position(&self) -> i32 {
        let n_pages = self.tab_view.n_pages();
        match (
            self.app.settings().new_tab_position(),
            self.tab_view.selected_page(),
        ) {
            (NewTabPosition::Next, Some(page)) => self.tab_view.page_position(&page) + 1,
            _ => n_pages,
        }
    }

    fn add_tab(&mut self, mut request: TabRequest) {
        if request.grid_size.is_none() && self.tab_view.n_pages() == 0 {
            request.grid_size = Some(self.app.settings().initial_grid_size());
        }
        match self.app.create_tab(request) {
            Ok(child) => {
                let page = self.tab_view.insert(&child, self.insert_position());
                self.tab_view.set_selected_page(&page);
            }
            Err(e) => {
                glib::g_critical!("Ptyxis", "Failed to create tab: {e}");
                if self.tab_view.n_pages() == 0 {
                    self.window.destroy();
                }
            }
        }
    }

    fn close_if_empty(&self) {
        if self.tab_view.n_pages() == 0 && !self.tab_view.is_transferring_page() {
            self.window.destroy();
        }
    }

    fn on_selected_page_changed(&mut self) {
        let page = self.tab_view.selected_page();
        self.title_bindings.set_source(page.as_ref());

        if let Some((terminal, ids)) = self.terminal_handlers.take() {
            for id in ids {
                terminal.disconnect(id);
            }
        }
        self.copy_action.set_enabled(false);
        let tab = page.as_ref().and_then(|page| self.app.tab(&page.child()));
        self.find_bar.emit(FindBarInput::SetTerminal(
            tab.as_ref().map(|t| t.terminal.clone()),
        ));
        let Some(page) = page else {
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
        let cwd_id =
            terminal::connect_current_directory_uri_changed(&tab.terminal, update_subtitle);

        let copy_action = self.copy_action.clone();
        copy_action.set_enabled(tab.terminal.has_selection());
        let selection_id = tab.terminal.connect_selection_changed(move |terminal| {
            copy_action.set_enabled(terminal.has_selection())
        });
        self.terminal_handlers = Some((tab.terminal.clone(), vec![cwd_id, selection_id]));
        tab.sender.emit(crate::tab::TabInput::Focus);
    }
}

/// Forgets all tabs of a window that is going away.
fn release_tabs(app: &App, tab_view: &adw::TabView) {
    for i in 0..tab_view.n_pages() {
        app.remove_tab(&tab_view.nth_page(i).child());
    }
}

fn build_primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<NewWindowAction>::to_menu_item("New _Window"));
    section.append_item(&RelmAction::<CloseTabAction>::to_menu_item("_Close Tab"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append_item(&RelmAction::<PreferencesAction>::to_menu_item(
        "_Preferences",
    ));
    section.append_item(&RelmAction::<ShowKeyboardShortcutsAction>::to_menu_item(
        "_Keyboard Shortcuts",
    ));
    menu.append_section(None, &section);
    menu
}

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
        if config::APP_ID.ends_with(".Devel") {
            window.add_css_class(DEVEL_STYLE_CLASS);
        }

        let tab_view = adw::TabView::new();
        tab_view.set_default_icon(&gio::ThemedIcon::new(DEFAULT_ICON_NAME));
        tab_view.set_vexpand(true);
        // Our own GSettings-driven shortcuts replace libadwaita's defaults.
        tab_view.set_shortcuts(adw::TabViewShortcuts::empty());

        let tab_bar = adw::TabBar::builder()
            .view(&tab_view)
            .autohide(true)
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
        header.pack_start(&new_tab_button);
        let primary_menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .menu_model(&build_primary_menu())
            .focus_on_click(false)
            .build();
        header.pack_end(&primary_menu_button);

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

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&tab_view);
        content.append(&find_bar_revealer);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.add_top_bar(&tab_bar);
        toolbar_view.set_content(Some(&content));
        window.set_content(Some(&toolbar_view));

        let title_bindings = glib::BindingGroup::new();
        title_bindings
            .bind("title", &window_title, "title")
            .sync_create()
            .build();
        title_bindings
            .bind("title", &window, "title")
            .sync_create()
            .build();

        add_action::<NewTabAction>(&window, &sender, || WindowInput::NewTab {
            profile_uuid: None,
        });
        let input = sender.input_sender().clone();
        let new_tab_with_profile =
            RelmAction::<NewTabWithProfileAction>::new_with_target_value(move |_, uuid| {
                input.emit(WindowInput::NewTab {
                    profile_uuid: Some(uuid),
                })
            });
        window.add_action(new_tab_with_profile.gio_action());
        add_action::<NewWindowAction>(&window, &sender, || WindowInput::NewWindow);
        add_action::<CloseTabAction>(&window, &sender, || WindowInput::CloseTab);
        add_action::<NextTabAction>(&window, &sender, || WindowInput::NextTab);
        add_action::<PreviousTabAction>(&window, &sender, || WindowInput::PreviousTab);
        let copy_action = add_action::<CopyAction>(&window, &sender, || WindowInput::Copy);
        copy_action.set_enabled(false);
        add_action::<PasteAction>(&window, &sender, || WindowInput::Paste);
        add_action::<ZoomInAction>(&window, &sender, || WindowInput::ZoomIn);
        add_action::<ZoomOutAction>(&window, &sender, || WindowInput::ZoomOut);
        add_action::<ZoomOneAction>(&window, &sender, || WindowInput::ZoomReset);
        add_action::<SearchAction>(&window, &sender, || WindowInput::ShowFind);
        add_action::<PreferencesAction>(&window, &sender, || WindowInput::ShowPreferences);
        add_action::<ShowKeyboardShortcutsAction>(&window, &sender, || {
            WindowInput::ShowKeyboardShortcuts
        });

        let shortcuts = Shortcuts::new()
            .inspect(|shortcuts| window.add_controller(shortcuts.controller().clone()))
            .inspect_err(|e| glib::g_warning!("Ptyxis", "Keyboard shortcuts unavailable: {e}"))
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
        let weak_app = Rc::downgrade(&app);
        tab_view.connect_create_window(move |_| {
            let app = weak_app.upgrade()?;
            let window = app.new_window();
            window.window.present();
            Some(window.tab_view)
        });

        // Tabs must be released while the tab view still has its pages, so
        // this runs synchronously rather than through a message.
        let weak_app = Rc::downgrade(&app);
        let weak_tab_view = tab_view.downgrade();
        window.connect_close_request(move |_| {
            if let (Some(app), Some(tab_view)) = (weak_app.upgrade(), weak_tab_view.upgrade()) {
                release_tabs(&app, &tab_view);
            }
            glib::Propagation::Proceed
        });

        let model = Window {
            app,
            window,
            tab_view,
            window_title,
            title_bindings,
            find_bar,
            find_bar_revealer,
            _shortcuts: shortcuts,
            copy_action,
            terminal_handlers: None,
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        use crate::tab::TabInput;
        match message {
            WindowInput::AddTab(request) => self.add_tab(request),
            WindowInput::NewTab { profile_uuid } => {
                let request = self.inherited_request(profile_uuid);
                self.add_tab(request);
            }
            WindowInput::NewWindow => {
                let request = self.inherited_request(None);
                let window = self.app.new_window();
                window.sender.emit(WindowInput::AddTab(request));
                window.window.present();
            }
            WindowInput::CloseTab => {
                if let Some(page) = self.tab_view.selected_page() {
                    self.tab_view.close_page(&page);
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
            WindowInput::ShowPreferences => {
                self.app.show_preferences(Some(self.window.upcast_ref()));
            }
            WindowInput::ShowKeyboardShortcuts => {
                self.app.show_shortcuts(Some(self.window.upcast_ref()));
            }
            WindowInput::FindDismissed => {
                self.find_bar_revealer.set_reveal_child(false);
                self.send_to_active_tab(TabInput::Focus);
            }
            WindowInput::SelectedPageChanged => self.on_selected_page_changed(),
            WindowInput::PageDetached {
                child,
                transferring,
            } => {
                if !transferring {
                    self.app.remove_tab(&child);
                    self.close_if_empty();
                }
            }
            WindowInput::TransferFinished => self.close_if_empty(),
        }
    }
}
