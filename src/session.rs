// session.rs
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

//! The windows and tabs saved when the last window closes and reopened on
//! the next start (`src/ptyxis-session.c`).
//!
//! The state is an `a{sv}` GVariant in the same layout as Ptyxis's
//! `session.gvariant`, kept in tty-rex's own configuration directory.
//! Pinned tabs always come back; the others only with the
//! `restore-session` setting.

use std::path::PathBuf;

use gtk::glib;
use gtk::glib::prelude::*;

use crate::app::TabRequest;
use crate::config;
use crate::terminal::ZoomLevel;

const FILE_NAME: &str = "session.gvariant";
const VERSION: u32 = 1;
/// Type of the `windows` and `tabs` lists.
const DICT_ARRAY_TYPE: &str = "aa{sv}";

mod key {
    pub const VERSION: &str = "version";
    pub const WINDOWS: &str = "windows";
    pub const MAXIMIZED: &str = "maximized";
    pub const TABS: &str = "tabs";
    pub const PROFILE: &str = "profile";
    pub const PINNED: &str = "pinned";
    pub const SIZE: &str = "size";
    pub const ZOOM: &str = "zoom";
    pub const ACTIVE: &str = "active";
    pub const WINDOW_TITLE: &str = "window-title";
    pub const CWD: &str = "cwd";
    pub const CONTAINER: &str = "container";
}

/// Where the session is kept (`get_session_file()`).
pub fn file_path() -> PathBuf {
    glib::user_config_dir().join(config::APP_ID).join(FILE_NAME)
}

/// One saved tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTab {
    pub profile: Option<String>,
    pub pinned: bool,
    /// Terminal size in (columns, rows).
    pub size: Option<(u32, u32)>,
    pub zoom: ZoomLevel,
    pub active: bool,
    pub window_title: Option<String>,
    pub cwd: Option<String>,
    /// Container id, when not the profile's default one.
    pub container: Option<String>,
}

/// One saved window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionWindow {
    pub maximized: bool,
    pub tabs: Vec<SessionTab>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub windows: Vec<SessionWindow>,
}

/// A tab to recreate, with where it goes in its window.
#[derive(Debug, Clone)]
pub struct RestoreTab {
    pub request: TabRequest,
    pub pinned: bool,
    pub active: bool,
}

/// A window to recreate.
#[derive(Debug, Clone)]
pub struct RestoreWindow {
    pub maximized: bool,
    pub tabs: Vec<RestoreTab>,
    /// Ends the window with a tab using the default profile, which is
    /// focused unless a restored tab was the active one. Used when only
    /// pinned tabs come back, so the user gets a fresh tab to work in.
    pub add_default_tab: bool,
}

/// Settings that shape what a restore recreates.
#[derive(Debug, Clone, Copy)]
pub struct RestoreOptions {
    pub restore_session: bool,
    pub restore_window_size: bool,
    pub default_size: (u32, u32),
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

fn dict_array(items: impl IntoIterator<Item = glib::Variant>) -> glib::Variant {
    glib::Variant::array_from_iter_with_type(glib::VariantTy::VARDICT, items)
}

fn lookup_dicts(dict: &glib::VariantDict, key: &str) -> Vec<glib::Variant> {
    let ty = glib::VariantTy::new(DICT_ARRAY_TYPE).expect("valid variant type");
    dict.lookup_value(key, Some(ty))
        .map(|list| list.iter().collect())
        .unwrap_or_default()
}

/// A value of the expected type, treating a mismatch like a missing key as
/// `g_variant_lookup()` does.
fn lookup<T: glib::variant::FromVariant>(dict: &glib::VariantDict, key: &str) -> Option<T> {
    dict.lookup(key).ok().flatten()
}

impl SessionTab {
    fn to_variant(&self) -> glib::Variant {
        let dict = glib::VariantDict::new(None);
        if let Some(profile) = &self.profile {
            dict.insert_value(key::PROFILE, &profile.to_variant());
        }
        dict.insert_value(key::PINNED, &self.pinned.to_variant());
        if let Some(size) = self.size {
            dict.insert_value(key::SIZE, &size.to_variant());
        }
        if self.zoom != ZoomLevel::DEFAULT {
            dict.insert_value(key::ZOOM, &self.zoom.to_ptyxis_enum().to_variant());
        }
        dict.insert_value(key::ACTIVE, &self.active.to_variant());
        if let Some(title) = self.window_title.as_ref().filter(|s| !s.is_empty()) {
            dict.insert_value(key::WINDOW_TITLE, &title.to_variant());
        }
        if let Some(cwd) = self.cwd.as_ref().filter(|s| !s.is_empty()) {
            dict.insert_value(key::CWD, &cwd.to_variant());
        }
        if let Some(container) = &self.container {
            dict.insert_value(key::CONTAINER, &container.to_variant());
        }
        dict.end()
    }

    fn from_variant(variant: &glib::Variant) -> Self {
        let dict = glib::VariantDict::new(Some(variant));
        Self {
            profile: non_empty(lookup(&dict, key::PROFILE)),
            pinned: lookup(&dict, key::PINNED).unwrap_or(false),
            size: lookup(&dict, key::SIZE),
            // Out of range levels fall back to the default, as in C.
            zoom: lookup(&dict, key::ZOOM)
                .and_then(ZoomLevel::from_ptyxis_enum)
                .unwrap_or_default(),
            active: lookup(&dict, key::ACTIVE).unwrap_or(false),
            window_title: lookup(&dict, key::WINDOW_TITLE),
            cwd: lookup(&dict, key::CWD),
            container: non_empty(lookup(&dict, key::CONTAINER)),
        }
    }
}

impl SessionWindow {
    fn to_variant(&self) -> glib::Variant {
        let dict = glib::VariantDict::new(None);
        if self.maximized {
            dict.insert_value(key::MAXIMIZED, &true.to_variant());
        }
        let tabs = dict_array(self.tabs.iter().map(SessionTab::to_variant));
        dict.insert_value(key::TABS, &tabs);
        dict.end()
    }

    fn from_variant(variant: &glib::Variant) -> Self {
        let dict = glib::VariantDict::new(Some(variant));
        Self {
            maximized: lookup(&dict, key::MAXIMIZED).unwrap_or(false),
            tabs: lookup_dicts(&dict, key::TABS)
                .iter()
                .map(SessionTab::from_variant)
                .collect(),
        }
    }
}

impl Session {
    /// The state as `ptyxis_session_save()` builds it.
    pub fn to_variant(&self) -> glib::Variant {
        let dict = glib::VariantDict::new(None);
        dict.insert_value(key::VERSION, &VERSION.to_variant());
        let windows = dict_array(self.windows.iter().map(SessionWindow::to_variant));
        dict.insert_value(key::WINDOWS, &windows);
        dict.end()
    }

    /// Reads a saved state; `None` without a version or window list,
    /// which `ptyxis_session_restore()` refuses too.
    pub fn from_variant(variant: &glib::Variant) -> Option<Self> {
        if !variant.is::<glib::VariantDict>() {
            return None;
        }
        let dict = glib::VariantDict::new(Some(variant));
        lookup::<u32>(&dict, key::VERSION)?;
        let ty = glib::VariantTy::new(DICT_ARRAY_TYPE).expect("valid variant type");
        let windows = dict.lookup_value(key::WINDOWS, Some(ty))?;
        Some(Self {
            windows: windows
                .iter()
                .map(|w| SessionWindow::from_variant(&w))
                .collect(),
        })
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let bytes = glib::Bytes::from(bytes);
        // Not trusted: GLib validates the data while reading it.
        let variant = glib::Variant::from_bytes_with_type(&bytes, glib::VariantTy::VARDICT);
        Self::from_variant(&variant)
    }

    pub fn to_bytes(&self) -> glib::Bytes {
        self.to_variant().data_as_bytes()
    }

    /// Loads the saved session, if there is a readable one.
    pub fn load() -> Option<Self> {
        let path = file_path();
        match std::fs::read(&path) {
            Ok(bytes) => Self::from_bytes(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                glib::g_warning!(
                    "tty-rex",
                    "Cannot read session state {}: {e}",
                    path.display()
                );
                None
            }
        }
    }

    /// The windows to recreate, following `ptyxis_session_restore()`:
    /// without `restore-session` only pinned tabs come back, and windows
    /// left without tabs are skipped.
    pub fn restore_plan(&self, options: RestoreOptions) -> Vec<RestoreWindow> {
        self.windows
            .iter()
            .filter_map(|window| {
                let tabs: Vec<RestoreTab> = window
                    .tabs
                    .iter()
                    .filter(|tab| tab.pinned || options.restore_session)
                    .map(|tab| restore_tab(tab, window.maximized, options))
                    .collect();
                (!tabs.is_empty()).then_some(RestoreWindow {
                    maximized: window.maximized,
                    tabs,
                    add_default_tab: !options.restore_session,
                })
            })
            .collect()
    }
}

fn restore_tab(tab: &SessionTab, maximized: bool, options: RestoreOptions) -> RestoreTab {
    let size = tab
        .size
        .filter(|_| options.restore_window_size)
        .unwrap_or(options.default_size);
    RestoreTab {
        request: TabRequest {
            profile_uuid: tab.profile.clone(),
            previous_cwd_uri: tab.cwd.clone(),
            initial_title: tab.window_title.clone(),
            zoom: tab.zoom,
            // A maximized window sizes its terminals itself.
            grid_size: (!maximized).then_some(size),
            container_id: tab.container.clone(),
            ..Default::default()
        },
        pinned: tab.pinned,
        active: tab.active,
    }
}

/// The container id worth saving for a tab: none when it runs in its
/// profile's default container anyway.
pub fn container_to_save(container: Option<&str>, profile_default: &str) -> Option<String> {
    container
        .filter(|id| *id != profile_default)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(pinned: bool, active: bool) -> SessionTab {
        SessionTab {
            pinned,
            active,
            ..Default::default()
        }
    }

    #[test]
    fn round_trips_through_bytes() {
        let session = Session {
            windows: vec![
                SessionWindow {
                    maximized: true,
                    tabs: vec![
                        SessionTab {
                            profile: Some("abc".into()),
                            pinned: true,
                            size: Some((80, 24)),
                            zoom: ZoomLevel::DEFAULT.zoom_in(),
                            active: true,
                            window_title: Some("vim".into()),
                            cwd: Some("file:///tmp".into()),
                            container: Some("c0ffee".into()),
                        },
                        tab(false, false),
                    ],
                },
                SessionWindow::default(),
            ],
        };
        let bytes = session.to_bytes();
        assert_eq!(Session::from_bytes(&bytes), Some(session));
    }

    #[test]
    fn reads_ptyxis_layout() {
        // As written by ptyxis_session_save().
        let text = "{'version': <uint32 1>, 'windows': <[{'maximized': <true>, \
                    'tabs': <[{'profile': <'p1'>, 'pinned': <false>, \
                    'size': <(uint32 100, uint32 30)>, 'zoom': <uint32 17>, \
                    'active': <true>, 'cwd': <'file:///home'>}]>}]>}";
        let variant = glib::Variant::parse(Some(glib::VariantTy::VARDICT), text).unwrap();
        let session = Session::from_variant(&variant).unwrap();
        assert_eq!(
            session.windows,
            [SessionWindow {
                maximized: true,
                tabs: vec![SessionTab {
                    profile: Some("p1".into()),
                    size: Some((100, 30)),
                    zoom: ZoomLevel::DEFAULT.zoom_in().zoom_in(),
                    active: true,
                    cwd: Some("file:///home".into()),
                    ..Default::default()
                }],
            }]
        );
    }

    #[test]
    fn rejects_invalid_state() {
        let cases: &[&[u8]] = &[b"", b"garbage that is not a variant"];
        for bytes in cases {
            assert_eq!(Session::from_bytes(bytes), None, "{bytes:?}");
        }
        let cases = [
            // No version.
            "{'windows': <@aa{sv} []>}",
            // No windows.
            "{'version': <uint32 1>}",
            // Windows of the wrong type.
            "{'version': <uint32 1>, 'windows': <'none'>}",
        ];
        for text in cases {
            let variant = glib::Variant::parse(Some(glib::VariantTy::VARDICT), text).unwrap();
            assert_eq!(Session::from_variant(&variant), None, "{text}");
        }
    }

    #[test]
    fn bad_values_fall_back() {
        let text = "{'version': <uint32 1>, 'windows': <[{'tabs': <[\
                    {'zoom': <uint32 99>, 'pinned': <'yes'>, 'profile': <''>}]>}]>}";
        let variant = glib::Variant::parse(Some(glib::VariantTy::VARDICT), text).unwrap();
        let session = Session::from_variant(&variant).unwrap();
        assert_eq!(session.windows[0].tabs, [SessionTab::default()]);
    }

    #[test]
    fn restore_plans() {
        let session = Session {
            windows: vec![
                SessionWindow {
                    maximized: false,
                    tabs: vec![
                        SessionTab {
                            size: Some((100, 40)),
                            ..tab(true, false)
                        },
                        tab(false, true),
                    ],
                },
                SessionWindow {
                    maximized: true,
                    tabs: vec![tab(false, false)],
                },
                SessionWindow::default(),
            ],
        };
        // (restore_session, restore_window_size,
        //  per window: (maximized, add_default_tab, [(pinned, active, grid)]))
        type Expected = &'static [(bool, bool, &'static [(bool, bool, Option<(u32, u32)>)])];
        let cases: [(bool, bool, Expected); 3] = [
            (
                true,
                true,
                &[
                    (
                        false,
                        false,
                        &[
                            (true, false, Some((100, 40))),
                            (false, true, Some((80, 24))),
                        ],
                    ),
                    (true, false, &[(false, false, None)]),
                ],
            ),
            (
                true,
                false,
                &[
                    (
                        false,
                        false,
                        &[(true, false, Some((80, 24))), (false, true, Some((80, 24)))],
                    ),
                    (true, false, &[(false, false, None)]),
                ],
            ),
            // Only the pinned tab, plus a fresh one; the other windows
            // have nothing left.
            (
                false,
                true,
                &[(false, true, &[(true, false, Some((100, 40)))])],
            ),
        ];
        for (restore_session, restore_window_size, expected) in cases {
            let plan = session.restore_plan(RestoreOptions {
                restore_session,
                restore_window_size,
                default_size: (80, 24),
            });
            let got: Vec<_> = plan
                .iter()
                .map(|w| {
                    let tabs: Vec<_> = w
                        .tabs
                        .iter()
                        .map(|t| (t.pinned, t.active, t.request.grid_size))
                        .collect();
                    (w.maximized, w.add_default_tab, tabs)
                })
                .collect();
            let expected: Vec<_> = expected
                .iter()
                .map(|(m, d, tabs)| (*m, *d, tabs.to_vec()))
                .collect();
            assert_eq!(got, expected, "{restore_session} {restore_window_size}");
        }
    }

    #[test]
    fn restore_carries_tab_state() {
        let session = Session {
            windows: vec![SessionWindow {
                maximized: false,
                tabs: vec![SessionTab {
                    profile: Some("p".into()),
                    zoom: ZoomLevel::MAX,
                    window_title: Some("htop".into()),
                    cwd: Some("file:///srv".into()),
                    container: Some("box".into()),
                    ..tab(true, true)
                }],
            }],
        };
        let plan = session.restore_plan(RestoreOptions {
            restore_session: true,
            restore_window_size: true,
            default_size: (80, 24),
        });
        let request = &plan[0].tabs[0].request;
        assert_eq!(request.profile_uuid.as_deref(), Some("p"));
        assert_eq!(request.zoom, ZoomLevel::MAX);
        assert_eq!(request.initial_title.as_deref(), Some("htop"));
        assert_eq!(request.previous_cwd_uri.as_deref(), Some("file:///srv"));
        assert_eq!(request.initial_cwd_uri, None);
        assert_eq!(request.container_id.as_deref(), Some("box"));
    }

    #[test]
    fn saves_only_non_default_containers() {
        let cases = [
            (None, "session", None),
            (Some("session"), "session", None),
            (Some("box"), "session", Some("box")),
            (Some("session"), "box", Some("session")),
        ];
        for (container, default, expected) in cases {
            assert_eq!(
                container_to_save(container, default).as_deref(),
                expected,
                "{container:?} {default}"
            );
        }
    }
}
