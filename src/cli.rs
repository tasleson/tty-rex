// cli.rs
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

//! Command-line handling (`src/main.c` and
//! `ptyxis_application_command_line()`).
//!
//! Options are parsed by `GApplication` rather than an external parser
//! such as clap: with `HANDLES_COMMAND_LINE` the primary instance receives
//! every invocation's options and working directory over D-Bus, which is
//! exactly what `ptyxis --tab` from another terminal relies on.

use std::path::{Path, PathBuf};

use gtk::glib;
use gtk::{gio, prelude::*};

/// Long option names, shared by registration and lookup.
pub mod option {
    pub const EXECUTE: &str = "execute";
    pub const FULLSCREEN: &str = "fullscreen";
    pub const IMPORT_PALETTE: &str = "import-palette";
    pub const MAXIMIZE: &str = "maximize";
    pub const NEW_WINDOW: &str = "new-window";
    pub const PREFERENCES: &str = "preferences";
    pub const STANDALONE: &str = "standalone";
    pub const TAB: &str = "tab";
    pub const TAB_WITH_PROFILE: &str = "tab-with-profile";
    pub const TITLE: &str = "title";
    pub const WORKING_DIRECTORY: &str = "working-directory";
}

struct OptionSpec {
    long: &'static str,
    short: Option<u8>,
    arg: glib::OptionArg,
    description: &'static str,
    arg_description: Option<&'static str>,
}

const OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        long: option::PREFERENCES,
        short: None,
        arg: glib::OptionArg::None,
        description: "Show the application preferences",
        arg_description: None,
    },
    OptionSpec {
        long: option::WORKING_DIRECTORY,
        short: Some(b'd'),
        arg: glib::OptionArg::Filename,
        description: "Use DIR for --tab, --tab-with-profile, --new-window, or -x",
        arg_description: Some("DIR"),
    },
    OptionSpec {
        long: option::EXECUTE,
        short: Some(b'x'),
        arg: glib::OptionArg::String,
        description: "Command to execute in new window",
        arg_description: None,
    },
    OptionSpec {
        long: option::NEW_WINDOW,
        short: None,
        arg: glib::OptionArg::None,
        description: "New terminal window",
        arg_description: None,
    },
    OptionSpec {
        long: option::TAB,
        short: None,
        arg: glib::OptionArg::None,
        description: "New terminal tab in active window",
        arg_description: None,
    },
    OptionSpec {
        long: option::TAB_WITH_PROFILE,
        short: None,
        arg: glib::OptionArg::String,
        description: "New terminal tab in active window using the profile UUID",
        arg_description: Some("PROFILE_UUID"),
    },
    OptionSpec {
        long: option::STANDALONE,
        short: Some(b's'),
        arg: glib::OptionArg::None,
        description: "Start a new instance, ignoring existing instances",
        arg_description: None,
    },
    OptionSpec {
        long: option::TITLE,
        short: Some(b'T'),
        arg: glib::OptionArg::String,
        description: "Set title for new tab",
        arg_description: None,
    },
    OptionSpec {
        long: option::MAXIMIZE,
        short: None,
        arg: glib::OptionArg::None,
        description: "Maximize a newly created window",
        arg_description: None,
    },
    OptionSpec {
        long: option::FULLSCREEN,
        short: None,
        arg: glib::OptionArg::None,
        description: "Fullscreen a newly created window",
        arg_description: None,
    },
    // Handled before GApplication runs (see `preprocess_args`); registered
    // so it shows up in --help like in the C version.
    OptionSpec {
        long: option::IMPORT_PALETTE,
        short: None,
        arg: glib::OptionArg::String,
        description: "Import a Ptyxis palette file",
        arg_description: Some("FILE"),
    },
];

const PARAMETER_STRING: &str = "[-- COMMAND ARGUMENTS]";
const SUMMARY: &str = "Examples:

  Run Separate Instance
    tty-rex -s

  Open Preferences
    tty-rex --preferences

  Run Custom Command in New Window
    tty-rex -x \"bash -c 'sleep 3'\"
    tty-rex -- bash -c 'sleep 3'

  Import a custom palette
    tty-rex --import-palette my.palette";

/// Registers the supported options on the application.
pub fn register_options(application: &impl IsA<gio::Application>) {
    for spec in OPTIONS {
        application.add_main_option(
            spec.long,
            glib::Char::from(spec.short.unwrap_or(0)),
            glib::OptionFlags::NONE,
            spec.arg,
            spec.description,
            spec.arg_description,
        );
    }
    application.set_option_context_parameter_string(Some(PARAMETER_STRING));
    application.set_option_context_summary(Some(SUMMARY));
}

/// Arguments after early processing, as `check_early_opts()` produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarlyArgs {
    pub args: Vec<String>,
    /// Run as a separate, non-unique instance.
    pub standalone: bool,
    /// `--import-palette FILE`: copy the file and exit without starting.
    pub import_palette: Option<PathBuf>,
}

/// Rewrites `-e`/`--` into `-x` with a shell-quoted command and decides
/// whether to run standalone: `-x` implies a separate instance unless
/// `--tab`, `--new-window` or `--tab-with-profile` ask for a shared one.
pub fn preprocess_args(args: Vec<String>) -> EarlyArgs {
    let mut out = Vec::with_capacity(args.len());
    let mut wants_shared = false;
    let mut wants_standalone = false;
    let mut explicit_standalone = false;
    let mut import_palette = None;
    let import_flag = format!("--{}", option::IMPORT_PALETTE);
    let import_prefix = format!("{import_flag}=");

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == import_flag {
            import_palette = iter.next().map(PathBuf::from);
            continue;
        }
        if let Some(path) = arg.strip_prefix(&import_prefix) {
            import_palette = Some(PathBuf::from(path));
            continue;
        }

        match arg.as_str() {
            "--tab" | "--new-window" | "--tab-with-profile" => wants_shared = true,
            a if a.starts_with("--tab-with-profile=") => wants_shared = true,
            "-x" | "--execute" => wants_standalone = true,
            a if a.starts_with("--execute=") => wants_standalone = true,
            "-s" | "--standalone" => explicit_standalone = true,
            _ => {}
        }

        // `-e` is accepted for x-terminal-emulator compatibility.
        if arg == "--" || arg == "-e" {
            let rest: Vec<String> = iter.by_ref().collect();
            if rest.is_empty() {
                out.push(arg);
                break;
            }
            let command = rest
                .iter()
                .map(|a| glib::shell_quote(a).to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            out.push("-x".to_owned());
            out.push(command);
            wants_standalone = true;
            break;
        }
        out.push(arg);
    }

    EarlyArgs {
        args: out,
        standalone: explicit_standalone || (wants_standalone && !wants_shared),
        import_palette,
    }
}

/// Options relevant to the port, extracted from the options dictionary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLineOptions {
    pub preferences: bool,
    pub tab: bool,
    pub new_window: bool,
    pub tab_with_profile: Option<String>,
    pub execute: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub title: Option<String>,
    pub maximize: bool,
    pub fullscreen: bool,
}

impl CommandLineOptions {
    pub fn from_dict(dict: &glib::VariantDict) -> Self {
        let string = |name| dict.lookup::<String>(name).ok().flatten();
        Self {
            preferences: dict.contains(option::PREFERENCES),
            tab: dict.contains(option::TAB),
            new_window: dict.contains(option::NEW_WINDOW),
            tab_with_profile: string(option::TAB_WITH_PROFILE),
            execute: string(option::EXECUTE),
            working_directory: dict
                .lookup::<PathBuf>(option::WORKING_DIRECTORY)
                .ok()
                .flatten(),
            title: string(option::TITLE).filter(|t| !t.is_empty()),
            maximize: dict.contains(option::MAXIMIZE),
            fullscreen: dict.contains(option::FULLSCREEN),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandLineError {
    #[error("--tab, --tab-with-profile, or --new-window may not be used together")]
    ConflictingTargets,
    #[error("Cannot parse command: {0}")]
    InvalidCommand(String),
}

/// Where a requested tab should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowTarget {
    /// The focused window, or a new one if there is none.
    Current,
    New,
}

/// What a command line asks the application to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLineAction {
    /// Show the preferences window (takes precedence like in C).
    Preferences,
    /// Present an existing window or open the default one.
    Activate { title: Option<String> },
    OpenTab {
        target: WindowTarget,
        profile_uuid: Option<String>,
        command: Option<Vec<String>>,
        cwd_uri: Option<String>,
        title: Option<String>,
        /// Whether `--maximize`/`--fullscreen` apply to the window the tab
        /// lands in, which Ptyxis only does for `-x` and `--new-window`
        /// (`maybe_maximize_or_fullscreen()`), never for `--tab` or
        /// `--tab-with-profile`.
        applies_window_state: bool,
    },
}

/// How a window opened from the command line should be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Maximized,
    Fullscreen,
}

/// `--maximize`/`--fullscreen` waiting for the next window that
/// `maybe_maximize_or_fullscreen()` is called for (the `maximize` and
/// `fullscreen` bits of `PtyxisApplication`).
///
/// Like Ptyxis, an option only sets its bit and never clears it, so a
/// request that opened no window (say `--tab --maximize`) stays pending
/// for the next window a later command line or activation opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PendingWindowState {
    maximize: bool,
    fullscreen: bool,
}

impl PendingWindowState {
    /// Records the options of a command line. Returns true when both are
    /// now pending, in which case fullscreen wins and the caller should
    /// warn as Ptyxis does.
    pub fn update(&mut self, options: &CommandLineOptions) -> bool {
        self.maximize |= options.maximize;
        self.fullscreen |= options.fullscreen;
        let conflict = self.maximize && self.fullscreen;
        if conflict {
            self.maximize = false;
        }
        conflict
    }

    /// Consumes the pending state for a window about to be presented.
    pub fn take(&mut self) -> Option<WindowState> {
        let state = if self.fullscreen {
            Some(WindowState::Fullscreen)
        } else if self.maximize {
            Some(WindowState::Maximized)
        } else {
            None
        };
        *self = Self::default();
        state
    }
}

/// Converts `--working-directory` (or the caller's cwd) into a URI the
/// way `ptyxis_application_command_line()` does. Relative paths resolve
/// against the caller's cwd, or `home` if that is unknown.
pub fn working_directory_uri(
    working_directory: Option<&Path>,
    caller_cwd: Option<&Path>,
    home: &Path,
) -> Option<String> {
    let caller_cwd = caller_cwd.filter(|p| !p.as_os_str().is_empty());
    let dir = working_directory.or(caller_cwd)?;

    if let Some(s) = dir.to_str()
        && glib::Uri::peek_scheme(s).is_some()
    {
        return Some(s.to_owned());
    }
    let path = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        caller_cwd.unwrap_or(home).join(dir)
    };
    // GFile canonicalizes `.`/`..` and escapes the path properly.
    Some(gio::File::for_path(path).uri().into())
}

/// Decides what to do for a command line, mirroring the branches of
/// `ptyxis_application_command_line()` (without session restore).
pub fn plan(
    options: &CommandLineOptions,
    caller_cwd: Option<&Path>,
    home: &Path,
) -> Result<CommandLineAction, CommandLineError> {
    if options.tab && options.new_window {
        return Err(CommandLineError::ConflictingTargets);
    }

    let cwd_uri = working_directory_uri(options.working_directory.as_deref(), caller_cwd, home);
    let shared_target = options.tab || options.tab_with_profile.is_some();
    let target = if shared_target && !options.new_window {
        WindowTarget::Current
    } else {
        WindowTarget::New
    };
    let title = options.title.clone();
    let profile_uuid = options.tab_with_profile.clone();
    // ptyxis_application_command_line() checks --tab before
    // --tab-with-profile before --new-window, and only the --new-window
    // and plain -x branches maximize or fullscreen.
    let applies_window_state = !options.tab && profile_uuid.is_none();

    if options.preferences {
        return Ok(CommandLineAction::Preferences);
    }

    if let Some(command) = &options.execute {
        let argv = glib::shell_parse_argv(command.as_str())
            .map_err(|e| CommandLineError::InvalidCommand(e.message().to_owned()))?
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        return Ok(CommandLineAction::OpenTab {
            target,
            profile_uuid,
            command: Some(argv),
            cwd_uri,
            title,
            applies_window_state,
        });
    }

    if shared_target || options.new_window {
        return Ok(CommandLineAction::OpenTab {
            target,
            profile_uuid,
            command: None,
            cwd_uri,
            title,
            applies_window_state,
        });
    }

    Ok(CommandLineAction::Activate { title })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn early_args() {
        let cases: &[(&[&str], &[&str], bool)] = &[
            (&["tty-rex"], &["tty-rex"], false),
            (&["tty-rex", "-s"], &["tty-rex", "-s"], true),
            (&["tty-rex", "-x", "top"], &["tty-rex", "-x", "top"], true),
            (
                &["tty-rex", "--execute=top"],
                &["tty-rex", "--execute=top"],
                true,
            ),
            (
                &["tty-rex", "--tab", "-x", "top"],
                &["tty-rex", "--tab", "-x", "top"],
                false,
            ),
            (
                &["tty-rex", "--", "bash", "-c", "sleep 3"],
                &["tty-rex", "-x", "'bash' '-c' 'sleep 3'"],
                true,
            ),
            (
                &["tty-rex", "-e", "vim", "my file"],
                &["tty-rex", "-x", "'vim' 'my file'"],
                true,
            ),
            (
                &["tty-rex", "--new-window", "--", "htop"],
                &["tty-rex", "--new-window", "-x", "'htop'"],
                false,
            ),
            (
                &["tty-rex", "--tab-with-profile=abc", "--", "htop"],
                &["tty-rex", "--tab-with-profile=abc", "-x", "'htop'"],
                false,
            ),
            (&["tty-rex", "--"], &["tty-rex", "--"], false),
        ];
        for (input, args, standalone) in cases {
            let early = preprocess_args(strings(input));
            assert_eq!(early.args, strings(args), "{input:?}");
            assert_eq!(early.standalone, *standalone, "{input:?}");
        }
    }

    #[test]
    fn import_palette_is_extracted() {
        let cases: &[(&[&str], &[&str], Option<&str>)] = &[
            (
                &["tty-rex", "--import-palette", "a.palette"],
                &["tty-rex"],
                Some("a.palette"),
            ),
            (
                &["tty-rex", "--import-palette=b.palette", "-s"],
                &["tty-rex", "-s"],
                Some("b.palette"),
            ),
            (
                &["tty-rex", "--", "echo", "--import-palette"],
                &["tty-rex", "-x", "'echo' '--import-palette'"],
                None,
            ),
            (&["tty-rex", "--tab"], &["tty-rex", "--tab"], None),
        ];
        for (input, args, palette) in cases {
            let early = preprocess_args(strings(input));
            assert_eq!(early.args, strings(args), "{input:?}");
            assert_eq!(
                early.import_palette.as_deref(),
                palette.map(Path::new),
                "{input:?}"
            );
        }
    }

    #[test]
    fn working_directories() {
        let home = Path::new("/home/user");
        let cases: &[(Option<&str>, Option<&str>, Option<&str>)] = &[
            (None, None, None),
            (None, Some("/srv"), Some("file:///srv")),
            (Some("/tmp/a b"), Some("/srv"), Some("file:///tmp/a%20b")),
            (Some("src"), Some("/srv"), Some("file:///srv/src")),
            (Some("src/../lib"), Some("/srv"), Some("file:///srv/lib")),
            (Some("src"), None, Some("file:///home/user/src")),
            (Some("src"), Some(""), Some("file:///home/user/src")),
            (Some("sftp://host/dir"), None, Some("sftp://host/dir")),
        ];
        for (wd, cwd, expected) in cases {
            assert_eq!(
                working_directory_uri(wd.map(Path::new), cwd.map(Path::new), home).as_deref(),
                *expected,
                "{wd:?} {cwd:?}"
            );
        }
    }

    #[test]
    fn plans() {
        let home = Path::new("/home/user");
        let cwd = Some(Path::new("/srv"));
        let cwd_uri = Some("file:///srv".to_owned());
        let opts = CommandLineOptions::default;
        let tab = |target,
                   profile_uuid: Option<&str>,
                   command: Option<&[&str]>,
                   applies_window_state| CommandLineAction::OpenTab {
            target,
            profile_uuid: profile_uuid.map(str::to_owned),
            command: command.map(strings),
            cwd_uri: cwd_uri.clone(),
            title: None,
            applies_window_state,
        };
        use WindowTarget::{Current, New};

        let cases = [
            (opts(), CommandLineAction::Activate { title: None }),
            (
                CommandLineOptions {
                    preferences: true,
                    tab: true,
                    ..opts()
                },
                CommandLineAction::Preferences,
            ),
            (
                CommandLineOptions {
                    title: Some("Logs".into()),
                    ..opts()
                },
                CommandLineAction::Activate {
                    title: Some("Logs".into()),
                },
            ),
            (
                CommandLineOptions {
                    tab: true,
                    ..opts()
                },
                tab(Current, None, None, false),
            ),
            (
                CommandLineOptions {
                    new_window: true,
                    ..opts()
                },
                tab(New, None, None, true),
            ),
            (
                CommandLineOptions {
                    tab_with_profile: Some("p".into()),
                    ..opts()
                },
                tab(Current, Some("p"), None, false),
            ),
            (
                CommandLineOptions {
                    tab_with_profile: Some("p".into()),
                    new_window: true,
                    ..opts()
                },
                tab(New, Some("p"), None, false),
            ),
            (
                CommandLineOptions {
                    execute: Some("htop -d 5".into()),
                    ..opts()
                },
                tab(New, None, Some(&["htop", "-d", "5"]), true),
            ),
            (
                CommandLineOptions {
                    execute: Some("htop".into()),
                    tab: true,
                    ..opts()
                },
                tab(Current, None, Some(&["htop"]), false),
            ),
            (
                CommandLineOptions {
                    execute: Some("htop".into()),
                    new_window: true,
                    tab_with_profile: Some("p".into()),
                    ..opts()
                },
                tab(New, Some("p"), Some(&["htop"]), false),
            ),
            (
                CommandLineOptions {
                    execute: Some("htop".into()),
                    new_window: true,
                    ..opts()
                },
                tab(New, None, Some(&["htop"]), true),
            ),
        ];
        for (options, expected) in cases {
            assert_eq!(plan(&options, cwd, home), Ok(expected), "{options:?}");
        }
    }

    #[test]
    fn pending_window_state() {
        use WindowState::{Fullscreen, Maximized};
        let opts = |maximize, fullscreen| CommandLineOptions {
            maximize,
            fullscreen,
            ..Default::default()
        };
        // (maximize, fullscreen) of one command line.
        type Line = (bool, bool);
        // Each case feeds command lines in order, then takes the state.
        let cases: &[(&[Line], bool, Option<WindowState>)] = &[
            (&[], false, None),
            (&[(false, false)], false, None),
            (&[(true, false)], false, Some(Maximized)),
            (&[(false, true)], false, Some(Fullscreen)),
            (&[(true, true)], true, Some(Fullscreen)),
            // An unconsumed request carries over to the next command line.
            (&[(true, false), (false, false)], false, Some(Maximized)),
            (&[(true, false), (false, true)], true, Some(Fullscreen)),
            (&[(false, true), (true, false)], true, Some(Fullscreen)),
        ];
        for (lines, conflict, expected) in cases {
            let mut pending = PendingWindowState::default();
            let conflicts = lines
                .iter()
                .map(|&(m, f)| pending.update(&opts(m, f)))
                .collect::<Vec<_>>();
            assert_eq!(
                conflicts.last().copied().unwrap_or(false),
                *conflict,
                "{lines:?}"
            );
            assert_eq!(pending.take(), *expected, "{lines:?}");
            assert_eq!(pending.take(), None, "{lines:?} taken twice");
        }
    }

    #[test]
    fn plan_errors() {
        let home = Path::new("/home/user");
        let conflicting = CommandLineOptions {
            tab: true,
            new_window: true,
            ..Default::default()
        };
        assert_eq!(
            plan(&conflicting, None, home),
            Err(CommandLineError::ConflictingTargets)
        );
        let invalid = CommandLineOptions {
            execute: Some("echo 'oops".into()),
            ..Default::default()
        };
        assert!(matches!(
            plan(&invalid, None, home),
            Err(CommandLineError::InvalidCommand(_))
        ));
    }

    #[test]
    fn options_from_dict() {
        let dict = glib::VariantDict::new(None);
        dict.insert(option::TAB, true);
        dict.insert(option::EXECUTE, "top");
        dict.insert(option::TITLE, "");
        dict.insert(option::FULLSCREEN, true);
        dict.insert_value(
            option::WORKING_DIRECTORY,
            &PathBuf::from("/tmp").to_variant(),
        );
        let options = CommandLineOptions::from_dict(&dict);
        assert_eq!(
            options,
            CommandLineOptions {
                tab: true,
                execute: Some("top".into()),
                working_directory: Some(PathBuf::from("/tmp")),
                fullscreen: true,
                ..Default::default()
            }
        );
    }
}
