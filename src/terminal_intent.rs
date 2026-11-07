// terminal_intent.rs
//
// Copyright 2025 Christian Hergert <christian@sourceandstack.com>
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

//! The `org.freedesktop.Terminal1` interface, through which file managers
//! ("Open in Terminal") and `xdg-terminal-exec` ask a terminal to open a
//! window (`src/ptyxis-terminal-intent.c`).
//!
//! It is exported next to GApplication's own interfaces on the
//! application's object path (see [`crate::application`]), and the
//! desktop file announces it with `Implements=`. Each `LaunchCommand` call
//! opens a new window with a tab per command, or a single shell tab when
//! no command is given.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::app::TabRequest;

/// The D-Bus interface name, also listed under `Implements=` in the
/// desktop file.
pub const INTERFACE_NAME: &str = "org.freedesktop.Terminal1";

/// `src/org.freedesktop.Terminal1.xml`.
const INTERFACE_XML: &str = r#"<node>
  <interface name="org.freedesktop.Terminal1">
    <method name="LaunchCommand">
      <arg type='aa{sv}' name='commands' direction='in' />
      <arg type='ay' name='desktop_entry' direction='in' />
      <arg type='a{sv}' name='options' direction='in' />
      <arg type='a{sv}' name='platform_data' direction='in' />
    </method>
  </interface>
</node>"#;

const LAUNCH_COMMAND_METHOD: &str = "LaunchCommand";
const LAUNCH_COMMAND_SIGNATURE: &str = "(aa{sv}aya{sv}a{sv})";

/// Keys of each command's dictionary.
const EXEC_KEY: &str = "exec";
const ENV_KEY: &str = "env";
const WORKING_DIRECTORY_KEY: &str = "working_directory";
/// Key of the options dictionary.
const KEEP_TERMINAL_OPEN_OPTION: &str = "keep-terminal-open";

/// The desktop entry key naming the directory to run in.
const DESKTOP_ENTRY_PATH_KEY: &str = "Path";

const ENVIRON_PAIR_ERROR: &str = "environ pair missing `=`";
const INVALID_DESKTOP_ENTRY_ERROR: &str = "Not a valid path to a Desktop Entry";

/// One of the commands to open a tab for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Command {
    /// The program to run instead of the profile's shell.
    pub argv: Option<Vec<String>>,
    /// `KEY=VALUE` pairs for the program.
    pub environment: Option<Vec<String>>,
    /// Where to run, as a URI.
    pub cwd_uri: Option<String>,
}

/// A parsed `LaunchCommand` call.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    pub commands: Vec<Command>,
    /// The desktop entry of the application being launched, if any.
    pub desktop_entry: Option<PathBuf>,
    pub keep_terminal_open: bool,
}

impl gio::prelude::DBusMethodCall for LaunchCommand {
    fn parse_call(
        _obj_path: &str,
        _interface: Option<&str>,
        method: &str,
        params: glib::Variant,
    ) -> Result<Self, glib::Error> {
        if method != LAUNCH_COMMAND_METHOD {
            return Err(glib::Error::new(
                gio::DBusError::UnknownMethod,
                &format!("Unknown method {method}"),
            ));
        }
        parse_launch_command(&params, &glib::home_dir())
    }
}

/// What the call's desktop entry contributes to its tabs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// The application's name, which becomes the title of its tabs.
    pub name: Option<String>,
    /// Its `Path=`, as a URI, for commands that name no directory.
    pub cwd_uri: Option<String>,
}

fn invalid_args(message: &str) -> glib::Error {
    glib::Error::new(gio::DBusError::InvalidArgs, message)
}

/// The string of a byte string variant, like `g_variant_get_bytestring()`:
/// up to the first NUL, or empty when the array is not NUL-terminated.
fn bytestring(bytes: &[u8]) -> &[u8] {
    match bytes.last() {
        Some(0) => bytes.split(|&b| b == 0).next().unwrap_or_default(),
        _ => &[],
    }
}

fn byte_array(variant: &glib::Variant) -> &[u8] {
    bytestring(variant.fixed_array::<u8>().unwrap_or_default())
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `aay` as strings, like the `^aay` format.
fn string_array(variant: &glib::Variant) -> Vec<String> {
    variant
        .iter()
        .map(|item| lossy(byte_array(&item)))
        .collect()
}

/// The value of `key` in the `a{sv}` `dict` if it has type `ty`, like
/// `g_variant_lookup()`.
fn lookup(dict: &glib::Variant, key: &str, ty: &glib::VariantTy) -> Option<glib::Variant> {
    glib::VariantDict::new(Some(dict)).lookup_value(key, Some(ty))
}

/// `path` relative to `home` unless absolute, as a URI.
fn directory_uri(path: &Path, home: &Path) -> String {
    gio::File::for_path(home.join(path)).uri().into()
}

/// `unpack_commands()`.
fn unpack_command(command: &glib::Variant, home: &Path) -> Result<Command, glib::Error> {
    let argv = lookup(command, EXEC_KEY, glib::VariantTy::BYTE_STRING_ARRAY)
        .map(|v| string_array(&v))
        .filter(|argv| argv.first().is_some_and(|arg0| !arg0.is_empty()));
    let environment = lookup(command, ENV_KEY, glib::VariantTy::BYTE_STRING_ARRAY)
        .map(|v| string_array(&v))
        .filter(|env| !env.is_empty());
    if environment.iter().flatten().any(|pair| !pair.contains('=')) {
        return Err(invalid_args(ENVIRON_PAIR_ERROR));
    }
    let cwd_uri = lookup(command, WORKING_DIRECTORY_KEY, glib::VariantTy::BYTE_STRING)
        .map(|v| byte_array(&v).to_vec())
        .filter(|dir| !dir.is_empty())
        .map(|dir: Vec<u8>| directory_uri(Path::new(OsStr::from_bytes(&dir)), home));
    Ok(Command {
        argv,
        environment,
        cwd_uri,
    })
}

/// Parses the arguments of `LaunchCommand`; relative directories are
/// taken from `home`.
fn parse_launch_command(params: &glib::Variant, home: &Path) -> Result<LaunchCommand, glib::Error> {
    if params.type_().as_str() != LAUNCH_COMMAND_SIGNATURE {
        return Err(invalid_args(&format!(
            "Expected {LAUNCH_COMMAND_SIGNATURE}, got {}",
            params.type_()
        )));
    }
    let commands = params
        .child_value(0)
        .iter()
        .map(|command| unpack_command(&command, home))
        .collect::<Result<_, _>>()?;
    let desktop_entry = Some(byte_array(&params.child_value(1)))
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(OsStr::from_bytes(path)));
    let keep_terminal_open = lookup(
        &params.child_value(2),
        KEEP_TERMINAL_OPEN_OPTION,
        glib::VariantTy::BOOLEAN,
    )
    .and_then(|v| v.get::<bool>())
    .unwrap_or(false);
    Ok(LaunchCommand {
        commands,
        desktop_entry,
        keep_terminal_open,
    })
}

/// Reads the name and directory of the desktop entry at `path`
/// (`unpack_desktop_entry()`).
pub fn read_desktop_entry(path: Option<&Path>, home: &Path) -> Result<DesktopEntry, glib::Error> {
    let Some(path) = path else {
        return Ok(DesktopEntry::default());
    };
    let info = gio_unix::DesktopAppInfo::from_filename(path)
        .ok_or_else(|| invalid_args(INVALID_DESKTOP_ENTRY_ERROR))?;
    Ok(DesktopEntry {
        name: Some(info.name().into()),
        cwd_uri: info
            .string(DESKTOP_ENTRY_PATH_KEY)
            .map(|dir| directory_uri(Path::new(dir.as_str()), home)),
    })
}

/// The tabs to open for `call`, the first of which is selected
/// (`ptyxis_terminal_intent_handle_launch_command()`). Without commands
/// that is a single tab running the default profile's shell.
pub fn tab_requests(call: LaunchCommand, entry: DesktopEntry) -> Vec<TabRequest> {
    let tab = |command: Command| TabRequest {
        command: command.argv,
        environment: command.environment.unwrap_or_default(),
        initial_cwd_uri: command.cwd_uri.or_else(|| entry.cwd_uri.clone()),
        // The application's name replaces the titles it sets.
        title_prefix: entry.name.clone(),
        ignore_osc_title: entry.name.is_some(),
        keep_terminal_open: call.keep_terminal_open,
        ..Default::default()
    };
    if call.commands.is_empty() {
        vec![tab(Command::default())]
    } else {
        call.commands.into_iter().map(tab).collect()
    }
}

/// Exports the interface on `connection` at `object_path`, handing each
/// call to `launch`.
pub fn export(
    connection: &gio::DBusConnection,
    object_path: &str,
    launch: impl Fn(LaunchCommand) -> Result<(), glib::Error> + 'static,
) -> Result<gio::RegistrationId, glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(INTERFACE_XML)?;
    let interface = node
        .lookup_interface(INTERFACE_NAME)
        .expect("the interface XML defines org.freedesktop.Terminal1");
    connection
        .register_object(object_path, &interface)
        .typed_method_call::<LaunchCommand>()
        .invoke_and_return(move |_, _, call| launch(call).map(|()| None))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const HOME: &str = "/home/user";

    fn bytes(s: &str) -> glib::Variant {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        glib::Variant::array_from_fixed_array(&bytes)
    }

    fn byte_strings(strings: &[&str]) -> glib::Variant {
        glib::Variant::array_from_iter_with_type(
            glib::VariantTy::BYTE_STRING,
            strings.iter().map(|s| bytes(s)),
        )
    }

    fn dict(entries: &[(&str, glib::Variant)]) -> glib::Variant {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect::<HashMap<String, glib::Variant>>()
            .to_variant()
    }

    fn params(
        commands: &[glib::Variant],
        desktop_entry: &str,
        options: glib::Variant,
    ) -> glib::Variant {
        let commands = glib::Variant::array_from_iter_with_type(
            glib::VariantTy::VARDICT,
            commands.iter().cloned(),
        );
        glib::Variant::tuple_from_iter([commands, bytes(desktop_entry), options, dict(&[])])
    }

    fn strings(strings: &[&str]) -> Vec<String> {
        strings.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn bytestrings_need_a_terminator() {
        let cases: [(&[u8], &[u8]); 5] = [
            (b"", b""),
            (b"abc", b""),
            (b"abc\0", b"abc"),
            (b"a\0bc\0", b"a"),
            (b"\0", b""),
        ];
        for (input, expected) in cases {
            assert_eq!(bytestring(input), expected, "{input:?}");
        }
    }

    #[test]
    fn unpacks_commands() {
        let cases = [
            ("empty", dict(&[]), Command::default()),
            (
                "everything",
                dict(&[
                    (EXEC_KEY, byte_strings(&["vim", "a b"])),
                    (ENV_KEY, byte_strings(&["A=1", "B="])),
                    (WORKING_DIRECTORY_KEY, bytes("/tmp/x")),
                ]),
                Command {
                    argv: Some(strings(&["vim", "a b"])),
                    environment: Some(strings(&["A=1", "B="])),
                    cwd_uri: Some("file:///tmp/x".to_owned()),
                },
            ),
            (
                "empty argv and environment are none",
                dict(&[
                    (EXEC_KEY, byte_strings(&[])),
                    (ENV_KEY, byte_strings(&[])),
                    (WORKING_DIRECTORY_KEY, bytes("")),
                ]),
                Command::default(),
            ),
            (
                "an empty program is none",
                dict(&[(EXEC_KEY, byte_strings(&["", "x"]))]),
                Command::default(),
            ),
            (
                "relative directories are in the home directory",
                dict(&[(WORKING_DIRECTORY_KEY, bytes("src/../docs"))]),
                Command {
                    cwd_uri: Some("file:///home/user/docs".to_owned()),
                    ..Default::default()
                },
            ),
            (
                "values of the wrong type are ignored",
                dict(&[(EXEC_KEY, "vim".to_variant())]),
                Command::default(),
            ),
        ];
        for (name, command, expected) in cases {
            let call = parse_launch_command(&params(&[command], "", dict(&[])), Path::new(HOME))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(call.commands, [expected], "{name}");
        }
    }

    #[test]
    fn rejects_environment_without_equals() {
        let command = dict(&[(ENV_KEY, byte_strings(&["A=1", "B"]))]);
        let err =
            parse_launch_command(&params(&[command], "", dict(&[])), Path::new(HOME)).unwrap_err();
        assert!(err.matches(gio::DBusError::InvalidArgs), "{err}");
        assert_eq!(err.message(), ENVIRON_PAIR_ERROR);
    }

    #[test]
    fn rejects_wrong_signature() {
        let err = parse_launch_command(&"x".to_variant(), Path::new(HOME)).unwrap_err();
        assert!(err.matches(gio::DBusError::InvalidArgs), "{err}");
    }

    #[test]
    fn parses_desktop_entry_and_options() {
        let cases = [
            ("", dict(&[]), None, false),
            (
                "/usr/share/applications/htop.desktop",
                dict(&[(KEEP_TERMINAL_OPEN_OPTION, true.to_variant())]),
                Some(PathBuf::from("/usr/share/applications/htop.desktop")),
                true,
            ),
            (
                "",
                dict(&[(KEEP_TERMINAL_OPEN_OPTION, "yes".to_variant())]),
                None,
                false,
            ),
        ];
        for (desktop_entry, options, expected_entry, expected_keep) in cases {
            let call = parse_launch_command(&params(&[], desktop_entry, options), Path::new(HOME))
                .unwrap();
            assert!(call.commands.is_empty());
            assert_eq!(call.desktop_entry, expected_entry, "{desktop_entry}");
            assert_eq!(call.keep_terminal_open, expected_keep, "{desktop_entry}");
        }
    }

    #[test]
    fn reads_desktop_entries() {
        let dir = std::env::temp_dir().join(format!(
            "tty-rex-terminal-intent-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = |name: &str, contents: &str| {
            let path = dir.join(name);
            std::fs::write(&path, contents).unwrap();
            path
        };
        let home = Path::new(HOME);
        let cases = [
            (
                entry(
                    "absolute.desktop",
                    "[Desktop Entry]\nType=Application\nName=Top\nExec=top\nPath=/var/tmp\n",
                ),
                Some(DesktopEntry {
                    name: Some("Top".to_owned()),
                    cwd_uri: Some("file:///var/tmp".to_owned()),
                }),
            ),
            (
                entry(
                    "relative.desktop",
                    "[Desktop Entry]\nType=Application\nName=Top\nExec=top\nPath=work\n",
                ),
                Some(DesktopEntry {
                    name: Some("Top".to_owned()),
                    cwd_uri: Some("file:///home/user/work".to_owned()),
                }),
            ),
            (
                entry(
                    "no-path.desktop",
                    "[Desktop Entry]\nType=Application\nName=Top\nExec=top\n",
                ),
                Some(DesktopEntry {
                    name: Some("Top".to_owned()),
                    cwd_uri: None,
                }),
            ),
            (
                entry("link.desktop", "[Desktop Entry]\nType=Link\nName=Top\n"),
                None,
            ),
            (dir.join("missing.desktop"), None),
        ];
        for (path, expected) in cases {
            let result = read_desktop_entry(Some(&path), home);
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), expected, "{path:?}"),
                None => {
                    let err = result.unwrap_err();
                    assert!(err.matches(gio::DBusError::InvalidArgs), "{path:?}: {err}");
                }
            }
        }
        assert_eq!(
            read_desktop_entry(None, home).unwrap(),
            DesktopEntry::default()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plans_tabs() {
        let entry = DesktopEntry {
            name: Some("Top".to_owned()),
            cwd_uri: Some("file:///var/tmp".to_owned()),
        };
        let call = LaunchCommand {
            commands: vec![
                Command {
                    argv: Some(strings(&["top"])),
                    environment: Some(strings(&["A=1"])),
                    cwd_uri: None,
                },
                Command {
                    cwd_uri: Some("file:///srv".to_owned()),
                    ..Default::default()
                },
            ],
            desktop_entry: None,
            keep_terminal_open: true,
        };
        let tabs = tab_requests(call, entry.clone());
        let summary: Vec<_> = tabs
            .iter()
            .map(|t| {
                (
                    t.command.clone(),
                    t.environment.clone(),
                    t.initial_cwd_uri.as_deref(),
                    t.title_prefix.as_deref(),
                    t.keep_terminal_open,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    Some(strings(&["top"])),
                    strings(&["A=1"]),
                    Some("file:///var/tmp"),
                    Some("Top"),
                    true
                ),
                (None, vec![], Some("file:///srv"), Some("Top"), true),
            ]
        );
        assert!(tabs.iter().all(|t| t.ignore_osc_title));

        let tabs = tab_requests(LaunchCommand::default(), DesktopEntry::default());
        assert_eq!(tabs.len(), 1);
        let tab = &tabs[0];
        assert!(tab.command.is_none() && tab.initial_cwd_uri.is_none());
        assert!(tab.title_prefix.is_none() && !tab.ignore_osc_title);
        assert!(!tab.keep_terminal_open);
    }
}
