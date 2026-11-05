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
const KEY_USE_SYSTEM_ACCENT: &str = "UseSystemAccent";
const KEY_FOREGROUND: &str = "Foreground";
const KEY_BACKGROUND: &str = "Background";
const KEY_CURSOR: &str = "Cursor";
const KEY_CURSOR_BACKGROUND: &str = "CursorBackground";
const KEY_CURSOR_FOREGROUND: &str = "CursorForeground";
const KEY_TITLEBAR_FOREGROUND: &str = "TitlebarForeground";
const KEY_TITLEBAR_BACKGROUND: &str = "TitlebarBackground";
const KEY_BELL_FOREGROUND: &str = "BellForeground";
const KEY_BELL_BACKGROUND: &str = "BellBackground";
const KEY_SUPERUSER_FOREGROUND: &str = "SuperuserForeground";
const KEY_SUPERUSER_BACKGROUND: &str = "SuperuserBackground";
const KEY_REMOTE_FOREGROUND: &str = "RemoteForeground";
const KEY_REMOTE_BACKGROUND: &str = "RemoteBackground";

/// Indexed colors the superuser and remote highlights derive from when a
/// palette does not set them (red and bright blue).
const SUPERUSER_BASE_INDEX: usize = 1;
/// Indexed color (yellow) the visual bell derives from.
const BELL_BASE_INDEX: usize = 11;
const REMOTE_BASE_INDEX: usize = 12;

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

/// A foreground/background pair used to highlight the window, such as
/// while a superuser or remote process leads the terminal
/// (`PtyxisPaletteScarf`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scarf {
    pub foreground: gdk::RGBA,
    pub background: gdk::RGBA,
}

/// The colors for one appearance (light or dark).
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub foreground: gdk::RGBA,
    pub background: gdk::RGBA,
    pub titlebar_foreground: gdk::RGBA,
    pub titlebar_background: gdk::RGBA,
    pub cursor_background: Option<gdk::RGBA>,
    pub cursor_foreground: Option<gdk::RGBA>,
    pub indexed: [gdk::RGBA; N_INDEXED_COLORS],
    /// Colors flashed by the visual bell and used for banners.
    pub visual_bell: Scarf,
    pub superuser: Scarf,
    pub remote: Scarf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    id: String,
    name: String,
    primary: bool,
    use_system_accent: bool,
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

/// Converts to hue (degrees), saturation and lightness, as
/// `_gdk_hsla_init_from_rgba()` in `src/gdkhsla.c` does.
fn rgb_to_hsl(color: &gdk::RGBA) -> (f32, f32, f32) {
    let (r, g, b) = (color.red(), color.green(), color.blue());
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, lightness);
    }
    let delta = max - min;
    let saturation = if lightness <= 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };
    let hue = if r == max {
        (g - b) / delta
    } else if g == max {
        2.0 + (b - r) / delta
    } else {
        4.0 + (r - g) / delta
    } * 60.0;
    (hue.rem_euclid(360.0), saturation, lightness)
}

/// Inverse of [`rgb_to_hsl`] (`_gdk_rgba_init_from_hsla()`).
fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32, alpha: f32) -> gdk::RGBA {
    if saturation == 0.0 {
        return gdk::RGBA::new(lightness, lightness, lightness, alpha);
    }
    let m2 = if lightness <= 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let m1 = 2.0 * lightness - m2;
    let channel = |hue: f32| {
        let hue = hue.rem_euclid(360.0);
        if hue < 60.0 {
            m1 + (m2 - m1) * hue / 60.0
        } else if hue < 180.0 {
            m2
        } else if hue < 240.0 {
            m1 + (m2 - m1) * (240.0 - hue) / 60.0
        } else {
            m1
        }
    };
    gdk::RGBA::new(
        channel(hue + 120.0),
        channel(hue),
        channel(hue - 120.0),
        alpha,
    )
}

/// Scales lightness and saturation by `factor` (`_gdk_rgba_shade()`).
pub fn shade(color: &gdk::RGBA, factor: f32) -> gdk::RGBA {
    let (hue, saturation, lightness) = rgb_to_hsl(color);
    hsl_to_rgb(
        hue,
        (saturation * factor).clamp(0.0, 1.0),
        (lightness * factor).clamp(0.0, 1.0),
        color.alpha(),
    )
}

/// Blends `amount` of `fg` over `bg`, yielding an opaque color (`mix()` in
/// `src/ptyxis-palette.c`).
pub fn mix(fg: &gdk::RGBA, bg: &gdk::RGBA, amount: f32) -> gdk::RGBA {
    let blend = |f: f32, b: f32| (1.0 - amount) * b + amount * f;
    gdk::RGBA::new(
        blend(fg.red(), bg.red()),
        blend(fg.green(), bg.green()),
        blend(fg.blue(), bg.blue()),
        1.0,
    )
}

struct FaceReader<'a> {
    key_file: &'a glib::KeyFile,
    source_name: &'a str,
    group: &'a str,
}

impl FaceReader<'_> {
    fn parse(&self, key: &str) -> Option<Result<gdk::RGBA, PaletteError>> {
        let value = self.key_file.string(self.group, key).ok()?;
        Some(
            gdk::RGBA::parse(value.as_str()).map_err(|_| PaletteError::InvalidColor {
                source_name: self.source_name.to_owned(),
                group: self.group.to_owned(),
                key: key.to_owned(),
                value: value.to_string(),
            }),
        )
    }

    /// An optional color; like the C loader, a malformed value is ignored
    /// so the derived default applies instead of rejecting the palette.
    fn optional(&self, key: &str) -> Result<Option<gdk::RGBA>, PaletteError> {
        Ok(self.parse(key).and_then(|result| {
            result
                .inspect_err(|e| glib::g_warning!("Ptyxis", "{e}; using the default"))
                .ok()
        }))
    }

    fn required(&self, key: &str) -> Result<gdk::RGBA, PaletteError> {
        self.parse(key).unwrap_or_else(|| {
            Err(PaletteError::MissingKey {
                source_name: self.source_name.to_owned(),
                group: self.group.to_owned(),
                key: key.to_owned(),
            })
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

        // Derived defaults follow ptyxis_palette_load_face().
        let dark = is_dark(&background);
        let titlebar_foreground = self
            .optional(KEY_TITLEBAR_FOREGROUND)?
            .unwrap_or_else(|| shade(&foreground, if dark { 1.25 } else { 0.95 }));
        let titlebar_background = self.optional(KEY_TITLEBAR_BACKGROUND)?.unwrap_or_else(|| {
            if dark {
                shade(&background, 1.25)
            } else {
                background
            }
        });
        let visual_bell = Scarf {
            foreground: self
                .optional(KEY_BELL_FOREGROUND)?
                .unwrap_or(titlebar_foreground),
            background: self
                .optional(KEY_BELL_BACKGROUND)?
                .unwrap_or_else(|| mix(&indexed[BELL_BASE_INDEX], &titlebar_background, 0.25)),
        };
        let scarf = |fg_key, bg_key, base: usize| -> Result<Scarf, PaletteError> {
            Ok(Scarf {
                foreground: self
                    .optional(fg_key)?
                    .unwrap_or_else(|| shade(&titlebar_foreground, if dark { 1.0 } else { 0.8 })),
                background: self.optional(bg_key)?.unwrap_or_else(|| {
                    mix(&indexed[base], &background, if dark { 0.33 } else { 0.5 })
                }),
            })
        };
        let superuser = scarf(
            KEY_SUPERUSER_FOREGROUND,
            KEY_SUPERUSER_BACKGROUND,
            SUPERUSER_BASE_INDEX,
        )?;
        let remote = scarf(
            KEY_REMOTE_FOREGROUND,
            KEY_REMOTE_BACKGROUND,
            REMOTE_BASE_INDEX,
        )?;

        Ok(Face {
            foreground,
            background,
            titlebar_foreground,
            titlebar_background,
            cursor_background,
            cursor_foreground,
            indexed,
            visual_bell,
            superuser,
            remote,
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
            use_system_accent: key_file
                .boolean(GROUP_PALETTE, KEY_USE_SYSTEM_ACCENT)
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

    /// Whether the desktop accent color is kept instead of one derived
    /// from the palette.
    pub fn use_system_accent(&self) -> bool {
        self.use_system_accent
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

/// User palettes from every directory; an id found in an earlier (more
/// specific) directory hides the same id in later ones, as lookups do.
fn user_palettes() -> Vec<Palette> {
    let mut palettes: Vec<Palette> = Vec::new();
    for dir in user_palettes_dirs(&glib::user_data_dir(), config::APP_ID) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(PALETTE_SUFFIX))
            else {
                continue;
            };
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|data| {
                    Palette::parse(id, &path.display().to_string(), &data)
                        .map_err(|e| e.to_string())
                }) {
                Ok(palette) if !palettes.iter().any(|p| p.id() == palette.id()) => {
                    palettes.push(palette)
                }
                Ok(_) => {}
                Err(e) => glib::g_warning!("Ptyxis", "Ignoring user palette: {e}"),
            }
        }
    }
    palettes
}

/// Sorts palettes by name the way the C list model's string sorter does
/// (case-insensitively).
pub fn sort_by_name(palettes: &mut [Palette]) {
    palettes.sort_by_cached_key(|p| p.name().to_lowercase());
}

/// Every bundled and user palette, sorted by name, as
/// `ptyxis_palette_get_all()` lists them.
pub fn all() -> Vec<Palette> {
    let mut palettes: Vec<Palette> = BUILTIN_PALETTES
        .iter()
        .filter_map(|(id, _)| lookup_builtin(id))
        .chain(user_palettes())
        .collect();
    sort_by_name(&mut palettes);
    palettes
}

/// Directory palettes are imported into (`ptyxis_get_user_palettes_dir()`).
pub fn import_dir() -> PathBuf {
    user_palettes_dirs(&glib::user_data_dir(), config::APP_ID).swap_remove(0)
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} has no file name", .0.display())]
    NoFileName(PathBuf),
    #[error("{} already exists.\nPlease remove it first.", .0.display())]
    AlreadyExists(PathBuf),
}

/// Copies a palette file into `dest_dir` like `ptyxis --import-palette`
/// (`check_early_opts()` in `src/main.c`): an existing file of the same
/// name is never overwritten. Returns the destination path.
pub fn import(source: &Path, dest_dir: &Path) -> Result<PathBuf, ImportError> {
    use std::os::unix::fs::DirBuilderExt;

    let io_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ImportError::Io { path, source }
    };
    let name = source
        .file_name()
        .ok_or_else(|| ImportError::NoFileName(source.to_path_buf()))?;
    let contents = std::fs::read(source).map_err(io_error(source))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o750)
        .create(dest_dir)
        .map_err(io_error(dest_dir))?;
    let dest = dest_dir.join(name);
    // create_new is atomic and refuses any existing entry, including a
    // dangling symlink, so the import can never write through one.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
        .map_err(|source| match source.kind() {
            std::io::ErrorKind::AlreadyExists => ImportError::AlreadyExists(dest.clone()),
            _ => ImportError::Io {
                path: dest.clone(),
                source,
            },
        })?;
    if let Err(source) = std::io::Write::write_all(&mut file, &contents) {
        drop(file);
        // Do not leave a truncated palette behind.
        let _ = std::fs::remove_file(&dest);
        return Err(ImportError::Io { path: dest, source });
    }
    Ok(dest)
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
        assert!(palette.use_system_accent());
        assert!(!lookup_builtin("tango").unwrap().use_system_accent());
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

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ptyxis-palette-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn importing_palettes() {
        let scratch = scratch_dir("import");
        let source_dir = scratch.join("src");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("Mine.palette");
        std::fs::write(&source, "[Palette]\nName=Mine\n").unwrap();
        let dest_dir = scratch.join("nested/palettes");

        let dest = import(&source, &dest_dir).expect("first import succeeds");
        assert_eq!(dest, dest_dir.join("Mine.palette"));
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            std::fs::read(&source).unwrap()
        );

        let err = import(&source, &dest_dir).unwrap_err();
        assert!(matches!(err, ImportError::AlreadyExists(_)), "{err}");
        assert!(err.to_string().contains("Please remove it first"));

        // A dangling symlink at the destination is refused, not followed.
        let link_target = scratch.join("outside.palette");
        std::os::unix::fs::symlink(&link_target, dest_dir.join("Linked.palette")).unwrap();
        let linked_source = source_dir.join("Linked.palette");
        std::fs::write(&linked_source, "[Palette]\nName=Linked\n").unwrap();
        let err = import(&linked_source, &dest_dir).unwrap_err();
        assert!(matches!(err, ImportError::AlreadyExists(_)), "{err}");
        assert!(
            !link_target.exists(),
            "import must not write through the symlink"
        );

        let err = import(&source_dir.join("missing.palette"), &dest_dir).unwrap_err();
        assert!(matches!(err, ImportError::Io { .. }), "{err}");
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn listing_is_sorted_case_insensitively() {
        let mut palettes: Vec<Palette> = ["tango", "3024", "Aci", "gnome"]
            .iter()
            .map(|id| lookup_builtin(id).unwrap())
            .collect();
        sort_by_name(&mut palettes);
        let names: Vec<_> = palettes.iter().map(|p| p.name().to_owned()).collect();
        assert_eq!(names, ["3024", "Aci", "GNOME", "Tango"]);
        assert!(all().len() >= BUILTIN_PALETTES.len());
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

    fn rgba(color: &str) -> gdk::RGBA {
        gdk::RGBA::parse(color).unwrap()
    }

    fn assert_close(actual: &gdk::RGBA, expected: &gdk::RGBA, context: &str) {
        let channels = |c: &gdk::RGBA| [c.red(), c.green(), c.blue(), c.alpha()];
        for (a, e) in channels(actual).into_iter().zip(channels(expected)) {
            assert!((a - e).abs() < 1e-4, "{context}: {actual} != {expected}");
        }
    }

    #[test]
    fn hsl_shading() {
        let cases = [
            // (color, factor, expected)
            ("#808080", 1.0, "#808080"),
            ("#ffffff", 0.5, "rgb(50%,50%,50%)"),
            ("#ff0000", 0.8, "rgb(72%,8%,8%)"),
            ("#ff0000", 1.25, "rgb(100%,25%,25%)"),
            ("#000000", 1.25, "#000000"),
            ("rgba(0,0,255,0.5)", 1.0, "rgba(0,0,255,0.5)"),
            ("#c01c28", 1.0, "#c01c28"),
        ];
        for (color, factor, expected) in cases {
            assert_close(
                &shade(&rgba(color), factor),
                &rgba(expected),
                &format!("shade({color}, {factor})"),
            );
        }
    }

    #[test]
    fn color_mixing() {
        let cases = [
            ("#ff0000", "#0000ff", 0.0, "#0000ff"),
            ("#ff0000", "#0000ff", 1.0, "#ff0000"),
            ("#ffffff", "#000000", 0.5, "rgb(50%,50%,50%)"),
            ("rgba(255,0,0,0.2)", "#000000", 1.0, "#ff0000"),
        ];
        for (fg, bg, amount, expected) in cases {
            assert_close(
                &mix(&rgba(fg), &rgba(bg), amount),
                &rgba(expected),
                &format!("mix({fg}, {bg}, {amount})"),
            );
        }
    }

    #[test]
    fn scarves_default_from_indexed_colors() {
        let data = format!("[Palette]\nName=Flat\n{MINIMAL_COLORS}");
        let face = Palette::parse("flat", "flat", &data)
            .unwrap()
            .face(true)
            .clone();
        // Black background: dark defaults.
        assert_close(
            &face.titlebar_background,
            &shade(&face.background, 1.25),
            "titlebar background",
        );
        assert_close(
            &face.superuser.background,
            &mix(&face.indexed[1], &face.background, 0.33),
            "superuser background",
        );
        assert_close(
            &face.remote.background,
            &mix(&face.indexed[12], &face.background, 0.33),
            "remote background",
        );
        assert_close(
            &face.superuser.foreground,
            &face.titlebar_foreground,
            "superuser foreground",
        );
        assert_eq!(face.visual_bell.foreground, face.titlebar_foreground);
        assert_close(
            &face.visual_bell.background,
            // 25% yellow over 75% title bar, spelled out per channel.
            &{
                let (yellow, bar) = (face.indexed[11], face.titlebar_background);
                let channel = |y: f32, b: f32| 0.25 * y + 0.75 * b;
                gdk::RGBA::new(
                    channel(yellow.red(), bar.red()),
                    channel(yellow.green(), bar.green()),
                    channel(yellow.blue(), bar.blue()),
                    1.0,
                )
            },
            "bell background",
        );

        let light = data.replace("Background=#000000", "Background=#ffffff");
        let face = Palette::parse("l", "l", &light)
            .unwrap()
            .face(false)
            .clone();
        assert_eq!(face.titlebar_background, face.background);
        assert_close(
            &face.remote.background,
            &mix(&face.indexed[12], &face.background, 0.5),
            "light remote background",
        );
        assert_close(
            &face.remote.foreground,
            &shade(&face.titlebar_foreground, 0.8),
            "light remote foreground",
        );
    }

    #[test]
    fn scarves_can_be_overridden() {
        let data = format!(
            "[Palette]\nName=S\n{MINIMAL_COLORS}SuperuserForeground=#010101\n\
             SuperuserBackground=#020202\nRemoteForeground=#030303\n\
             RemoteBackground=#040404\nTitlebarBackground=#050505\n\
             BellForeground=#060606\nBellBackground=#070707\nUseSystemAccent=true\n"
        );
        let palette = Palette::parse("s", "s", &data).unwrap();
        assert!(palette.use_system_accent());
        let face = Palette::parse("s", "s", &data).unwrap().face(true).clone();
        assert_eq!(face.superuser.foreground, rgba("#010101"));
        assert_eq!(face.superuser.background, rgba("#020202"));
        assert_eq!(face.remote.foreground, rgba("#030303"));
        assert_eq!(face.remote.background, rgba("#040404"));
        assert_eq!(face.titlebar_background, rgba("#050505"));
        assert_eq!(face.visual_bell.foreground, rgba("#060606"));
        assert_eq!(face.visual_bell.background, rgba("#070707"));

        // Malformed optional colors fall back to the defaults.
        let bad = format!("[Palette]\nName=S\n{MINIMAL_COLORS}RemoteBackground=nope\n");
        let face = Palette::parse("s", "s", &bad).unwrap().face(true).clone();
        assert_close(
            &face.remote.background,
            &mix(&face.indexed[12], &face.background, 0.33),
            "malformed remote background",
        );
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
