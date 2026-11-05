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

mod custom_links;
mod profile_editor;
mod widgets;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use relm4::prelude::*;

use self::custom_links::CustomLinksGroup;
use self::profile_editor::{ProfileEditor, ProfileEditorInit, ProfileEditorInput};
use crate::choices;
use crate::palette::{self, Palette};
use crate::palette_preview::PalettePreview;
use crate::profile::{self, Profile, Profiles};
use crate::settings::{AppSettings, key};

const DEFAULT_HEIGHT: i32 = 700;
const MIN_WIDTH: i32 = 360;
const MIN_HEIGHT: i32 = 294;
const PALETTE_CSS_CLASS: &str = "palette";
const PALETTE_SUFFIX: &str = ".palette";
const PALETTE_RELOAD_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
const DEFAULT_PROFILE_ACTIONS: &str = "default-profile";
const SHOW_ALL_PALETTES: (&str, &str) = ("Show All Palettes", "down-small-symbolic");
const SHOW_FEWER_PALETTES: (&str, &str) = ("Show Fewer Palettes", "up-small-symbolic");

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
}

pub struct Preferences {
    settings: AppSettings,
    profiles: Profiles,
    default_profile: Option<Profile>,
    default_binder: widgets::Binder,
    custom_links: Option<CustomLinksGroup>,
    opacity_group: adw::PreferencesGroup,
    palette_flow: gtk::FlowBox,
    palette_previews: Vec<(PalettePreview, gtk::ToggleButton)>,
    show_all_palettes: bool,
    show_more: adw::ButtonContent,
    profiles_list: gtk::ListBox,
    profile_editor: Option<Controller<ProfileEditor>>,
    /// Profiles shown in the list, kept alive for their label handlers.
    listed_profiles: Vec<Profile>,
    /// Handlers on shared objects, disconnected on shutdown.
    shared_handlers: Vec<(glib::Object, glib::SignalHandlerId)>,
    _palette_monitors: Vec<gio::FileMonitor>,
    palette_reload: Option<glib::SourceId>,
}

impl Preferences {
    fn current_palette_id(&self) -> String {
        self.default_profile
            .as_ref()
            .map(Profile::palette_id)
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
    }

    fn bind_default_profile(
        &mut self,
        root: &adw::PreferencesWindow,
        sender: &ComponentSender<Self>,
    ) {
        let profile = match self.profiles.default_profile() {
            Ok(profile) => profile,
            Err(e) => {
                glib::g_warning!("Ptyxis", "Cannot edit the default profile: {e}");
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
                glib::g_warning!("Ptyxis", "Cannot edit profile {uuid}: {e}");
                return;
            }
        };
        let editor = ProfileEditor::builder()
            .launch(ProfileEditorInit {
                profile,
                profiles: self.profiles.clone(),
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
                        glib::g_warning!("Ptyxis", "Failed to save font: {e}");
                    }
                }
            },
        );
    }
}

/// A palette preview inside a toggle button bound to the default
/// profile's `palette` action (`create_palette_preview()`).
fn palette_card(
    palette: &Palette,
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
                    glib::g_warning!("Ptyxis", "Failed to set default profile: {e}");
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
                    glib::g_warning!("Ptyxis", "Failed to duplicate profile: {e}");
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
                    Err(e) => glib::g_warning!("Ptyxis", "Failed to remove profile: {e}"),
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
            .inspect_err(|e| glib::g_warning!("Ptyxis", "Cannot load custom links: {e}"))
            .ok()
            .map(|profile| CustomLinksGroup::new(&profile));
        if let Some(links) = &custom_links {
            behavior.add(links.widget());
        }
        root.add(&behavior);

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
        let id = gsettings.connect_changed(Some(key::PROFILE_UUIDS), move |_, _| {
            changed.emit(PreferencesInput::ProfilesChanged)
        });
        shared_handlers.push((gsettings.clone().upcast(), id));
        let changed = input.clone();
        let id = gsettings.connect_changed(Some(key::DEFAULT_PROFILE_UUID), move |_, _| {
            changed.emit(PreferencesInput::DefaultProfileChanged)
        });
        shared_handlers.push((gsettings.clone().upcast(), id));
        let changed = input.clone();
        let id = gsettings.connect_changed(None, move |_, key| {
            if matches!(key, key::FONT_NAME | key::USE_SYSTEM_FONT) {
                changed.emit(PreferencesInput::RefreshPalettes)
            }
        });
        shared_handlers.push((gsettings.clone().upcast(), id));
        let style_manager = adw::StyleManager::default();
        let changed = input.clone();
        let id = style_manager
            .connect_dark_notify(move |_| changed.emit(PreferencesInput::RefreshPalettes));
        shared_handlers.push((style_manager.upcast(), id));

        let palette_monitors =
            palette::user_palettes_dirs(&glib::user_data_dir(), crate::config::APP_ID)
                .into_iter()
                .filter_map(|dir| {
                    gio::File::for_path(&dir)
                        .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
                        .inspect_err(|e| {
                            glib::g_warning!("Ptyxis", "Cannot watch {}: {e}", dir.display())
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
            default_profile: None,
            default_binder: binder,
            custom_links,
            opacity_group,
            palette_flow,
            palette_previews: Vec::new(),
            show_all_palettes: false,
            show_more,
            profiles_list,
            profile_editor: None,
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
                    glib::g_warning!("Ptyxis", "Failed to add profile: {e}");
                }
            }
            PreferencesInput::EditProfile(uuid) => self.edit_profile(root, &uuid),
            PreferencesInput::SelectFont => self.select_font(root),
        }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        if let Some(source) = self.palette_reload.take() {
            source.remove();
        }
        for (object, id) in self.shared_handlers.drain(..) {
            object.disconnect(id);
        }
    }
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

/// "Restore Session" and the "Restore Window Size" expander.
fn window_group(settings: &gio::Settings) -> adw::PreferencesGroup {
    let restore_session = widgets::switch_row(
        "Restore Session",
        Some("Attempt to restore previous tabs when Ptyxis starts"),
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
            glib::g_warning!("Ptyxis", "Cannot create {}: {e}", dir.display());
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
                        glib::g_warning!("Ptyxis", "Failed to import palette: {e}");
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
