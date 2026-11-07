// debug_info.rs
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

//! The troubleshooting information of the About dialog
//! (`generate_debug_info()` in `src/ptyxis-application.c`).
//!
//! [`DebugInfo`] holds what the application gathers and [`DebugInfo::render`]
//! lays it out exactly as Ptyxis does, so the text can be tested without a
//! display.

use std::ffi::{CStr, c_char, c_uint};
use std::fmt::Write as _;
use std::path::Path;

/// `/.flatpak-info`, present inside a Flatpak sandbox
/// (`ptyxis_init_ctor()`), whose contents are reported too.
pub const FLATPAK_INFO_PATH: &str = "/.flatpak-info";
/// The VTE shell integration script, reported as present or missing.
pub const VTE_SH_PATH: &str = "/etc/profile.d/vte.sh";
/// The host's copy of [`VTE_SH_PATH`] as seen from a Flatpak sandbox.
pub const HOST_VTE_SH_PATH: &str = "/var/run/host/etc/profile.d/vte.sh";
pub const OS_RELEASE_PATH: &str = "/etc/os-release";
/// The host's `os-release` as seen from a Flatpak sandbox.
pub const HOST_OS_RELEASE_PATH: &str = "/var/run/host/etc/os-release";

/// How glibc's `printf()` shows a `NULL` string, which Ptyxis prints for an
/// unknown GTK theme.
const NULL_STRING: &str = "(null)";
/// Where the operating system name sits in `os-release`.
const OS_NAME_PREFIX: &str = "NAME=\"";

/// A library's version at run time and the one tty-rex was built against.
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryVersion {
    pub runtime: (u32, u32, u32),
    /// As `build.rs` learned it from pkg-config, e.g. "1.86.0".
    pub compiled: String,
}

impl LibraryVersion {
    fn describe(&self) -> String {
        let (major, minor, micro) = self.runtime;
        format!(
            "{major}.{minor}.{micro} (compiled against {})",
            self.compiled
        )
    }
}

/// The monitor a window is on.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// In millihertz, as GDK reports it.
    pub refresh_rate: i32,
}

/// One terminal window.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    pub n_tabs: u32,
    /// The type name of the window's GSK renderer.
    pub renderer: String,
    pub scale: f64,
    pub scale_factor: i32,
    /// `None` while the window is on no monitor, which Ptyxis never expects.
    pub monitor: Option<MonitorInfo>,
}

/// A container shells can be spawned into.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerInfo {
    pub display_name: String,
    pub provider: String,
}

/// Everything `generate_debug_info()` reports.
#[derive(Clone, Debug, PartialEq)]
pub struct DebugInfo {
    pub app_name: String,
    pub version: String,
    pub build_identifier: String,
    /// The `NAME` of `os-release` (`ptyxis_application_get_os_name()`).
    pub os_name: Option<String>,
    /// `sysname`, `release`, `version` and `machine` of `uname()`.
    pub uname: Option<[String; 4]>,
    /// Whether tty-rex runs in a Flatpak sandbox.
    pub in_sandbox: bool,
    pub desktop_session: Option<String>,
    pub glib: LibraryVersion,
    pub gtk: LibraryVersion,
    pub vte: LibraryVersion,
    /// `vte_get_features()`.
    pub vte_features: String,
    /// The type name of the GDK display.
    pub display: String,
    pub accessibility: bool,
    pub gtk_theme: Option<String>,
    pub system_font: String,
    /// The configured font, or `None` when the system font is used.
    pub font: Option<String>,
    pub windows: Vec<WindowInfo>,
    pub development_build: bool,
    pub app_id: String,
    pub vte_sh_path: String,
    pub vte_sh_exists: bool,
    /// Every container but the session.
    pub containers: Vec<ContainerInfo>,
    pub flatpak_info: Option<String>,
    pub os_release: Option<String>,
}

impl DebugInfo {
    /// The text shown on the About dialog's troubleshooting page, laid out
    /// as `generate_debug_info()` does.
    pub fn render(&self) -> String {
        // Writing to a String cannot fail.
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{} {} ({})",
            self.app_name, self.version, self.build_identifier
        );
        s.push('\n');

        // Ptyxis reads the name from the agent's `OsName` D-Bus property,
        // which GDBus sends as "" while the agent has not set it.
        let os_name = self.os_name.as_deref().unwrap_or_default();
        let _ = writeln!(s, "Operating System: {os_name}");
        s.push('\n');

        if let Some([sysname, release, version, machine]) = &self.uname {
            let _ = writeln!(s, "uname.sysname = {sysname}");
            let _ = writeln!(s, "uname.release = {release}");
            let _ = writeln!(s, "uname.version = {version}");
            let _ = writeln!(s, "uname.machine = {machine}");
        }

        // tty-rex has no ptyxis-agent: it spawns shells itself, wherever
        // it runs.
        s.push('\n');
        let place = if self.in_sandbox {
            "in sandbox"
        } else {
            "on host"
        };
        let _ = writeln!(s, "Agent: none, spawning directly {place}");

        s.push('\n');
        let session = self.desktop_session.as_deref().unwrap_or("Unspecified");
        let _ = writeln!(s, "Desktop Session: {session}");

        s.push('\n');
        let _ = writeln!(s, "GLib: {}", self.glib.describe());
        let _ = writeln!(s, "GTK: {}", self.gtk.describe());
        let _ = writeln!(s, "VTE: {} {}", self.vte.describe(), self.vte_features);

        s.push('\n');
        let _ = writeln!(s, "Display: {}", self.display);
        let a11y = if self.accessibility { "Yes" } else { "No" };
        let _ = writeln!(s, "Accessibility: {a11y}");

        s.push('\n');
        let theme = self.gtk_theme.as_deref().unwrap_or(NULL_STRING);
        let _ = writeln!(s, "GTK Theme: {theme}");
        let _ = writeln!(s, "System Font: {}", self.system_font);
        match &self.font {
            None => s.push_str("Font: -- Using System Font --\n"),
            Some(font) => {
                let _ = writeln!(s, "Font: {font}");
            }
        }

        for (id, window) in self.windows.iter().enumerate() {
            s.push('\n');
            let _ = writeln!(s, "window[{id}].n_tabs = {}", window.n_tabs);
            let _ = writeln!(s, "window[{id}].renderer = {}", window.renderer);
            // `%lf` prints six decimals.
            let _ = writeln!(s, "window[{id}].scale = {:.6}", window.scale);
            let _ = writeln!(s, "window[{id}].scale_factor = {}", window.scale_factor);
            if let Some(m) = &window.monitor {
                let _ = writeln!(
                    s,
                    "window[{id}].monitor.geometry = {},{} {}×{}",
                    m.x, m.y, m.width, m.height
                );
                let _ = writeln!(s, "window[{id}].monitor.refresh_rate = {}", m.refresh_rate);
            }
        }

        if self.development_build {
            s.push('\n');
            s.push_str("** DEVELOPMENT BUILD **\n");
        }

        s.push('\n');
        let _ = writeln!(s, "App ID: {}", self.app_id);

        s.push('\n');
        let state = if self.vte_sh_exists {
            "exists"
        } else {
            "missing"
        };
        let _ = writeln!(s, "{} {state}", self.vte_sh_path);

        s.push('\n');
        s.push_str("Containers:\n");
        for container in &self.containers {
            let _ = writeln!(s, "  • {} ({})", container.display_name, container.provider);
        }

        for contents in [&self.flatpak_info, &self.os_release].into_iter().flatten() {
            s.push('\n');
            s.push_str(contents);
        }

        s
    }
}

/// The operating system name of `os-release` contents, as the agent reads
/// it (`ptyxis_agent_impl_load_os_release()`): the quoted text after the
/// first `NAME="`.
///
/// Like the C code this takes the first match anywhere, so a
/// `PRETTY_NAME="…"` line before `NAME=` wins, and an unquoted `NAME=`
/// gives no name.
pub fn os_name_from_release(contents: &str) -> Option<String> {
    let start = contents.find(OS_NAME_PREFIX)? + OS_NAME_PREFIX.len();
    let rest = &contents[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// The files describing the system: the `vte.sh` and `os-release` of the
/// host, which a Flatpak sandbox sees under `/var/run/host`.
pub fn system_paths(in_sandbox: bool) -> (&'static Path, &'static Path) {
    if in_sandbox {
        (Path::new(HOST_VTE_SH_PATH), Path::new(HOST_OS_RELEASE_PATH))
    } else {
        (Path::new(VTE_SH_PATH), Path::new(OS_RELEASE_PATH))
    }
}

// GLib exports its run-time version only as variables, which glib-sys
// does not bind.
unsafe extern "C" {
    static glib_major_version: c_uint;
    static glib_minor_version: c_uint;
    static glib_micro_version: c_uint;
}

/// The GLib loaded at run time (`glib_major_version` and friends).
pub fn glib_runtime_version() -> (u32, u32, u32) {
    // SAFETY: GLib initializes these constants before any code runs and
    // never writes them again.
    unsafe { (glib_major_version, glib_minor_version, glib_micro_version) }
}

/// The VTE loaded at run time and its `vte_get_features()`.
pub fn vte_runtime_version() -> ((u32, u32, u32), String) {
    // SAFETY: these return constants of the loaded libvte; the features
    // string is static and NUL-terminated.
    unsafe {
        let features = vte::ffi::vte_get_features();
        let features = if features.is_null() {
            String::new()
        } else {
            CStr::from_ptr(features).to_string_lossy().into_owned()
        };
        (
            (
                vte::ffi::vte_get_major_version(),
                vte::ffi::vte_get_minor_version(),
                vte::ffi::vte_get_micro_version(),
            ),
            features,
        )
    }
}

/// `sysname`, `release`, `version` and `machine` from `uname()`.
pub fn uname() -> Option<[String; 4]> {
    // SAFETY: utsname is plain old data and uname() fills it in.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid, writable utsname.
    if unsafe { libc::uname(&mut u) } != 0 {
        return None;
    }
    let field = |chars: &[c_char]| {
        // SAFETY: uname() NUL-terminates every field within its array.
        unsafe { CStr::from_ptr(chars.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    Some([
        field(&u.sysname),
        field(&u.release),
        field(&u.version),
        field(&u.machine),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_name_matches_agent() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "NAME=\"Fedora Linux\"\nVERSION=\"43\"\nPRETTY_NAME=\"Fedora Linux 43\"\n",
                Some("Fedora Linux"),
            ),
            // The first `NAME="` anywhere wins, as with strstr().
            (
                "PRETTY_NAME=\"Debian GNU/Linux 13\"\nNAME=\"Debian GNU/Linux\"\n",
                Some("Debian GNU/Linux 13"),
            ),
            ("NAME=Arch\n", None),
            ("NAME=\"unterminated\n", None),
            ("", None),
            ("NAME=\"\"\n", Some("")),
        ];
        for (contents, expected) in cases {
            assert_eq!(
                os_name_from_release(contents).as_deref(),
                *expected,
                "{contents:?}"
            );
        }
    }

    #[test]
    fn runtime_versions_are_plausible() {
        let (major, _, _) = glib_runtime_version();
        assert_eq!(major, 2);
        let ((vte_major, vte_minor, _), _) = vte_runtime_version();
        assert!(vte_major > 0 || vte_minor > 0);
        let [sysname, ..] = uname().expect("uname works");
        assert!(!sysname.is_empty());
    }

    #[test]
    fn system_paths_follow_sandbox() {
        assert_eq!(
            system_paths(false),
            (Path::new(VTE_SH_PATH), Path::new(OS_RELEASE_PATH))
        );
        assert_eq!(
            system_paths(true),
            (Path::new(HOST_VTE_SH_PATH), Path::new(HOST_OS_RELEASE_PATH))
        );
    }

    fn full_info() -> DebugInfo {
        DebugInfo {
            app_name: "tty-rex".into(),
            version: "0.1.0".into(),
            build_identifier: "v0.1.0-3-gabcdef0".into(),
            os_name: Some("Fedora Linux".into()),
            uname: Some([
                "Linux".into(),
                "6.17.1".into(),
                "#1 SMP".into(),
                "x86_64".into(),
            ]),
            in_sandbox: false,
            desktop_session: Some("gnome".into()),
            glib: LibraryVersion {
                runtime: (2, 86, 1),
                compiled: "2.86.0".into(),
            },
            gtk: LibraryVersion {
                runtime: (4, 20, 2),
                compiled: "4.20.1".into(),
            },
            vte: LibraryVersion {
                runtime: (0, 82, 1),
                compiled: "0.82.1".into(),
            },
            vte_features: "+BIDI +GNUTLS".into(),
            display: "GdkWaylandDisplay".into(),
            accessibility: true,
            gtk_theme: Some("Adwaita".into()),
            system_font: "Monospace 11".into(),
            font: None,
            windows: vec![
                WindowInfo {
                    n_tabs: 2,
                    renderer: "GskNglRenderer".into(),
                    scale: 1.25,
                    scale_factor: 2,
                    monitor: Some(MonitorInfo {
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                        refresh_rate: 60000,
                    }),
                },
                WindowInfo {
                    n_tabs: 1,
                    renderer: "GskCairoRenderer".into(),
                    scale: 1.0,
                    scale_factor: 1,
                    monitor: None,
                },
            ],
            development_build: false,
            app_id: "io.github.tasleson.TtyRex".into(),
            vte_sh_path: VTE_SH_PATH.into(),
            vte_sh_exists: true,
            containers: vec![
                ContainerInfo {
                    display_name: "fedora-toolbox-43".into(),
                    provider: "toolbox".into(),
                },
                ContainerInfo {
                    display_name: "box".into(),
                    provider: "distrobox".into(),
                },
            ],
            flatpak_info: None,
            os_release: Some("NAME=\"Fedora Linux\"\n".into()),
        }
    }

    #[test]
    fn renders_like_ptyxis() {
        let expected = "\
tty-rex 0.1.0 (v0.1.0-3-gabcdef0)

Operating System: Fedora Linux

uname.sysname = Linux
uname.release = 6.17.1
uname.version = #1 SMP
uname.machine = x86_64

Agent: none, spawning directly on host

Desktop Session: gnome

GLib: 2.86.1 (compiled against 2.86.0)
GTK: 4.20.2 (compiled against 4.20.1)
VTE: 0.82.1 (compiled against 0.82.1) +BIDI +GNUTLS

Display: GdkWaylandDisplay
Accessibility: Yes

GTK Theme: Adwaita
System Font: Monospace 11
Font: -- Using System Font --

window[0].n_tabs = 2
window[0].renderer = GskNglRenderer
window[0].scale = 1.250000
window[0].scale_factor = 2
window[0].monitor.geometry = 0,0 1920×1080
window[0].monitor.refresh_rate = 60000

window[1].n_tabs = 1
window[1].renderer = GskCairoRenderer
window[1].scale = 1.000000
window[1].scale_factor = 1

App ID: io.github.tasleson.TtyRex

/etc/profile.d/vte.sh exists

Containers:
  • fedora-toolbox-43 (toolbox)
  • box (distrobox)

NAME=\"Fedora Linux\"
";
        assert_eq!(full_info().render(), expected);
    }

    #[test]
    fn renders_optional_parts() {
        let info = DebugInfo {
            os_name: None,
            uname: None,
            in_sandbox: true,
            desktop_session: None,
            accessibility: false,
            gtk_theme: None,
            font: Some("Source Code Pro 12".into()),
            windows: Vec::new(),
            development_build: true,
            vte_sh_path: HOST_VTE_SH_PATH.into(),
            vte_sh_exists: false,
            containers: Vec::new(),
            flatpak_info: Some("[Application]\nname=x\n".into()),
            os_release: None,
            ..full_info()
        };
        let text = info.render();
        let expected_lines = [
            "Operating System: \n\n\nAgent: none, spawning directly in sandbox\n",
            "Desktop Session: Unspecified\n",
            "Accessibility: No\n",
            "GTK Theme: (null)\n",
            "Font: Source Code Pro 12\n\n** DEVELOPMENT BUILD **\n\nApp ID:",
            "/var/run/host/etc/profile.d/vte.sh missing\n",
            "Containers:\n\n[Application]\nname=x\n",
        ];
        for line in expected_lines {
            assert!(text.contains(line), "missing {line:?} in:\n{text}");
        }
        assert!(!text.contains("uname."));
        assert!(!text.contains("window["));
        assert!(text.ends_with("name=x\n"));
    }
}
