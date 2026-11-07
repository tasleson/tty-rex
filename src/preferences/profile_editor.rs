// preferences/profile_editor.rs
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

//! The "Edit Profile" subpage (`src/ptyxis-profile-editor.c` / `.ui`).

use adw::prelude::*;
use gtk::{gio, glib};
use relm4::prelude::*;

use super::custom_links::CustomLinksGroup;
use super::widgets;
use crate::choices;
use crate::container::{Containers, ListenerId};
use crate::palette;
use crate::profile::{self, Profile, Profiles, key};

pub struct ProfileEditorInit {
    pub profile: Profile,
    pub profiles: Profiles,
    pub containers: Containers,
}

#[derive(Debug)]
pub enum ProfileEditorInput {
    CopyUuid,
    Delete,
    /// User palettes changed; refresh the palette choices.
    RefreshPalettes,
    /// Containers came or went; refresh the default container choices.
    RefreshContainers,
}

pub struct ProfileEditor {
    profile: Profile,
    palette_row: adw::ComboRow,
    containers_row: adw::ComboRow,
    profiles: Profiles,
    containers: Containers,
    containers_listener: ListenerId,
    _custom_links: CustomLinksGroup,
}

impl Component for ProfileEditor {
    type Init = ProfileEditorInit;
    type Input = ProfileEditorInput;
    type Output = ();
    type CommandOutput = ();
    type Root = adw::NavigationPage;
    type Widgets = ();

    fn init_root() -> Self::Root {
        adw::NavigationPage::builder().title("Edit Profile").build()
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let settings = init.profile.gsettings().clone();
        let mut binder = widgets::Binder::default();
        let page = widgets::page("Profile", "");

        let label = adw::EntryRow::builder().title("Name").build();
        settings
            .bind(key::LABEL, &label, "text")
            .mapping(|variant, _| Some(profile::display_label(variant.str()?).to_value()))
            .build();
        let uuid_row = adw::ActionRow::builder()
            .title("Identifier")
            .subtitle(init.profile.uuid())
            .css_classes(["property"])
            .build();
        uuid_row.add_suffix(
            &gtk::Button::builder()
                .action_name("uuid.copy")
                .icon_name("edit-copy-symbolic")
                .tooltip_text("Copy")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build(),
        );
        let group = widgets::group(None);
        group.add(&label);
        group.add(&uuid_row);
        page.add(&group);

        let palette_row = adw::ComboRow::builder().title("Palette").build();
        bind_palettes(&settings, &palette_row);
        let (opacity_row, opacity, _) = widgets::opacity_row();
        // Hidden in the C template too; kept so the key stays editable once
        // window transparency is supported.
        opacity_row.set_visible(false);
        widgets::bind(&settings, key::OPACITY, &opacity, "value");
        let group = widgets::group(Some("Color"));
        group.add(&palette_row);
        group.add(&opacity_row);
        page.add(&group);

        let bold = widgets::switch_row("Use Bright Colors for Bold", None);
        widgets::bind(&settings, key::BOLD_IS_BRIGHT, &bold, "active");
        let group = widgets::group(None);
        group.add(&bold);
        page.add(&group);

        page.add(&spacing_group(&mut binder));

        let proxy = widgets::switch_row(
            "Proxy",
            Some("Applies the network proxy settings to the container environment"),
        );
        widgets::bind(&settings, key::USE_PROXY, &proxy, "active");
        let group = widgets::group(Some("Network"));
        group.add(&proxy);
        page.add(&group);

        let use_custom_command = widgets::switch_row("Use Custom Command", None);
        let custom_command = adw::EntryRow::builder().title("Custom Command").build();
        widgets::bind(
            &settings,
            key::USE_CUSTOM_COMMAND,
            &use_custom_command,
            "active",
        );
        widgets::bind(&settings, key::CUSTOM_COMMAND, &custom_command, "text");
        use_custom_command
            .bind_property("active", &custom_command, "sensitive")
            .sync_create()
            .build();
        let group = widgets::group(Some("Shell"));
        group.add(&use_custom_command);
        group.add(&custom_command);
        page.add(&group);

        page.add(&login_shell_group(&mut binder, None));

        let containers = adw::ComboRow::builder()
            .title("Default Container")
            .subtitle("The default container to use with this profile")
            .build();
        bind_containers(&settings, &containers, &init.containers);
        let preserve_container = widgets::combo_row(
            "Preserve Container",
            Some("Preserve the current container when opening new tabs and windows"),
            choices::PRESERVE_CONTAINERS,
        );
        widgets::bind_choice(
            &settings,
            key::PRESERVE_CONTAINER,
            &preserve_container,
            choices::PRESERVE_CONTAINERS,
        );
        let group = widgets::group(None);
        group.add(&containers);
        group.add(&preserve_container);
        page.add(&group);

        page.add(&directory_and_exit_group(&mut binder));
        page.add(&scrolling_group(&mut binder));
        page.add(&scrollback_group(&mut binder));
        page.add(&compatibility_group(&mut binder));
        binder.apply(&settings);

        let custom_links = CustomLinksGroup::new(&init.profile);
        page.add(custom_links.widget());

        let remove = adw::ButtonRow::builder()
            .title("Remove Profile")
            .action_name("profile.delete")
            .css_classes(["destructive-action"])
            .build();
        let group = widgets::group(None);
        group.add(&remove);
        page.add(&group);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        root.set_child(Some(&toolbar));

        let input = sender.input_sender();
        widgets::install_action(&root, "uuid", "copy", input, || {
            ProfileEditorInput::CopyUuid
        });
        widgets::install_action(&root, "profile", "delete", input, || {
            ProfileEditorInput::Delete
        });

        let input = sender.input_sender().clone();
        let containers_listener = init
            .containers
            .connect_changed(move || input.emit(ProfileEditorInput::RefreshContainers));

        let model = ProfileEditor {
            profile: init.profile,
            palette_row,
            containers_row: containers,
            profiles: init.profiles,
            containers: init.containers,
            containers_listener,
            _custom_links: custom_links,
        };
        ComponentParts { model, widgets: () }
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.containers.disconnect(self.containers_listener);
    }

    fn update(&mut self, message: Self::Input, _sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            ProfileEditorInput::RefreshPalettes => {
                gio::Settings::unbind(&self.palette_row, "selected");
                bind_palettes(self.profile.gsettings(), &self.palette_row);
            }
            ProfileEditorInput::RefreshContainers => {
                gio::Settings::unbind(&self.containers_row, "selected");
                bind_containers(
                    self.profile.gsettings(),
                    &self.containers_row,
                    &self.containers,
                );
            }
            ProfileEditorInput::CopyUuid => {
                root.clipboard().set_text(self.profile.uuid());
                let toast = adw::Toast::new("Copied to clipboard");
                toast.set_timeout(widgets::COPIED_TOAST_TIMEOUT_SECS);
                widgets::add_toast(root, toast);
            }
            ProfileEditorInput::Delete => {
                let toast = removal_toast(&self.profiles, &self.profile);
                if let Err(e) = self.profiles.remove(self.profile.uuid()) {
                    glib::g_warning!("tty-rex", "Failed to remove profile: {e}");
                    return;
                }
                widgets::add_toast(root, toast);
                widgets::pop_subpage(root);
            }
        }
    }
}

/// Offers the known containers as the profile's default container. A
/// default that is not around (yet) is listed by its id so the row still
/// shows it instead of silently showing another one.
fn bind_containers(settings: &gio::Settings, row: &adw::ComboRow, containers: &Containers) {
    let mut entries: Vec<(String, String)> = containers
        .list()
        .iter()
        .map(|c| (c.title().to_owned(), c.id().to_owned()))
        .collect();
    let current = settings.string(key::DEFAULT_CONTAINER);
    if !current.is_empty() && !entries.iter().any(|(_, id)| *id == current) {
        entries.push((current.to_string(), current.into()));
    }
    let titles: Vec<&str> = entries.iter().map(|(title, _)| title.as_str()).collect();
    widgets::bind_values(
        settings,
        key::DEFAULT_CONTAINER,
        row,
        &titles,
        entries.iter().map(|(_, id)| id.clone()).collect(),
    );
}

/// Offers every palette in the profile's palette combo row.
fn bind_palettes(settings: &gio::Settings, row: &adw::ComboRow) {
    let palettes = palette::all();
    let titles: Vec<&str> = palettes.iter().map(|p| p.name()).collect();
    widgets::bind_values(
        settings,
        key::PALETTE,
        row,
        &titles,
        palettes.iter().map(|p| p.id().to_owned()).collect(),
    );
}

/// "Removed profile “X”" with an undo button that re-adds it.
pub fn removal_toast(profiles: &Profiles, profile: &Profile) -> adw::Toast {
    let profiles = profiles.clone();
    let uuid = profile.uuid().to_owned();
    widgets::undo_toast(
        &format!("Removed profile “{}”", profile.label()),
        move || {
            if let Err(e) = profiles.add(&uuid) {
                glib::g_warning!("tty-rex", "Failed to restore profile: {e}");
            }
        },
    )
}

/// Groups shared by the profile editor and the preferences Behavior page
/// (which edits the default profile).
pub fn scrolling_group(binder: &mut widgets::Binder) -> adw::PreferencesGroup {
    let on_keystroke = widgets::switch_row(
        "Scroll on Keystroke",
        Some("Scroll to the bottom upon input keystroke"),
    );
    let on_output = widgets::switch_row(
        "Scroll on Output",
        Some("Scroll to the bottom when new content is available"),
    );
    binder.bind(key::SCROLL_ON_KEYSTROKE, &on_keystroke, "active");
    binder.bind(key::SCROLL_ON_OUTPUT, &on_output, "active");
    let group = widgets::group(Some("Scrolling"));
    group.add(&on_keystroke);
    group.add(&on_output);
    group
}

pub fn scrollback_group(binder: &mut widgets::Binder) -> adw::PreferencesGroup {
    let limit = widgets::switch_row(
        "Limit Scrollback",
        Some("Restrict the number of lines saved for scrollback"),
    );
    let lines = widgets::spin_row(
        "Scrollback Lines",
        Some("The number of lines to keep for scrollback"),
        widgets::SCROLLBACK_RANGE,
    );
    binder.bind(key::LIMIT_SCROLLBACK, &limit, "active");
    binder.bind(key::SCROLLBACK_LINES, &lines, "value");
    limit
        .bind_property("active", &lines, "sensitive")
        .sync_create()
        .build();
    let group = widgets::group(None);
    group.add(&limit);
    group.add(&lines);
    group
}

pub fn spacing_group(binder: &mut widgets::Binder) -> adw::PreferencesGroup {
    let line_spacing = widgets::spin_row("Line Spacing", None, widgets::SPACING_RANGE);
    let column_spacing = widgets::spin_row("Column Spacing", None, widgets::SPACING_RANGE);
    binder.bind(key::CELL_HEIGHT_SCALE, &line_spacing, "value");
    binder.bind(key::CELL_WIDTH_SCALE, &column_spacing, "value");
    let group = widgets::group(None);
    group.add(&line_spacing);
    group.add(&column_spacing);
    group
}

pub fn login_shell_group(
    binder: &mut widgets::Binder,
    title: Option<&str>,
) -> adw::PreferencesGroup {
    let login_shell = widgets::switch_row("Use Login Shell", None);
    binder.bind(key::LOGIN_SHELL, &login_shell, "active");
    let group = widgets::group(title);
    group.add(&login_shell);
    group
}

pub fn directory_and_exit_group(binder: &mut widgets::Binder) -> adw::PreferencesGroup {
    let preserve_directory = widgets::combo_row(
        "Preserve Working Directory",
        None,
        choices::PRESERVE_DIRECTORIES,
    );
    let exit_action = widgets::combo_row("Exit Action", None, choices::EXIT_ACTIONS);
    binder.bind_choice(
        key::PRESERVE_DIRECTORY,
        &preserve_directory,
        choices::PRESERVE_DIRECTORIES,
    );
    binder.bind_choice(key::EXIT_ACTION, &exit_action, choices::EXIT_ACTIONS);
    let group = widgets::group(None);
    group.add(&preserve_directory);
    group.add(&exit_action);
    group
}

pub fn compatibility_group(binder: &mut widgets::Binder) -> adw::PreferencesGroup {
    let backspace = widgets::combo_row(
        "Backspace Key",
        Some("Sequence to send for Backspace key"),
        choices::ERASE_BINDINGS,
    );
    let delete = widgets::combo_row(
        "Delete Key",
        Some("Sequence to send for Delete key"),
        choices::ERASE_BINDINGS,
    );
    let cjk = widgets::combo_row(
        "Ambiguous-width Characters",
        None,
        choices::CJK_AMBIGUOUS_WIDTHS,
    );
    binder.bind_choice(key::BACKSPACE_BINDING, &backspace, choices::ERASE_BINDINGS);
    binder.bind_choice(key::DELETE_BINDING, &delete, choices::ERASE_BINDINGS);
    binder.bind_choice(
        key::CJK_AMBIGUOUS_WIDTH,
        &cjk,
        choices::CJK_AMBIGUOUS_WIDTHS,
    );
    let group = widgets::group(Some("Compatibility"));
    group.add(&backspace);
    group.add(&delete);
    group.add(&cjk);
    group
}
