// preferences/custom_links.rs
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

//! The "Custom Links" group and its editor page
//! (`src/ptyxis-custom-link-row.c`, `src/ptyxis-custom-link-editor.c`).
//!
//! Several groups can show the same profile (the Behavior page edits the
//! default profile, which may also be open in the profile editor) and the
//! C application may edit it concurrently, so the settings key is the
//! source of truth: every group follows `changed::custom-links`, and
//! writes are based on the links last read from it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};

use super::widgets;
use crate::profile::{self, CustomLink, Profile};

const GROUP_ACTIONS: &str = "custom-link";
const ROW_ACTIONS: &str = "custom_link";

/// Identifies a link for as long as it is shown, independent of its index.
pub type LinkId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: LinkId,
    link: CustomLink,
    /// False for a link added in the UI but not edited yet; like the C
    /// version such links are not written to settings.
    saved: bool,
}

/// The links of one group, keyed by stable ids.
#[derive(Debug, Default)]
pub struct LinkList {
    entries: Vec<Entry>,
    next_id: LinkId,
}

impl LinkList {
    pub fn new(links: Vec<CustomLink>) -> Self {
        let mut list = Self::default();
        list.replace(links);
        list
    }

    fn fresh_id(&mut self) -> LinkId {
        self.next_id += 1;
        self.next_id
    }

    fn replace(&mut self, links: Vec<CustomLink>) {
        self.entries = links
            .into_iter()
            .map(|link| Entry {
                id: self.fresh_id(),
                link,
                saved: true,
            })
            .collect();
    }

    /// The links that belong in settings.
    pub fn saved_links(&self) -> Vec<CustomLink> {
        self.entries
            .iter()
            .filter(|e| e.saved)
            .map(|e| e.link.clone())
            .collect()
    }

    pub fn ids(&self) -> Vec<LinkId> {
        self.entries.iter().map(|e| e.id).collect()
    }

    pub fn get(&self, id: LinkId) -> Option<&CustomLink> {
        self.entries.iter().find(|e| e.id == id).map(|e| &e.link)
    }

    fn index_of(&self, id: LinkId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }

    /// Adopts links read from settings. Returns false (keeping ids, and so
    /// open editors) when they already match what this list would save.
    /// Unsaved new links are kept at the end.
    pub fn sync(&mut self, links: Vec<CustomLink>) -> bool {
        if links == self.saved_links() {
            return false;
        }
        let unsaved: Vec<Entry> = self.entries.drain(..).filter(|e| !e.saved).collect();
        self.replace(links);
        self.entries.extend(unsaved);
        true
    }

    pub fn add_unsaved(&mut self) -> LinkId {
        let id = self.fresh_id();
        self.entries.push(Entry {
            id,
            link: CustomLink::default(),
            saved: false,
        });
        id
    }

    /// Updates a link; it becomes saved from now on.
    pub fn update(&mut self, id: LinkId, link: CustomLink) -> bool {
        match self.entries.iter_mut().find(|e| e.id == id) {
            Some(entry) => {
                entry.link = link;
                entry.saved = true;
                true
            }
            None => false,
        }
    }

    /// Removes a link, returning its position for undo.
    pub fn remove(&mut self, id: LinkId) -> Option<(usize, CustomLink)> {
        let index = self.index_of(id)?;
        Some((index, self.entries.remove(index).link))
    }

    /// Re-inserts a removed link at `index` (clamped), as a saved link.
    pub fn insert(&mut self, index: usize, link: CustomLink) -> LinkId {
        let id = self.fresh_id();
        let index = index.min(self.entries.len());
        self.entries.insert(
            index,
            Entry {
                id,
                link,
                saved: true,
            },
        );
        id
    }
}

struct State {
    profile: Profile,
    links: LinkList,
    rows: HashMap<LinkId, adw::ActionRow>,
    handler: Option<glib::SignalHandlerId>,
}

impl State {
    fn disconnect(&mut self) {
        if let Some(id) = self.handler.take() {
            self.profile.gsettings().disconnect(id);
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// The custom links list for one profile.
#[derive(Clone)]
pub struct CustomLinksGroup {
    group: adw::PreferencesGroup,
    list_box: gtk::ListBox,
    state: Rc<RefCell<State>>,
}

impl CustomLinksGroup {
    pub fn new(profile: &Profile) -> Self {
        let group = widgets::group(Some("Custom Links"));
        let list_box = widgets::boxed_list();
        group.add(&list_box);
        let this = Self {
            group,
            list_box,
            state: Rc::new(RefCell::new(State {
                profile: profile.clone(),
                links: LinkList::new(profile.custom_links()),
                rows: HashMap::new(),
                handler: None,
            })),
        };
        this.watch();

        let actions = gio::SimpleActionGroup::new();
        let add = gio::SimpleAction::new("add", None);
        let weak = this.downgrade();
        add.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.add_link();
            }
        });
        actions.add_action(&add);
        this.group
            .insert_action_group(GROUP_ACTIONS, Some(&actions));
        this.rebuild();
        this
    }

    pub fn widget(&self) -> &adw::PreferencesGroup {
        &self.group
    }

    /// Switches to another profile's links (when the default changes).
    pub fn set_profile(&self, profile: &Profile) {
        {
            let mut state = self.state.borrow_mut();
            state.disconnect();
            state.profile = profile.clone();
            state.links = LinkList::new(profile.custom_links());
        }
        self.watch();
        self.rebuild();
    }

    fn watch(&self) {
        let weak = self.downgrade();
        let id = self.state.borrow().profile.gsettings().connect_changed(
            Some(profile::key::CUSTOM_LINKS),
            move |_, _| {
                if let Some(this) = weak.upgrade() {
                    this.reload();
                }
            },
        );
        self.state.borrow_mut().handler = Some(id);
    }

    fn reload(&self) {
        let changed = {
            let mut state = self.state.borrow_mut();
            let links = state.profile.custom_links();
            state.links.sync(links)
        };
        if changed {
            self.rebuild();
        }
    }

    fn downgrade(&self) -> WeakGroup {
        WeakGroup {
            group: self.group.downgrade(),
            list_box: self.list_box.downgrade(),
            state: Rc::downgrade(&self.state),
        }
    }

    fn save(&self) {
        let (profile, links) = {
            let state = self.state.borrow();
            (state.profile.clone(), state.links.saved_links())
        };
        // Writing emits changed::custom-links, which syncs (as a no-op).
        if let Err(e) = profile.set_custom_links(&links) {
            glib::g_warning!("tty-rex", "Failed to save custom links: {e}");
        }
    }

    fn rebuild(&self) {
        self.list_box.remove_all();
        let ids = self.state.borrow().links.ids();
        let mut rows = HashMap::new();
        for id in ids {
            let row = self.link_row(id);
            self.list_box.append(&row);
            rows.insert(id, row);
        }
        self.state.borrow_mut().rows = rows;
        self.list_box.append(&widgets::add_button_row(
            "Add Link",
            &format!("{GROUP_ACTIONS}.add"),
        ));
    }

    fn link_row(&self, id: LinkId) -> adw::ActionRow {
        let link = self
            .state
            .borrow()
            .links
            .get(id)
            .cloned()
            .unwrap_or_default();
        let row = adw::ActionRow::builder()
            .title(&link.pattern)
            .subtitle(&link.target)
            .activatable(true)
            .build();
        let menu = gio::Menu::new();
        menu.append(Some("_Edit…"), Some(&format!("{ROW_ACTIONS}.edit")));
        menu.append(Some("_Remove"), Some(&format!("{ROW_ACTIONS}.remove")));
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
        let edit = gio::SimpleAction::new("edit", None);
        let weak = self.downgrade();
        let row_ref = row.downgrade();
        edit.connect_activate(move |_, _| {
            if let (Some(this), Some(row)) = (weak.upgrade(), row_ref.upgrade()) {
                widgets::push_subpage(&row, &this.editor(id), false);
            }
        });
        actions.add_action(&edit);
        let remove = gio::SimpleAction::new("remove", None);
        let weak = self.downgrade();
        remove.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.remove_link(id);
            }
        });
        actions.add_action(&remove);
        row.insert_action_group(ROW_ACTIONS, Some(&actions));
        row
    }

    fn add_link(&self) {
        let id = self.state.borrow_mut().links.add_unsaved();
        self.rebuild();
        widgets::push_subpage(&self.group, &self.editor(id), false);
    }

    fn remove_link(&self, id: LinkId) {
        let Some((index, link)) = self.state.borrow_mut().links.remove(id) else {
            return;
        };
        self.save();
        self.rebuild();

        let weak = self.downgrade();
        let title = format!("Removed custom link “{}”", link.pattern);
        let toast = widgets::undo_toast(&title, move || {
            if let Some(this) = weak.upgrade() {
                this.state.borrow_mut().links.insert(index, link.clone());
                this.save();
                this.rebuild();
            }
        });
        widgets::add_toast(&self.group, toast);
    }

    /// Applies an edit: updates the stored link and that row in place.
    fn edit_link(&self, id: LinkId, link: CustomLink) {
        let row = {
            let mut state = self.state.borrow_mut();
            if !state.links.update(id, link.clone()) {
                return;
            }
            state.rows.get(&id).cloned()
        };
        self.save();
        if let Some(row) = row {
            row.set_title(&link.pattern);
            row.set_subtitle(&link.target);
        }
    }

    /// The "Edit Custom Link" page; edits are saved as they happen.
    fn editor(&self, id: LinkId) -> adw::NavigationPage {
        let link = self
            .state
            .borrow()
            .links
            .get(id)
            .cloned()
            .unwrap_or_default();
        let pattern = adw::EntryRow::builder()
            .title("Regex Pattern")
            .text(&link.pattern)
            .build();
        let target = adw::EntryRow::builder()
            .title("Target URL")
            .text(&link.target)
            .build();

        let on_change = {
            let weak = self.downgrade();
            let pattern = pattern.downgrade();
            let target = target.downgrade();
            move || {
                let (Some(this), Some(pattern), Some(target)) =
                    (weak.upgrade(), pattern.upgrade(), target.upgrade())
                else {
                    return;
                };
                this.edit_link(
                    id,
                    CustomLink {
                        pattern: pattern.text().into(),
                        target: target.text().into(),
                    },
                );
            }
        };
        let on_change = Rc::new(on_change);
        let changed = Rc::clone(&on_change);
        pattern.connect_changed(move |_| changed());
        target.connect_changed(move |_| on_change());

        let group = widgets::group(None);
        group.add(&pattern);
        group.add(&target);
        let page = widgets::page("Custom Link", "");
        page.add(&group);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        adw::NavigationPage::builder()
            .title("Edit Custom Link")
            .child(&toolbar)
            .build()
    }
}

struct WeakGroup {
    group: glib::WeakRef<adw::PreferencesGroup>,
    list_box: glib::WeakRef<gtk::ListBox>,
    state: Weak<RefCell<State>>,
}

impl WeakGroup {
    fn upgrade(&self) -> Option<CustomLinksGroup> {
        Some(CustomLinksGroup {
            group: self.group.upgrade()?,
            list_box: self.list_box.upgrade()?,
            state: self.state.upgrade()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(pattern: &str) -> CustomLink {
        CustomLink {
            pattern: pattern.into(),
            target: String::new(),
        }
    }

    fn patterns(links: &[CustomLink]) -> Vec<&str> {
        links.iter().map(|l| l.pattern.as_str()).collect()
    }

    #[test]
    fn undo_reinserts_at_original_position() {
        let cases = [
            (0, ["x", "a", "b"]),
            (1, ["a", "x", "b"]),
            (2, ["a", "b", "x"]),
            (9, ["a", "b", "x"]),
        ];
        for (index, expected) in cases {
            let mut list = LinkList::new(vec![link("a"), link("b")]);
            list.insert(index, link("x"));
            assert_eq!(patterns(&list.saved_links()), expected, "{index}");
        }
    }

    #[test]
    fn ids_survive_removal_of_other_links() {
        let mut list = LinkList::new(vec![link("a"), link("b"), link("c")]);
        let [a, b, c] = list.ids()[..] else { panic!() };
        assert_eq!(list.remove(a), Some((0, link("a"))));
        // `c` is still addressed by its id even though its index moved.
        assert!(list.update(c, link("C")));
        assert_eq!(list.get(b), Some(&link("b")));
        assert_eq!(patterns(&list.saved_links()), ["b", "C"]);
        assert_eq!(list.remove(a), None);
    }

    #[test]
    fn new_links_are_saved_only_once_edited() {
        let mut list = LinkList::new(vec![link("a")]);
        let new = list.add_unsaved();
        assert_eq!(patterns(&list.saved_links()), ["a"]);
        assert!(list.update(new, link("n")));
        assert_eq!(patterns(&list.saved_links()), ["a", "n"]);
    }

    #[test]
    fn sync_follows_external_changes() {
        let mut list = LinkList::new(vec![link("a")]);
        let a = list.ids()[0];
        let pending = list.add_unsaved();

        // Our own write echoing back changes nothing and keeps ids.
        assert!(!list.sync(vec![link("a")]));
        assert_eq!(list.ids(), [a, pending]);

        // Another editor (or C Ptyxis) changed the key: adopt it, keep
        // the unsaved link, and later saves start from the new value.
        assert!(list.sync(vec![link("x"), link("y")]));
        assert_eq!(patterns(&list.saved_links()), ["x", "y"]);
        assert_eq!(list.ids().last(), Some(&pending));
        let x = list.ids()[0];
        assert!(list.update(x, link("X")));
        assert_eq!(patterns(&list.saved_links()), ["X", "y"]);
    }

    /// Two groups on one profile (Behavior page and profile editor) stay
    /// consistent through the settings key instead of clobbering it.
    #[test]
    fn writes_start_from_the_settings_value() {
        if crate::settings::tests::expected_schema(crate::config::SCHEMA_ID).is_none() {
            return;
        }
        crate::settings::use_memory_backend();
        let profile = Profile::open("custom-links-test").unwrap();
        let mut first = LinkList::new(profile.custom_links());
        let mut second = LinkList::new(profile.custom_links());

        let id = first.add_unsaved();
        first.update(id, link("from-first"));
        profile.set_custom_links(&first.saved_links()).unwrap();

        // The second group re-reads on change before it writes.
        assert!(second.sync(profile.custom_links()));
        let id = second.add_unsaved();
        second.update(id, link("from-second"));
        profile.set_custom_links(&second.saved_links()).unwrap();
        assert_eq!(
            patterns(&profile.custom_links()),
            ["from-first", "from-second"]
        );
    }
}
