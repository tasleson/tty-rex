// preferences/mod.rs
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

//! The preferences window (`src/ptyxis-preferences-window.c` / `.ui`).
//!
//! Like the C version this is an `adw::PreferencesWindow` rather than an
//! `adw::PreferencesDialog`: the C code keeps the deprecated window so it
//! can be a separate toplevel that stays usable next to terminal windows.
//! The Appearance and Behavior pages edit the default profile and follow
//! it when another profile is made the default.
#![allow(deprecated)] // AdwPreferencesWindow and its subpage/toast API.

mod accel_dialog;
mod custom_links;
mod profile_editor;
mod widgets;

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use relm4::prelude::*;

use self::accel_dialog::{AccelDialog, AccelDialogInit};
use self::custom_links::CustomLinksGroup;
use self::profile_editor::{ProfileEditor, ProfileEditorInit, ProfileEditorInput};
use crate::choices;
use crate::container::Containers;
use crate::palette::{self, Face, Palette, Scarf};
use crate::palette_preview::PalettePreview;
use crate::profile::{self, Profile, Profiles};
use crate::settings::{AppSettings, key, tty_rex_key};
use crate::shortcuts;
use crate::signal_guard::SignalGuard;

const DEFAULT_HEIGHT: i32 = 700;
const MIN_WIDTH: i32 = 360;
const MIN_HEIGHT: i32 = 294;
const PALETTE_CSS_CLASS: &str = "palette";
const PALETTE_SUFFIX: &str = ".palette";
const PALETTE_RELOAD_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
const DEFAULT_PROFILE_ACTIONS: &str = "default-profile";
const SHORTCUTS_PAGE_NAME: &str = "shortcuts";
const SHORTCUT_DISABLED_PAGE: &str = "disabled";
const SHORTCUT_LABEL_PAGE: &str = "label";
const SHOW_ALL_PALETTES: (&str, &str) = ("Show All Palettes", "down-small-symbolic");
const SHOW_FEWER_PALETTES: (&str, &str) = ("Show Fewer Palettes", "up-small-symbolic");

/// Picks the palette highlight a color setting replaces.
type HighlightOf = fn(&Face) -> &Scarf;

/// Highlight color rows: title, subtitle, setting and the palette
/// highlight shown while nothing is chosen.
const HIGHLIGHT_ROWS: &[(&str, &str, &str, HighlightOf)] = &[
    (
        "Superuser Color",
        "Header bar color while sudo or another superuser command runs",
        tty_rex_key::SUPERUSER_HIGHLIGHT_COLOR,
        |face| &face.superuser,
    ),
    (
        "Remote Color",
        "Header bar color while ssh or another remote command runs",
        tty_rex_key::REMOTE_HIGHLIGHT_COLOR,
        |face| &face.remote,
    ),
];

/// Whether a palette appears in the Appearance grid (`do_filter_palettes()`):
/// primary palettes and the current one always do; others only when all
/// palettes are shown and they support the current light/dark style.
pub fn palette_visible(palette: &Palette, current_id: &str, show_all: bool, dark: bool) -> bool {
    if palette.is_primary() || palette.id() == current_id {
        return true;
    }
    show_all
        && if dark {
            palette.has_dark()
        } else {
            palette.has_light()
        }
}

pub struct PreferencesInit {
    pub settings: AppSettings,
    pub profiles: Profiles,
    pub containers: Containers,
}

#[derive(Debug)]
pub enum PreferencesInput {
    /// Rebuild the profile list (ids, labels or the default changed).
    ProfilesChanged,
    DefaultProfileChanged,
    RefreshPalettes,
    /// A `.palette` file appeared, changed or vanished in a user directory.
    QueueUserPalettesReload,
    UserPalettesChanged,
    ToggleShowAllPalettes,
    AddProfile,
    EditProfile(String),
    SelectFont,
    /// A tty-rex highlight color setting changed.
    HighlightColorsChanged,
    /// Show the Shortcuts page (`ptyxis_preferences_window_edit_shortcuts()`).
    ShowShortcuts,
    EditShortcut {
        key: &'static str,
        title: &'static str,
    },
}

pub struct Preferences {
    settings: AppSettings,
    profiles: Profiles,
    containers: Containers,
    default_profile: Option<Profile>,
    default_binder: widgets::Binder,
    custom_links: Option<CustomLinksGroup>,
    opacity_group: adw::PreferencesGroup,
    palette_flow: gtk::FlowBox,
    palette_previews: Vec<(PalettePreview, gtk::ToggleButton)>,
    highlight_rows: Vec<(widgets::ColorRow, HighlightOf)>,
    show_all_palettes: bool,
    show_more: adw::ButtonContent,
    profiles_list: gtk::ListBox,
    profile_editor: Option<Controller<ProfileEditor>>,
    shortcut_settings: Option<shortcuts::ShortcutSettings>,
    accel_dialog: Option<Controller<AccelDialog>>,
    /// Profiles shown in the list, kept alive for their label handlers.
    listed_profiles: Vec<Profile>,
    /// Handlers on shared objects, disconnected on shutdown.
    shared_handlers: Vec<SignalGuard>,
    _palette_monitors: Vec<gio::FileMonitor>,
    palette_reload: Option<glib::SourceId>,
}

impl Preferences {
    fn current_palette_id(&self) -> String {
        self.default_profile
            .as_ref()
            .map(|profile| profile.palette_id().into())
            .unwrap_or_default()
    }

    fn rebuild_palettes(&mut self) {
        while let Some(child) = self.palette_flow.first_child() {
            self.palette_flow.remove(&child);
        }
        self.palette_previews.clear();

        let dark = adw::StyleManager::default().is_dark();
        let current = self.current_palette_id();
        let font = self.settings.font_desc();
        for palette in palette::all() {
            if palette_visible(&palette, &current, self.show_all_palettes, dark) {
                let (child, preview, button) = palette_card(&palette, dark, &font);
                self.palette_flow.append(&child);
                preview.set_selected(palette.id() == current);
                self.palette_previews.push((preview, button));
            }
        }
        self.refresh_highlight_rows();
    }

    /// Shows the chosen highlight colors, or the default profile's palette
    /// colors where none is chosen.
    fn refresh_highlight_rows(&self) {
        let Some(palette) = palette::lookup_or_default(&self.current_palette_id()) else {
            return;
        };
        let face = palette.face(adw::StyleManager::default().is_dark());
        for (row, highlight_of) in &self.highlight_rows {
            row.refresh(&highlight_of(face).background);
        }
    }

    fn bind_default_profile(
        &mut self,
        root: &adw::PreferencesWindow,
        sender: &ComponentSender<Self>,
    ) {
        let profile = match self.profiles.default_profile() {
            Ok(profile) => profile,
            Err(e) => {
                glib::g_warning!("tty-rex", "Cannot edit the default profile: {e}");
                return;
            }
        };
        let gsettings = profile.gsettings();

        self.default_binder.apply(gsettings);
        // As in C, only show the transparency control if it was changed.
        self.opacity_group
            .set_visible(gsettings.double(profile::key::OPACITY) < 1.0);

        let actions = gio::SimpleActionGroup::new();
        actions.add_action(&gsettings.create_action(profile::key::PALETTE));
        root.insert_action_group(DEFAULT_PROFILE_ACTIONS, Some(&actions));

        let input = sender.input_sender().clone();
        gsettings.connect_changed(Some(profile::key::PALETTE), move |_, _| {
            input.emit(PreferencesInput::RefreshPalettes)
        });

        if let Some(links) = &self.custom_links {
            links.set_profile(&profile);
        }
        self.default_profile = Some(profile);
    }

    fn rebuild_profiles(&mut self, sender: &ComponentSender<Self>) {
        self.profiles_list.remove_all();
        self.listed_profiles = self.profiles.list();
        // Compare against the profile actually in use, so a stale
        // default-profile-uuid still marks one row.
        let default_uuid = self.profiles.effective_default_uuid().unwrap_or_default();
        for profile in &self.listed_profiles {
            let row = profile_row(
                profile,
                profile.uuid() == default_uuid,
                &self.profiles,
                sender,
            );
            self.profiles_list.append(&row);
            let input = sender.input_sender().clone();
            profile
                .gsettings()
                .connect_changed(Some(profile::key::LABEL), move |_, _| {
                    input.emit(PreferencesInput::ProfilesChanged)
                });
        }
        self.profiles_list
            .append(&widgets::add_button_row("Add Profile", "profile.add"));
    }

    fn edit_profile(&mut self, root: &adw::PreferencesWindow, uuid: &str) {
        let profile = match Profile::open(uuid) {
            Ok(profile) => profile,
            Err(e) => {
                glib::g_warning!("tty-rex", "Cannot edit profile {uuid}: {e}");
                return;
            }
        };
        let editor = ProfileEditor::builder()
            .launch(ProfileEditorInit {
                profile,
                profiles: self.profiles.clone(),
                containers: self.containers.clone(),
            })
            .detach();
        root.pop_subpage();
        root.push_subpage(editor.widget());
        self.profile_editor = Some(editor);
    }

    fn select_font(&self, root: &adw::PreferencesWindow) {
        let gsettings = self.settings.gsettings().clone();
        let font_name = gsettings.string(key::FONT_NAME);
        let initial = if font_name.is_empty() {
            self.settings.system_font_name()
        } else {
            font_name.to_string()
        };
        let filter = gtk::CustomFilter::new(|item| {
            if let Some(family) = item.downcast_ref::<pango::FontFamily>() {
                family.is_monospace()
            } else if let Some(face) = item.downcast_ref::<pango::FontFace>() {
                face.family().is_monospace()
            } else {
                false
            }
        });
        let dialog = gtk::FontDialog::builder()
            .title("Select Font")
            .filter(&filter)
            .build();
        dialog.choose_font(
            Some(root),
            Some(&pango::FontDescription::from_string(&initial)),
            None::<&gio::Cancellable>,
            move |result| {
                // Dismissing the dialog is reported as an error; ignore it.
                if let Ok(font) = result {
                    let name = font.to_str();
                    if !name.is_empty()
                        && let Err(e) = gsettings.set_string(key::FONT_NAME, &name)
                    {
                        glib::g_warning!("tty-rex", "Failed to save font: {e}");
                    }
                }
            },
        );
    }
}

/// A palette preview inside a toggle button bound to the default
/// profile's `palette` action (`create_palette_preview()`).
fn palette_card(
    palette: &Rc<Palette>,
    dark: bool,
    font: &pango::FontDescription,
) -> (gtk::FlowBoxChild, PalettePreview, gtk::ToggleButton) {
    let preview = PalettePreview::new(palette, dark, Some(font));
    let button = gtk::ToggleButton::builder()
        .css_classes([PALETTE_CSS_CLASS])
        .action_name(format!(
            "{DEFAULT_PROFILE_ACTIONS}.{}",
            profile::key::PALETTE
        ))
        .action_target(&palette.id().to_variant())
        .child(preview.widget())
        .focus_on_click(false)
        .can_focus(false)
        .overflow(gtk::Overflow::Hidden)
        .build();
    let child = gtk::FlowBoxChild::builder().child(&button).build();
    let target = palette.id().to_variant();
    child.connect_activate(move |child| {
        let _ = child.activate_action(
            &format!("{DEFAULT_PROFILE_ACTIONS}.{}", profile::key::PALETTE),
            Some(&target),
        );
    });
    (child, preview, button)
}

/// A row of the Profiles page (`src/ptyxis-profile-row.c`).
fn profile_row(
    profile: &Profile,
    is_default: bool,
    profiles: &Profiles,
    sender: &ComponentSender<Preferences>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(profile.label())
        .activatable(true)
        .build();
    let checkmark = gtk::Image::builder()
        .icon_name("object-select-symbolic")
        .valign(gtk::Align::Center)
        .margin_end(6)
        .visible(is_default)
        .build();
    row.add_suffix(&checkmark);

    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some("_Make Default"), Some("profile.make-default"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("_Duplicate…"), Some("profile.duplicate"));
    section.append(Some("_Edit…"), Some("profile.edit"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some("_Remove"), Some("profile.remove"));
    menu.append_section(None, &section);
    row.add_suffix(
        &gtk::MenuButton::builder()
            .css_classes(["flat"])
            .tooltip_text("View More")
            .icon_name("view-more-symbolic")
            .valign(gtk::Align::Center)
            .menu_model(&menu)
            .build(),
    );

    let actions = gio::SimpleActionGroup::new();
    let add = |name: &str, f: Box<dyn Fn()>| {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| f());
        actions.add_action(&action);
        action
    };
    let uuid = profile.uuid().to_owned();

    let make_default = {
        let profiles = profiles.clone();
        let uuid = uuid.clone();
        add(
            "make-default",
            Box::new(move || {
                if let Err(e) = profiles.set_default(&uuid) {
                    glib::g_warning!("tty-rex", "Failed to set default profile: {e}");
                }
            }),
        )
    };
    make_default.set_enabled(!is_default);
    {
        let profiles = profiles.clone();
        let profile = profile.clone();
        add(
            "duplicate",
            Box::new(move || {
                if let Err(e) = profiles.duplicate(&profile) {
                    glib::g_warning!("tty-rex", "Failed to duplicate profile: {e}");
                }
            }),
        );
    }
    {
        let input = sender.input_sender().clone();
        let uuid = uuid.clone();
        add(
            "edit",
            Box::new(move || input.emit(PreferencesInput::EditProfile(uuid.clone()))),
        );
    }
    {
        let profiles = profiles.clone();
        let profile = profile.clone();
        let row_ref = row.downgrade();
        add(
            "remove",
            Box::new(move || {
                let Some(row) = row_ref.upgrade() else {
                    return;
                };
                let toast = profile_editor::removal_toast(&profiles, &profile);
                match profiles.remove(profile.uuid()) {
                    Ok(()) => widgets::add_toast(&row, toast),
                    Err(e) => glib::g_warning!("tty-rex", "Failed to remove profile: {e}"),
                }
            }),
        );
    }
    row.insert_action_group("profile", Some(&actions));

    let input = sender.input_sender().clone();
    row.connect_activated(move |_| input.emit(PreferencesInput::EditProfile(uuid.clone())));
    row
}

impl Component for Preferences {
    type Init = PreferencesInit;
    type Input = PreferencesInput;
    type Output = ();
    type CommandOutput = ();
    type Root = adw::PreferencesWindow;
    type Widgets = ();

    fn init_root() -> Self::Root {
        adw::PreferencesWindow::builder()
            .default_height(DEFAULT_HEIGHT)
            .width_request(MIN_WIDTH)
            .height_request(MIN_HEIGHT)
            .modal(false)
            .build()
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let gsettings = init.settings.gsettings().clone();
        let mut binder = widgets::Binder::default();
        let input = sender.input_sender();

        let close = gtk::ShortcutController::new();
        close.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<control>w"),
            Some(gtk::NamedAction::new("window.close")),
        ));
        root.add_controller(close);

        // Appearance
        let appearance = widgets::page("Appearance", "font-select-symbolic");

        let show_more = adw::ButtonContent::builder()
            .label(SHOW_ALL_PALETTES.0)
            .icon_name(SHOW_ALL_PALETTES.1)
            .build();
        let show_more_button = gtk::Button::builder()
            .child(&show_more)
            .css_classes(["flat"])
            .build();
        let toggle = input.clone();
        show_more_button
            .connect_clicked(move |_| toggle.emit(PreferencesInput::ToggleShowAllPalettes));
        let palette_flow = gtk::FlowBox::builder()
            .hexpand(true)
            .column_spacing(9)
            .row_spacing(9)
            .max_children_per_line(3)
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .build();
        palette_flow.add_controller(palette_drop_target());
        let group = widgets::group(Some("Palette"));
        group.set_header_suffix(Some(&show_more_button));
        group.add(&palette_flow);
        appearance.add(&group);

        let (opacity_row, opacity, _) = widgets::opacity_row();
        binder.bind(profile::key::OPACITY, &opacity, "value");
        let opacity_group = widgets::group(None);
        opacity_group.set_visible(false);
        opacity_group.add(&opacity_row);
        appearance.add(&opacity_group);

        appearance.add(&font_group(&gsettings, input));
        appearance.add(&profile_editor::spacing_group(&mut binder));

        let text_blink = widgets::combo_row("Blinking Text", None, choices::TEXT_BLINK_MODES);
        widgets::bind_choice(
            &gsettings,
            key::TEXT_BLINK_MODE,
            &text_blink,
            choices::TEXT_BLINK_MODES,
        );
        let group = widgets::group(None);
        group.add(&text_blink);
        appearance.add(&group);

        let cursor_shape = widgets::combo_row("Cursor Shape", None, choices::CURSOR_SHAPES);
        widgets::bind_choice(
            &gsettings,
            key::CURSOR_SHAPE,
            &cursor_shape,
            choices::CURSOR_SHAPES,
        );
        let cursor_blink = widgets::combo_row("Cursor Blinking", None, choices::CURSOR_BLINK_MODES);
        widgets::bind_choice(
            &gsettings,
            key::CURSOR_BLINK_MODE,
            &cursor_blink,
            choices::CURSOR_BLINK_MODES,
        );
        let group = widgets::group(Some("Cursor"));
        group.add(&cursor_shape);
        group.add(&cursor_blink);
        appearance.add(&group);

        let (group, highlight_rows) = highlight_group(&init.settings);
        appearance.add(&group);
        root.add(&appearance);

        // Behavior
        let behavior = widgets::page("Behavior", "preferences-desktop-accessibility-symbolic");
        let tab_position = widgets::combo_row("New Tab Position", None, choices::TAB_POSITIONS);
        widgets::bind_choice(
            &gsettings,
            key::NEW_TAB_POSITION,
            &tab_position,
            choices::TAB_POSITIONS,
        );
        let group = widgets::group(Some("Interface"));
        group.add(&tab_position);
        behavior.add(&group);
        behavior.add(&window_group(&gsettings));
        behavior.add(&telemetry_group(&init.settings));

        let switch_group = |title: &str, rows: &[(&str, &str, &'static str)]| {
            let group = widgets::group(Some(title));
            for (title, subtitle, key) in rows {
                let row = widgets::switch_row(title, Some(subtitle));
                widgets::bind(&gsettings, key, &row, "active");
                group.add(&row);
            }
            group
        };
        behavior.add(&switch_group(
            "Accessibility",
            &[(
                "Allow Screen Readers",
                "Permit screen readers such as Orca to read terminal content and typed text",
                key::ENABLE_A11Y,
            )],
        ));
        behavior.add(&switch_group(
            "Bell",
            &[
                (
                    "Audible Bell",
                    "Emit an audible bell when requested by active tab",
                    key::AUDIBLE_BELL,
                ),
                (
                    "Visual Bell",
                    "Emit a visual bell when requested by active tab",
                    key::VISUAL_BELL,
                ),
            ],
        ));
        behavior.add(&switch_group(
            "Title",
            &[(
                "Disable Dynamic Title",
                "Do not show running command in tab title",
                key::DISABLE_DYNAMIC_TITLE,
            )],
        ));

        let scrollbars = widgets::combo_row("Use Scrollbars", None, choices::SCROLLBAR_POLICIES);
        widgets::bind_choice(
            &gsettings,
            key::SCROLLBAR_POLICY,
            &scrollbars,
            choices::SCROLLBAR_POLICIES,
        );
        let group = widgets::group(Some("Scrolling"));
        group.add(&scrollbars);
        behavior.add(&group);
        let scrolling = profile_editor::scrolling_group(&mut binder);
        // The C page titles this group "Scrolling" only once, above.
        scrolling.set_title("");
        behavior.add(&scrolling);
        behavior.add(&profile_editor::scrollback_group(&mut binder));
        behavior.add(&profile_editor::login_shell_group(
            &mut binder,
            Some("Shell"),
        ));
        behavior.add(&profile_editor::directory_and_exit_group(&mut binder));
        behavior.add(&profile_editor::compatibility_group(&mut binder));
        let custom_links = init
            .profiles
            .default_profile()
            .inspect_err(|e| glib::g_warning!("tty-rex", "Cannot load custom links: {e}"))
            .ok()
            .map(|profile| CustomLinksGroup::new(&profile));
        if let Some(links) = &custom_links {
            behavior.add(links.widget());
        }
        root.add(&behavior);

        // Shortcuts
        let shortcut_settings = shortcuts::open_settings()
            .inspect_err(|e| glib::g_warning!("tty-rex", "Shortcut settings unavailable: {e}"))
            .ok();
        if let Some(settings) = &shortcut_settings {
            root.add(&shortcuts_page(settings, input));
        }

        // Profiles
        let profiles_page = widgets::page("Profiles", "system-users-symbolic");
        let profiles_list = widgets::boxed_list();
        let group = widgets::group(Some("Profiles"));
        group.add(&profiles_list);
        profiles_page.add(&group);
        root.add(&profiles_page);

        widgets::install_action(&root, "profile", "add", input, || {
            PreferencesInput::AddProfile
        });

        let mut shared_handlers = Vec::new();
        let changed = input.clone();
        shared_handlers.push(SignalGuard::new(
            &gsettings,
            gsettings.connect_changed(Some(key::PROFILE_UUIDS), move |_, _| {
                changed.emit(PreferencesInput::ProfilesChanged)
            }),
        ));
        let changed = input.clone();
        shared_handlers.push(SignalGuard::new(
            &gsettings,
            gsettings.connect_changed(Some(key::DEFAULT_PROFILE_UUID), move |_, _| {
                changed.emit(PreferencesInput::DefaultProfileChanged)
            }),
        ));
        let changed = input.clone();
        shared_handlers.push(SignalGuard::new(
            &gsettings,
            gsettings.connect_changed(None, move |_, key| {
                if matches!(key, key::FONT_NAME | key::USE_SYSTEM_FONT) {
                    changed.emit(PreferencesInput::RefreshPalettes)
                }
            }),
        ));
        let tty_rex_settings = init.settings.tty_rex_gsettings().clone();
        let changed = input.clone();
        shared_handlers.push(SignalGuard::new(
            &tty_rex_settings,
            tty_rex_settings.connect_changed(None, move |_, key| {
                if tty_rex_key::COLOR_KEYS.contains(&key) {
                    changed.emit(PreferencesInput::HighlightColorsChanged)
                }
            }),
        ));
        let style_manager = adw::StyleManager::default();
        let changed = input.clone();
        shared_handlers.push(SignalGuard::new(
            &style_manager,
            style_manager
                .connect_dark_notify(move |_| changed.emit(PreferencesInput::RefreshPalettes)),
        ));

        let palette_monitors =
            palette::user_palettes_dirs(&glib::user_data_dir(), crate::config::APP_ID)
                .into_iter()
                .filter_map(|dir| {
                    gio::File::for_path(&dir)
                        .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
                        .inspect_err(|e| {
                            glib::g_warning!("tty-rex", "Cannot watch {}: {e}", dir.display())
                        })
                        .ok()
                })
                .collect::<Vec<_>>();
        for monitor in &palette_monitors {
            let changed = input.clone();
            monitor.connect_changed(move |_, file, other, _| {
                let is_palette = |f: Option<&gio::File>| {
                    f.and_then(|f| f.basename())
                        .is_some_and(|name| name.to_string_lossy().ends_with(PALETTE_SUFFIX))
                };
                if is_palette(Some(file)) || is_palette(other) {
                    changed.emit(PreferencesInput::QueueUserPalettesReload);
                }
            });
        }

        let mut model = Preferences {
            settings: init.settings,
            profiles: init.profiles,
            containers: init.containers,
            default_profile: None,
            default_binder: binder,
            custom_links,
            opacity_group,
            palette_flow,
            palette_previews: Vec::new(),
            highlight_rows,
            show_all_palettes: false,
            show_more,
            profiles_list,
            profile_editor: None,
            shortcut_settings,
            accel_dialog: None,
            listed_profiles: Vec::new(),
            shared_handlers,
            _palette_monitors: palette_monitors,
            palette_reload: None,
        };
        model.bind_default_profile(&root, &sender);
        model.rebuild_palettes();
        model.rebuild_profiles(&sender);
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            PreferencesInput::ProfilesChanged => self.rebuild_profiles(&sender),
            PreferencesInput::DefaultProfileChanged => {
                self.bind_default_profile(root, &sender);
                self.rebuild_palettes();
                self.rebuild_profiles(&sender);
            }
            PreferencesInput::RefreshPalettes => self.rebuild_palettes(),
            PreferencesInput::UserPalettesChanged => {
                self.palette_reload = None;
                self.rebuild_palettes();
                if let Some(editor) = &self.profile_editor {
                    editor.emit(ProfileEditorInput::RefreshPalettes);
                }
            }
            PreferencesInput::QueueUserPalettesReload => {
                // Coalesce bursts of file events, like the C user palette
                // model's low-priority reload timeout.
                if self.palette_reload.is_none() {
                    let input = sender.input_sender().clone();
                    self.palette_reload = Some(glib::timeout_add_local_once(
                        PALETTE_RELOAD_DELAY,
                        move || input.emit(PreferencesInput::UserPalettesChanged),
                    ));
                }
            }
            PreferencesInput::ToggleShowAllPalettes => {
                self.show_all_palettes = !self.show_all_palettes;
                let (label, icon) = if self.show_all_palettes {
                    SHOW_FEWER_PALETTES
                } else {
                    SHOW_ALL_PALETTES
                };
                self.show_more.set_label(label);
                self.show_more.set_icon_name(icon);
                self.rebuild_palettes();
            }
            PreferencesInput::AddProfile => {
                if let Err(e) = self.profiles.create() {
                    glib::g_warning!("tty-rex", "Failed to add profile: {e}");
                }
            }
            PreferencesInput::EditProfile(uuid) => self.edit_profile(root, &uuid),
            PreferencesInput::SelectFont => self.select_font(root),
            PreferencesInput::HighlightColorsChanged => self.refresh_highlight_rows(),
            PreferencesInput::ShowShortcuts => {
                root.pop_subpage();
                root.set_visible_page_name(SHORTCUTS_PAGE_NAME);
            }
            PreferencesInput::EditShortcut { key, title } => {
                let Some(settings) = &self.shortcut_settings else {
                    return;
                };
                let settings = settings.clone();
                let dialog = AccelDialog::builder()
                    .launch(AccelDialogInit {
                        title: title.to_owned(),
                        settings,
                        key,
                    })
                    .detach();
                dialog.widget().present(Some(root));
                self.accel_dialog = Some(dialog);
            }
        }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        if let Some(source) = self.palette_reload.take() {
            source.remove();
        }
        self.shared_handlers.clear();
    }
}

/// The Shortcuts page: one row per shortcut key, grouped as in the C
/// template (`PtyxisShortcutRow`).
fn shortcuts_page(
    settings: &shortcuts::ShortcutSettings,
    input: &relm4::Sender<PreferencesInput>,
) -> adw::PreferencesPage {
    let page = widgets::page(
        "Shortcuts",
        "preferences-desktop-keyboard-shortcuts-symbolic",
    );
    page.set_name(Some(SHORTCUTS_PAGE_NAME));
    for (title, entries) in shortcuts::EDITOR_GROUPS {
        let group = widgets::group(*title);
        for entry in *entries {
            group.add(&shortcut_row(settings.for_key(entry.key), entry, input));
        }
        page.add(&group);
    }
    page
}

fn shortcut_row(
    settings: &gio::Settings,
    entry: &shortcuts::ShortcutEntry,
    input: &relm4::Sender<PreferencesInput>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(entry.title)
        .activatable(true)
        .build();
    if let Some(subtitle) = entry.subtitle {
        row.set_subtitle(subtitle);
    }
    let label = adw::ShortcutLabel::new("");
    label.set_halign(gtk::Align::End);
    label.set_valign(gtk::Align::Center);
    let stack = gtk::Stack::new();
    stack.add_named(
        &gtk::Label::builder()
            .label("disabled")
            .css_classes(["dim-label"])
            .build(),
        Some(SHORTCUT_DISABLED_PAGE),
    );
    stack.add_named(&label, Some(SHORTCUT_LABEL_PAGE));
    row.add_suffix(&stack);

    let update = move |settings: &gio::Settings, key: &str| {
        let value = settings.string(key);
        let enabled = shortcuts::effective_accel(&value)
            .and_then(gtk::accelerator_parse)
            .is_some();
        label.set_accelerator(&value);
        stack.set_visible_child_name(if enabled {
            SHORTCUT_LABEL_PAGE
        } else {
            SHORTCUT_DISABLED_PAGE
        });
    };
    update(settings, entry.key);
    settings.connect_changed(Some(entry.key), update);

    // Activation goes through a row action like the C row's
    // "shortcut.select", which also exposes it to assistive technologies.
    let actions = gio::SimpleActionGroup::new();
    let select = gio::SimpleAction::new("select", None);
    let input = input.clone();
    let (key, title) = (entry.key, entry.title);
    select.connect_activate(move |_, _| input.emit(PreferencesInput::EditShortcut { key, title }));
    actions.add_action(&select);
    row.insert_action_group("shortcut", Some(&actions));
    row.set_action_name(Some("shortcut.select"));
    row
}

/// The "Font" group: system font switch expanding into a custom font row.
fn font_group(
    settings: &gio::Settings,
    input: &relm4::Sender<PreferencesInput>,
) -> adw::PreferencesGroup {
    let use_system_font = gtk::Switch::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .build();
    widgets::bind(settings, key::USE_SYSTEM_FONT, &use_system_font, "active");
    let expander = adw::ExpanderRow::builder()
        .title("Use System _Font")
        .use_underline(true)
        .build();
    expander.add_action(&use_system_font);
    use_system_font
        .bind_property("active", &expander, "expanded")
        .sync_create()
        .bidirectional()
        .invert_boolean()
        .build();

    let font_name = gtk::Label::builder()
        .hexpand(true)
        .xalign(1.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    widgets::bind(settings, key::FONT_NAME, &font_name, "label");
    let suffix = gtk::Box::builder().hexpand(true).spacing(12).build();
    suffix.append(&font_name);
    suffix.append(&gtk::Image::from_icon_name("go-next-symbolic"));
    let font_row = adw::ActionRow::builder().title("Custom Font").build();
    font_row.add_suffix(&suffix);
    use_system_font
        .bind_property("active", &font_name, "sensitive")
        .sync_create()
        .invert_boolean()
        .build();
    use_system_font
        .bind_property("active", &font_row, "activatable")
        .sync_create()
        .invert_boolean()
        .build();
    let select = input.clone();
    font_row.connect_activated(move |_| select.emit(PreferencesInput::SelectFont));
    expander.add_row(&font_row);

    let group = widgets::group(Some("Font"));
    group.add(&expander);
    group
}

/// The `visual-process-leader` switch and the highlight colors it uses,
/// which are only editable while it is on.
fn highlight_group(
    settings: &AppSettings,
) -> (adw::PreferencesGroup, Vec<(widgets::ColorRow, HighlightOf)>) {
    let gsettings = settings.gsettings();
    let enabled = widgets::switch_row(
        "Highlight Superuser and Remote Sessions",
        Some("Color the header bar while sudo, ssh or similar tools lead the terminal"),
    );
    widgets::bind(gsettings, key::VISUAL_PROCESS_LEADER, &enabled, "active");

    let group = widgets::group(Some("Session Highlighting"));
    group.add(&enabled);
    let rows = HIGHLIGHT_ROWS
        .iter()
        .map(|&(title, subtitle, setting, highlight_of)| {
            let row = widgets::ColorRow::new(settings, setting, title, subtitle);
            gsettings
                .bind(key::VISUAL_PROCESS_LEADER, &row.row, "sensitive")
                .get()
                .build();
            group.add(&row.row);
            (row, highlight_of)
        })
        .collect();
    (group, rows)
}

/// The telemetry footer's update interval, a tty-rex setting. The footer
/// follows changes as they are made.
fn telemetry_group(settings: &AppSettings) -> adw::PreferencesGroup {
    let interval = widgets::spin_row(
        "Telemetry Update Interval",
        Some("Seconds between updates of the network, CPU and memory readings"),
        widgets::TELEMETRY_INTERVAL_RANGE,
    );
    widgets::bind(
        settings.tty_rex_gsettings(),
        tty_rex_key::TELEMETRY_INTERVAL,
        &interval,
        "value",
    );
    let group = widgets::group(Some("Telemetry"));
    group.add(&interval);
    group
}

/// "Restore Session" and the "Restore Window Size" expander.
fn window_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let restore_session = widgets::switch_row(
        "Restore Session",
        Some("Attempt to restore previous tabs when tty-rex starts"),
    );
    widgets::bind(settings, key::RESTORE_SESSION, &restore_session, "active");

    let restore_size = gtk::Switch::builder().valign(gtk::Align::Center).build();
    widgets::bind(settings, key::RESTORE_WINDOW_SIZE, &restore_size, "active");
    let expander = adw::ExpanderRow::builder()
        .title("Restore Window Size")
        .subtitle("Restore the previous window size when creating a new window")
        .show_enable_switch(false)
        .build();
    expander.add_suffix(&restore_size);
    restore_size
        .bind_property("active", &expander, "expanded")
        .sync_create()
        .bidirectional()
        .invert_boolean()
        .build();
    for (title, key) in [
        ("Columns", key::DEFAULT_COLUMNS),
        ("Rows", key::DEFAULT_ROWS),
    ] {
        let row = widgets::spin_row(title, None, widgets::GRID_RANGE);
        widgets::bind(settings, key, &row, "value");
        expander.add_row(&row);
    }

    let group = widgets::group(None);
    group.add(&restore_session);
    group.add(&expander);
    group
}

/// Accepts dropped `.palette` files and copies them into the user palette
/// directory (`ptyxis_preferences_window_drop_palette_cb()`); the
/// directory monitor then refreshes the grid.
fn palette_drop_target() -> gtk::DropTarget {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    target.connect_drop(|_, value, _, _| {
        let Ok(files) = value.get::<gdk::FileList>() else {
            return false;
        };
        let dir = palette::import_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            glib::g_warning!("tty-rex", "Cannot create {}: {e}", dir.display());
            return false;
        }
        for file in files.files() {
            let Some(name) = file.basename() else {
                return false;
            };
            if !name.to_string_lossy().ends_with(PALETTE_SUFFIX) {
                return false;
            }
            let dest = gio::File::for_path(dir.join(name));
            file.copy_async(
                &dest,
                gio::FileCopyFlags::OVERWRITE,
                glib::Priority::DEFAULT,
                None::<&gio::Cancellable>,
                None,
                |result| {
                    if let Err(e) = result {
                        glib::g_warning!("tty-rex", "Failed to import palette: {e}");
                    }
                },
            );
        }
        true
    });
    target
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette(source: &str, id: &str) -> Palette {
        Palette::parse(id, id, source).unwrap()
    }

    #[test]
    fn palette_filter() {
        let colors = "Foreground=#ffffff\nColor0=#000000\nColor1=#000000\nColor2=#000000\n\
            Color3=#000000\nColor4=#000000\nColor5=#000000\nColor6=#000000\nColor7=#000000\n\
            Color8=#000000\nColor9=#000000\nColor10=#000000\nColor11=#000000\nColor12=#000000\n\
            Color13=#000000\nColor14=#000000\nColor15=#000000\n";
        let dark_only = palette(
            &format!("[Palette]\nName=D\nBackground=#000000\n{colors}"),
            "d",
        );
        let light_only = palette(
            &format!("[Palette]\nName=L\nBackground=#ffffff\n{colors}"),
            "l",
        );
        let primary = palette(
            &format!("[Palette]\nName=P\nPrimary=true\nBackground=#000000\n{colors}"),
            "p",
        );
        // (palette, current id, show all, dark, visible)
        let cases = [
            (&primary, "", false, false, true),
            (&dark_only, "d", false, false, true),
            (&dark_only, "", false, true, false),
            (&dark_only, "", true, true, true),
            (&dark_only, "", true, false, false),
            (&light_only, "", true, false, true),
            (&light_only, "", true, true, false),
        ];
        for (palette, current, show_all, dark, expected) in cases {
            assert_eq!(
                palette_visible(palette, current, show_all, dark),
                expected,
                "{} current={current} all={show_all} dark={dark}",
                palette.id()
            );
        }
    }
}
