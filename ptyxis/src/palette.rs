// palette.rs
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

//! Color palettes in the `.palette` key-file format of `src/palettes/`
//! (see `src/ptyxis-palette.c`).
//!
//! A palette either has `[Light]` and `[Dark]` groups, or puts a single
//! face directly in `[Palette]`, in which case it is used for both.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gtk::{gdk, glib};

use crate::config;

include!(concat!(env!("OUT_DIR"), "/builtin_palettes.rs"));

/// Palette used when a profile's palette cannot be found.
pub const DEFAULT_PALETTE_ID: &str = "gnome";
/// Number of indexed colors a palette must provide.
pub const N_INDEXED_COLORS: usize = 16;

const PALETTE_SUFFIX: &str = ".palette";
const GROUP_PALETTE: &str = "Palette";
const GROUP_LIGHT: &str = "Light";
const GROUP_DARK: &str = "Dark";
const KEY_NAME: &str = "Name";
const KEY_PRIMARY: &str = "Primary";
const KEY_FOREGROUND: &str = "Foreground";
const KEY_BACKGROUND: &str = "Background";
const KEY_CURSOR: &str = "Cursor";
const KEY_CURSOR_BACKGROUND: &str = "CursorBackground";
const KEY_CURSOR_FOREGROUND: &str = "CursorForeground";

#[derive(Debug, thiserror::Error)]
pub enum PaletteError {
    #[error("\"{source_name}\" is not a valid key file: {error}")]
    KeyFile {
        source_name: String,
        error: glib::Error,
    },
    #[error("\"{source_name}\" is missing {group} section")]
    MissingGroup { source_name: String, group: String },
    #[error("\"{source_name}\" is missing {key} key in {group} section")]
    MissingKey {
        source_name: String,
        group: String,
        key: String,
    },
    #[error("\"{value}\" is not a valid color for {key} in section {group} of \"{source_name}\"")]
    InvalidColor {
        source_name: String,
        group: String,
        key: String,
        value: String,
    },
}

/// The colors for one appearance (light or dark).
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub foreground: gdk::RGBA,
    pub background: gdk::RGBA,
    pub cursor_background: Option<gdk::RGBA>,
    pub cursor_foreground: Option<gdk::RGBA>,
    pub indexed: [gdk::RGBA; N_INDEXED_COLORS],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    id: String,
    name: String,
    primary: bool,
    has_light: bool,
    has_dark: bool,
    light: Face,
    dark: Face,
}

/// Perceived brightness check from <http://alienryderflex.com/hsp.html>,
/// identical to `is_dark()` in the C implementation.
pub fn is_dark(color: &gdk::RGBA) -> bool {
    let [r, g, b] = [color.red(), color.green(), color.blue()].map(|c| f64::from(c) * 255.0);
    let hsp = (0.299 * r * r + 0.587 * g * g + 0.114 * b * b).sqrt();
    hsp <= 127.5
}

struct FaceReader<'a> {
    key_file: &'a glib::KeyFile,
    source_name: &'a str,
    group: &'a str,
}

impl FaceReader<'_> {
    fn optional(&self, key: &str) -> Result<Option<gdk::RGBA>, PaletteError> {
        let Ok(value) = self.key_file.string(self.group, key) else {
            return Ok(None);
        };
        gdk::RGBA::parse(value.as_str())
            .map(Some)
            .map_err(|_| PaletteError::InvalidColor {
                source_name: self.source_name.to_owned(),
                group: self.group.to_owned(),
                key: key.to_owned(),
                value: value.to_string(),
            })
    }

    fn required(&self, key: &str) -> Result<gdk::RGBA, PaletteError> {
        self.optional(key)?.ok_or_else(|| PaletteError::MissingKey {
            source_name: self.source_name.to_owned(),
            group: self.group.to_owned(),
            key: key.to_owned(),
        })
    }

    fn read(&self) -> Result<Face, PaletteError> {
        if !self.key_file.has_group(self.group) {
            return Err(PaletteError::MissingGroup {
                source_name: self.source_name.to_owned(),
                group: self.group.to_owned(),
            });
        }

        let cursor_background = match self.optional(KEY_CURSOR_BACKGROUND)? {
            Some(color) => Some(color),
            None => self.optional(KEY_CURSOR)?,
        };
        let cursor_foreground = self.optional(KEY_CURSOR_FOREGROUND)?;
        let foreground = self.required(KEY_FOREGROUND)?;
        let background = self.required(KEY_BACKGROUND)?;

        let mut indexed = [gdk::RGBA::BLACK; N_INDEXED_COLORS];
        for (i, slot) in indexed.iter_mut().enumerate() {
            *slot = self.required(&format!("Color{i}"))?;
        }

        Ok(Face {
            foreground,
            background,
            cursor_background,
            cursor_foreground,
            indexed,
        })
    }
}

impl Palette {
    /// Parses palette `data`. `source_name` is only used in error messages.
    pub fn parse(id: &str, source_name: &str, data: &str) -> Result<Self, PaletteError> {
        let key_file = glib::KeyFile::new();
        key_file
            .load_from_data(data, glib::KeyFileFlags::NONE)
            .map_err(|error| PaletteError::KeyFile {
                source_name: source_name.to_owned(),
                error,
            })?;

        if !key_file.has_group(GROUP_PALETTE) {
            return Err(PaletteError::MissingGroup {
                source_name: source_name.to_owned(),
                group: GROUP_PALETTE.to_owned(),
            });
        }
        let name = key_file
            .string(GROUP_PALETTE, KEY_NAME)
            .map_err(|_| PaletteError::MissingKey {
                source_name: source_name.to_owned(),
                group: GROUP_PALETTE.to_owned(),
                key: KEY_NAME.to_owned(),
            })?
            .to_string();

        let reader = |group| FaceReader {
            key_file: &key_file,
            source_name,
            group,
        };

        let (light, dark, has_light, has_dark) =
            if !key_file.has_group(GROUP_LIGHT) && !key_file.has_group(GROUP_DARK) {
                let face = reader(GROUP_PALETTE).read()?;
                let dark = is_dark(&face.background);
                (face.clone(), face, !dark, dark)
            } else {
                (
                    reader(GROUP_LIGHT).read()?,
                    reader(GROUP_DARK).read()?,
                    true,
                    true,
                )
            };

        Ok(Self {
            id: id.to_owned(),
            name,
            primary: key_file
                .boolean(GROUP_PALETTE, KEY_PRIMARY)
                .unwrap_or(false),
            has_light,
            has_dark,
            light,
            dark,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_primary(&self) -> bool {
        self.primary
    }

    pub fn has_light(&self) -> bool {
        self.has_light
    }

    pub fn has_dark(&self) -> bool {
        self.has_dark
    }

    pub fn face(&self, dark: bool) -> &Face {
        if dark { &self.dark } else { &self.light }
    }
}

/// Application id of the stable build, whose settings the port shares.
const STABLE_APP_ID: &str = "org.gnome.Ptyxis";
const PALETTES_SUBDIR: &str = "palettes";

/// Directories searched for user-installed palettes, most specific first.
///
/// `ptyxis_get_user_palettes_dir()` uses `<data dir>/<APP_ID>/palettes`.
/// Because the port reads the stable `org.gnome.Ptyxis` settings even
/// when running under another application id (e.g. `.Devel`), profiles
/// may name palettes imported by the stable build, so its directory is
/// searched as well.
pub fn user_palettes_dirs(data_dir: &Path, app_id: &str) -> Vec<PathBuf> {
    let mut dirs = vec![data_dir.join(app_id).join(PALETTES_SUBDIR)];
    if app_id != STABLE_APP_ID {
        dirs.push(data_dir.join(STABLE_APP_ID).join(PALETTES_SUBDIR));
    }
    dirs
}

fn lookup_user(id: &str) -> Option<Palette> {
    user_palettes_dirs(&glib::user_data_dir(), config::APP_ID)
        .into_iter()
        .find_map(|dir| {
            let path = dir.join(format!("{id}{PALETTE_SUFFIX}"));
            let data = std::fs::read_to_string(&path).ok()?;
            Palette::parse(id, &path.display().to_string(), &data)
                .inspect_err(|e| glib::g_warning!("Ptyxis", "Ignoring user palette: {e}"))
                .ok()
        })
}

fn lookup_builtin(id: &str) -> Option<Palette> {
    // Bundled palettes never change, so parse each at most once.
    thread_local! {
        static CACHE: RefCell<HashMap<&'static str, Palette>> = RefCell::default();
    }

    let (builtin_id, data) = BUILTIN_PALETTES
        .iter()
        .find(|(builtin, _)| *builtin == id)?;
    if let Some(palette) = CACHE.with_borrow(|cache| cache.get(builtin_id).cloned()) {
        return Some(palette);
    }
    let palette = Palette::parse(id, id, data)
        .inspect_err(|e| glib::g_critical!("Ptyxis", "Bundled palette is invalid: {e}"))
        .ok()?;
    CACHE.with_borrow_mut(|cache| cache.insert(builtin_id, palette.clone()));
    Some(palette)
}

/// Looks up a palette by id, preferring user palettes like the C version.
pub fn lookup(id: &str) -> Option<Palette> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    lookup_user(id).or_else(|| lookup_builtin(id))
}

/// Looks up `id`, falling back to the default palette.
pub fn lookup_or_default(id: &str) -> Option<Palette> {
    lookup(id).or_else(|| lookup_builtin(DEFAULT_PALETTE_ID))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_COLORS: &str = "Foreground=#ffffff\nBackground=#000000\n\
        Color0=#000000\nColor1=#110000\nColor2=#001100\nColor3=#111100\n\
        Color4=#000011\nColor5=#110011\nColor6=#001111\nColor7=#cccccc\n\
        Color8=#555555\nColor9=#ff0000\nColor10=#00ff00\nColor11=#ffff00\n\
        Color12=#0000ff\nColor13=#ff00ff\nColor14=#00ffff\nColor15=#ffffff\n";

    #[test]
    fn all_builtin_palettes_parse() {
        assert!(BUILTIN_PALETTES.len() > 100, "palettes should be embedded");
        for (id, data) in BUILTIN_PALETTES {
            let palette = Palette::parse(id, id, data)
                .unwrap_or_else(|e| panic!("builtin palette {id} failed: {e}"));
            assert!(!palette.name().is_empty(), "{id} has no name");
            assert!(palette.has_light() || palette.has_dark(), "{id}");
        }
    }

    #[test]
    fn gnome_palette_matches_source() {
        let palette = lookup_builtin(DEFAULT_PALETTE_ID).expect("gnome palette is bundled");
        assert_eq!(palette.name(), "GNOME");
        assert!(palette.is_primary());
        assert!(palette.has_light() && palette.has_dark());
        let dark = palette.face(true);
        assert_eq!(dark.background, gdk::RGBA::parse("#1c1c1f").unwrap());
        assert_eq!(dark.indexed[1], gdk::RGBA::parse("#c01c28").unwrap());
        let light = palette.face(false);
        assert_eq!(light.cursor_background, gdk::RGBA::parse("#1d1d20").ok());
        assert_eq!(light.cursor_foreground, None);
    }

    #[test]
    fn single_face_palette_is_used_for_both() {
        let data = format!("[Palette]\nName=Flat\n{MINIMAL_COLORS}");
        let palette = Palette::parse("flat", "flat", &data).unwrap();
        assert_eq!(palette.face(true), palette.face(false));
        // Black background is dark, so only the dark appearance is native.
        assert!(palette.has_dark());
        assert!(!palette.has_light());
        assert_eq!(palette.face(true).cursor_background, None);
    }

    #[test]
    fn cursor_background_preferred_over_cursor() {
        let data = format!(
            "[Palette]\nName=C\n{MINIMAL_COLORS}Cursor=#010101\nCursorBackground=#020202\n\
             CursorForeground=#030303\n"
        );
        let face = Palette::parse("c", "c", &data).unwrap().face(false).clone();
        assert_eq!(face.cursor_background, gdk::RGBA::parse("#020202").ok());
        assert_eq!(face.cursor_foreground, gdk::RGBA::parse("#030303").ok());
    }

    #[test]
    fn invalid_palettes_are_rejected() {
        let flat = |extra: &str| format!("[Palette]\nName=Bad\n{MINIMAL_COLORS}{extra}");
        let cases: Vec<(String, &str)> = vec![
            ("not a key file".into(), "not a valid key file"),
            ("[Other]\nName=X\n".into(), "missing Palette section"),
            (format!("[Palette]\n{MINIMAL_COLORS}"), "missing Name key"),
            (
                "[Palette]\nName=X\nForeground=#fff\n".into(),
                "missing Background key in Palette section",
            ),
            (
                flat("Color3=nope\n"),
                "\"nope\" is not a valid color for Color3",
            ),
            (
                format!("[Palette]\nName=X\n[Light]\n{MINIMAL_COLORS}"),
                "missing Dark section",
            ),
        ];
        for (data, expected) in cases {
            let err = Palette::parse("bad", "bad.palette", &data).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "expected {expected:?} in {err}"
            );
        }
    }

    #[test]
    fn lookup_rejects_paths_and_unknown_ids() {
        for id in ["", "../gnome", "nope/gnome", "definitely-not-a-palette"] {
            assert!(lookup(id).is_none(), "{id:?}");
        }
        assert_eq!(
            lookup_or_default("definitely-not-a-palette").map(|p| p.id().to_owned()),
            Some(DEFAULT_PALETTE_ID.to_owned())
        );
    }

    #[test]
    fn user_palette_directories() {
        let data = Path::new("/home/user/.local/share");
        let stable = data.join("org.gnome.Ptyxis/palettes");
        assert_eq!(
            user_palettes_dirs(data, "org.gnome.Ptyxis.Devel"),
            vec![data.join("org.gnome.Ptyxis.Devel/palettes"), stable.clone()]
        );
        assert_eq!(user_palettes_dirs(data, "org.gnome.Ptyxis"), vec![stable]);
    }

    #[test]
    fn builtin_lookups_are_cached_copies() {
        let first = lookup_builtin(DEFAULT_PALETTE_ID).unwrap();
        let second = lookup_builtin(DEFAULT_PALETTE_ID).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn brightness_classification() {
        let cases = [
            ("#000000", true),
            ("#ffffff", false),
            ("#1c1c1f", true),
            ("#fdf6e3", false),
        ];
        for (color, dark) in cases {
            assert_eq!(is_dark(&gdk::RGBA::parse(color).unwrap()), dark, "{color}");
        }
    }
}
