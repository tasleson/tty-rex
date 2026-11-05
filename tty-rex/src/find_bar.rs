// find_bar.rs
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

//! Scrollback search bar (`src/ptyxis-find-bar.c` / `.ui`).

use gtk::glib;
use gtk::prelude::*;
use relm4::prelude::*;
use vte::prelude::*;

/// PCRE2 compile flags understood by `vte_regex_new_for_search()`; VTE
/// does not export them, so these mirror `src/ptyxis-util.h`.
pub const PCRE2_CASELESS: u32 = 0x0000_0008;
pub const PCRE2_MULTILINE: u32 = 0x0000_0400;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub match_case: bool,
    pub whole_words: bool,
    pub use_regex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub pattern: String,
    pub flags: u32,
}

/// Builds the VTE search pattern and flags as
/// `ptyxis_find_bar_get_search()` does. Empty text clears the search.
pub fn build_query(text: &str, options: SearchOptions) -> Option<SearchQuery> {
    if text.is_empty() {
        return None;
    }

    let mut flags = PCRE2_MULTILINE;
    if !options.match_case {
        flags |= PCRE2_CASELESS;
    }
    let mut pattern = if options.use_regex {
        text.to_owned()
    } else {
        glib::Regex::escape_string(text).to_string()
    };
    if options.whole_words {
        pattern = format!("\\b{pattern}\\b");
    }
    Some(SearchQuery { pattern, flags })
}

#[derive(Debug)]
pub enum FindBarInput {
    /// Retargets the bar; clears the current query like the C version.
    SetTerminal(Option<vte::Terminal>),
    QueryChanged,
    Next,
    Previous,
    Dismiss,
    Focus,
}

#[derive(Debug)]
pub enum FindBarOutput {
    Dismissed,
}

pub struct FindBar {
    terminal: Option<vte::Terminal>,
    entry: gtk::Entry,
    match_case: gtk::CheckButton,
    whole_words: gtk::CheckButton,
    use_regex: gtk::CheckButton,
}

impl FindBar {
    fn options(&self) -> SearchOptions {
        SearchOptions {
            match_case: self.match_case.is_active(),
            whole_words: self.whole_words.is_active(),
            use_regex: self.use_regex.is_active(),
        }
    }

    fn apply_query(&self) {
        let Some(terminal) = &self.terminal else {
            return;
        };
        let regex = build_query(&self.entry.text(), self.options()).and_then(|query| {
            vte::Regex::for_search(&query.pattern, query.flags)
                .inspect_err(|_| self.entry.add_css_class("error"))
                .ok()
        });
        if regex.is_some() || self.entry.text().is_empty() {
            self.entry.remove_css_class("error");
        }
        terminal.search_set_regex(regex.as_ref(), 0);
        terminal.search_set_wrap_around(true);
    }
}

/// Constructor for a message emitted by a button or shortcut.
type MessageFn = fn() -> FindBarInput;

fn shortcut(
    trigger: &str,
    input: &relm4::Sender<FindBarInput>,
    message: MessageFn,
) -> gtk::Shortcut {
    let input = input.clone();
    gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string(trigger),
        Some(gtk::CallbackAction::new(move |_, _| {
            input.emit(message());
            glib::Propagation::Stop
        })),
    )
}

impl Component for FindBar {
    type Init = ();
    type Input = FindBarInput;
    type Output = FindBarOutput;
    type CommandOutput = ();
    type Root = gtk::Box;
    type Widgets = ();

    fn init_root() -> Self::Root {
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(9)
            .css_name("findbar")
            .build()
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let input = sender.input_sender();

        let entry = gtk::Entry::builder()
            .width_chars(10)
            .hexpand(true)
            .primary_icon_name("edit-find-symbolic")
            .placeholder_text("Search History")
            .build();
        // The terminal searches upward from the bottom, so Return goes up.
        let entry_shortcuts = gtk::ShortcutController::new();
        entry_shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
        entry_shortcuts.add_shortcut(shortcut("<shift>Return", input, || FindBarInput::Next));
        entry_shortcuts.add_shortcut(shortcut("Return", input, || FindBarInput::Previous));
        entry.add_controller(entry_shortcuts);
        let changed = input.clone();
        entry.connect_changed(move |_| changed.emit(FindBarInput::QueryChanged));

        let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        navigation.add_css_class("linked");
        let buttons: [(&str, &str, MessageFn); 2] = [
            ("go-up-symbolic", "Search Up", || FindBarInput::Previous),
            ("go-down-symbolic", "Search Down", || FindBarInput::Next),
        ];
        for (icon, tooltip, message) in buttons {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .build();
            let clicked = input.clone();
            button.connect_clicked(move |_| clicked.emit(message()));
            navigation.append(&button);
        }

        let check = |label: &str| {
            let button = gtk::CheckButton::builder()
                .label(label)
                .use_underline(true)
                .build();
            let toggled = input.clone();
            button.connect_toggled(move |_| toggled.emit(FindBarInput::QueryChanged));
            button
        };
        let match_case = check("Match _Case");
        let whole_words = check("Whole _Words");
        let use_regex = check("Use _Regular Expressions");
        let options = gtk::Box::new(gtk::Orientation::Vertical, 6);
        options.append(&match_case);
        options.append(&whole_words);
        options.append(&use_regex);
        let options_button = gtk::MenuButton::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("Search Options")
            .always_show_arrow(true)
            .direction(gtk::ArrowType::Up)
            .popover(&gtk::Popover::builder().child(&options).build())
            .build();

        let close_button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .margin_start(12)
            .valign(gtk::Align::Center)
            .css_classes(["close-button"])
            .tooltip_text("Close the search bar")
            .build();
        let dismiss = input.clone();
        close_button.connect_clicked(move |_| dismiss.emit(FindBarInput::Dismiss));

        root.append(&entry);
        root.append(&navigation);
        root.append(&options_button);
        root.append(&close_button);

        let bar_shortcuts = gtk::ShortcutController::new();
        bar_shortcuts.add_shortcut(shortcut("Escape", input, || FindBarInput::Dismiss));
        bar_shortcuts.add_shortcut(shortcut("<ctrl>g", input, || FindBarInput::Previous));
        bar_shortcuts.add_shortcut(shortcut("<ctrl><shift>g", input, || FindBarInput::Next));
        root.add_controller(bar_shortcuts);

        let model = FindBar {
            terminal: None,
            entry,
            match_case,
            whole_words,
            use_regex,
        };
        ComponentParts { model, widgets: () }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        match message {
            FindBarInput::SetTerminal(terminal) => {
                if self.terminal != terminal {
                    if let Some(old) = &self.terminal {
                        old.search_set_regex(None, 0);
                    }
                    self.terminal = terminal;
                    // Emits `changed`, which re-applies the (empty) query.
                    self.entry.set_text("");
                }
            }
            FindBarInput::QueryChanged => self.apply_query(),
            FindBarInput::Next => {
                if let Some(terminal) = &self.terminal {
                    terminal.search_find_next();
                }
            }
            FindBarInput::Previous => {
                if let Some(terminal) = &self.terminal {
                    terminal.search_find_previous();
                }
            }
            FindBarInput::Dismiss => {
                // Ignoring the error is fine: it only fails during shutdown.
                let _ = sender.output(FindBarOutput::Dismissed);
            }
            FindBarInput::Focus => {
                self.entry.grab_focus();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASELESS: u32 = PCRE2_MULTILINE | PCRE2_CASELESS;

    #[test]
    fn queries() {
        let opts = |match_case, whole_words, use_regex| SearchOptions {
            match_case,
            whole_words,
            use_regex,
        };
        let cases = [
            ("", opts(false, false, false), None),
            ("foo", opts(false, false, false), Some(("foo", CASELESS))),
            (
                "foo",
                opts(true, false, false),
                Some(("foo", PCRE2_MULTILINE)),
            ),
            (
                "a.b*",
                opts(false, false, false),
                Some(("a\\.b\\*", CASELESS)),
            ),
            ("a.b*", opts(false, false, true), Some(("a.b*", CASELESS))),
            (
                "foo",
                opts(false, true, false),
                Some(("\\bfoo\\b", CASELESS)),
            ),
            (
                "f(o)",
                opts(true, true, false),
                Some(("\\bf\\(o\\)\\b", PCRE2_MULTILINE)),
            ),
            (
                "f(o)",
                opts(true, true, true),
                Some(("\\bf(o)\\b", PCRE2_MULTILINE)),
            ),
        ];
        for (text, options, expected) in cases {
            let expected = expected.map(|(pattern, flags)| SearchQuery {
                pattern: pattern.to_owned(),
                flags,
            });
            assert_eq!(build_query(text, options), expected, "{text:?} {options:?}");
        }
    }
}
