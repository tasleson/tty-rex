// choices.rs
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

//! Titles for enum-valued settings shown as combo rows in preferences,
//! in the order of the `GListStore`s in `src/ptyxis-preferences-window.ui`
//! and `src/ptyxis-profile-editor.ui`.

/// A combo row entry: displayed title and schema enum nick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub title: &'static str,
    pub nick: &'static str,
}

const fn choice(title: &'static str, nick: &'static str) -> Choice {
    Choice { title, nick }
}

pub const TEXT_BLINK_MODES: &[Choice] = &[
    choice("Never", "never"),
    choice("Focused", "focused"),
    choice("Unfocused", "unfocused"),
    choice("Always", "always"),
];

pub const CURSOR_SHAPES: &[Choice] = &[
    choice("Block", "block"),
    choice("I-Beam", "ibeam"),
    choice("Underline", "underline"),
];

pub const CURSOR_BLINK_MODES: &[Choice] = &[
    choice("Follow System", "system"),
    choice("Enabled", "on"),
    choice("Disabled", "off"),
];

pub const TAB_POSITIONS: &[Choice] = &[choice("Next", "next"), choice("Last", "last")];

pub const SCROLLBAR_POLICIES: &[Choice] = &[
    choice("Never", "never"),
    choice("Follow System", "system"),
    choice("Always", "always"),
];

pub const PRESERVE_DIRECTORIES: &[Choice] = &[
    choice("Never", "never"),
    choice("Safe", "safe"),
    choice("Always", "always"),
];

pub const PRESERVE_CONTAINERS: &[Choice] = &[choice("Never", "never"), choice("Always", "always")];

pub const EXIT_ACTIONS: &[Choice] = &[
    choice("No Action", "none"),
    choice("Restart", "restart"),
    choice("Close", "close"),
];

pub const CJK_AMBIGUOUS_WIDTHS: &[Choice] = &[choice("Narrow", "narrow"), choice("Wide", "wide")];

pub const ERASE_BINDINGS: &[Choice] = &[
    choice("Automatic", "auto"),
    choice("Control-H", "ascii-backspace"),
    choice("ASCII DEL", "ascii-delete"),
    choice("Escape Sequence", "delete-sequence"),
    choice("TTY Erase", "tty"),
];

/// Position of `nick` in `choices` (`string_to_index()` in the C code).
pub fn index_of(choices: &[Choice], nick: &str) -> Option<u32> {
    choices
        .iter()
        .position(|c| c.nick == nick)
        .and_then(|i| u32::try_from(i).ok())
}

/// Nick at `index` (`index_to_string()` in the C code).
pub fn nick_at(choices: &[Choice], index: u32) -> Option<&'static str> {
    choices.get(usize::try_from(index).ok()?).map(|c| c.nick)
}

/// Titles for a `gtk::StringList` model.
pub fn titles(choices: &[Choice]) -> Vec<&'static str> {
    choices.iter().map(|c| c.title).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::schema_template;

    #[test]
    fn choices_cover_schema_enums() {
        let cases: &[(&[Choice], &str)] = &[
            (TEXT_BLINK_MODES, "TextBlinkMode"),
            (CURSOR_SHAPES, "CursorShape"),
            (CURSOR_BLINK_MODES, "CursorBlinkMode"),
            (TAB_POSITIONS, "NewTabPosition"),
            (SCROLLBAR_POLICIES, "ScrollbarPolicy"),
            (PRESERVE_DIRECTORIES, "PreserveDirectory"),
            (PRESERVE_CONTAINERS, "PreserveContainer"),
            (EXIT_ACTIONS, "ExitAction"),
            (CJK_AMBIGUOUS_WIDTHS, "CjkAmbiguousWidth"),
            (ERASE_BINDINGS, "EraseBinding"),
        ];
        for (choices, schema_enum) in cases {
            let block = schema_template::enum_block(schema_enum);
            assert_eq!(
                block.matches("nick=\"").count(),
                choices.len(),
                "{schema_enum}"
            );
            for c in *choices {
                assert!(
                    block.contains(&format!("nick=\"{}\"", c.nick)),
                    "{schema_enum}: {}",
                    c.nick
                );
            }
        }
    }

    #[test]
    fn index_mapping() {
        assert_eq!(index_of(EXIT_ACTIONS, "restart"), Some(1));
        assert_eq!(index_of(EXIT_ACTIONS, "bogus"), None);
        assert_eq!(nick_at(EXIT_ACTIONS, 2), Some("close"));
        assert_eq!(nick_at(EXIT_ACTIONS, 3), None);
        assert_eq!(titles(TAB_POSITIONS), ["Next", "Last"]);
    }
}
