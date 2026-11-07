// spawn.rs
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

//! Process spawning behind a backend boundary.
//!
//! The C implementation spawns everything through `ptyxis-agent` over
//! D-Bus (`agent/org.gnome.Ptyxis.Agent.xml`), which also handles
//! containers. The port currently ships only [`HostSpawner`], which runs
//! the user's shell through VTE on the host, or in a container by
//! wrapping it in a host command (see [`crate::container`]), but tabs
//! only talk to the [`Spawner`] trait so an agent-backed implementation
//! can replace it without touching the UI.
//!
//! Command-line construction mirrors `ptyxis_client_spawn_async()` in
//! `src/ptyxis-client.c` and is kept pure so it can be unit tested.
//!
//! An agent-backed spawner will need more than the trait carries today:
//! a way to cancel an in-flight spawn when the tab closes first.

use std::cell::{Cell, OnceCell};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use gtk::{gio, glib};
use vte::prelude::*;

use crate::config;
use crate::container::{self, Container, ExecContext};
use crate::proxy;
use crate::settings::PreserveDirectory;

/// Used when no usable shell could be discovered; identical to the C
/// fallback so NSS-provided login shells still work.
const FALLBACK_SHELL_ARGV: [&str; 3] = [
    "sh",
    "-c",
    "if [ -x \"$(getent passwd $(whoami) | cut -d : -f 7)\" ]; then exec $(getent passwd $(whoami) | cut -d : -f 7); else exec sh; fi",
];

/// Shells recognized without consulting `/etc/shells` (`ptyxis_is_shell()`).
const BUILTIN_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "fish", "tcsh", "csh", "tmux"];

/// Shells known to accept `-l` (`ptyxis_shell_supports_dash_l()`).
const DASH_L_SHELLS: &[&str] = &["bash", "fish", "zsh", "dash", "tcsh", "sh"];

const ETC_SHELLS: &str = "/etc/shells";
/// Looked for in a container when the user's shell is unknown.
const DEFAULT_SHELL_NAME: &str = "bash";
const LOGIN_FLAG: &str = "-l";

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("Failed to parse custom command “{command}”: {source}")]
    InvalidCustomCommand {
        command: String,
        source: glib::Error,
    },
    #[error("Failed to prepare container “{name}”: {message}")]
    Container { name: String, message: String },
    #[error("{0}")]
    Spawn(#[from] glib::Error),
}

/// What a tab wants to run. Backends decide how to turn it into a process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnRequest {
    /// Explicit argv (e.g. from `ptyxis -x`); overrides the profile.
    pub command: Option<Vec<String>>,
    /// The profile's custom command, if enabled.
    pub custom_command: Option<String>,
    pub login_shell: bool,
    pub preserve_directory: PreserveDirectory,
    /// Working directory as a URI (initial or inherited from another tab).
    pub cwd_uri: Option<String>,
    pub profile_uuid: String,
    /// Whether to pass the proxy settings on (the profile's `use-proxy`).
    pub use_proxy: bool,
    /// Where to run; the host session by default.
    pub container: Container,
    /// `KEY=VALUE` pairs set last, over tty-rex's own variables
    /// (`ptyxis_tab_set_environment()`).
    pub environment: Vec<String>,
}

/// A fully resolved process to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

/// How a child process terminated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExit {
    Exited(i32),
    Signaled(i32),
}

impl ChildExit {
    /// Decodes a `waitpid()` status as delivered by VTE's `child-exited`.
    pub fn from_wait_status(status: i32) -> Self {
        let status = std::process::ExitStatus::from_raw(status);
        match (status.code(), status.signal()) {
            (Some(code), _) => Self::Exited(code),
            (None, Some(signal)) => Self::Signaled(signal),
            // Stopped/continued statuses are never reported for exits.
            (None, None) => Self::Exited(-1),
        }
    }

    pub fn is_success(self) -> bool {
        self == Self::Exited(0)
    }
}

fn basename(arg0: &str) -> &str {
    arg0.rsplit('/').next().unwrap_or(arg0)
}

/// Whether `arg0` names a shell, by builtin name or `/etc/shells` entry.
pub fn is_shell_in(arg0: &str, configured_shells: &[String]) -> bool {
    BUILTIN_SHELLS.contains(&basename(arg0)) || configured_shells.iter().any(|s| s == arg0)
}

/// Whether `arg0` names a shell on the host (`ptyxis_is_shell()`), reading
/// `/etc/shells` once like the C function.
pub fn is_shell(arg0: &str) -> bool {
    is_shell_in(arg0, configured_shells())
}

/// The shells `/etc/shells` lists, read once and shared by [`is_shell`]
/// and the shell discovery.
fn configured_shells() -> &'static [String] {
    static CONFIGURED_SHELLS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    CONFIGURED_SHELLS.get_or_init(|| {
        std::fs::read_to_string(ETC_SHELLS)
            .map(|contents| parse_etc_shells(&contents))
            .unwrap_or_default()
    })
}

/// Parses `/etc/shells`, skipping blank lines and comments.
pub fn parse_etc_shells(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

pub fn shell_supports_dash_l(shell: &str) -> bool {
    DASH_L_SHELLS.contains(&basename(shell))
}

/// Extracts the local path from a working directory URI (or plain path).
pub fn path_from_uri(uri: &str) -> Option<String> {
    if uri.starts_with('/') {
        return Some(uri.to_owned());
    }
    glib::filename_from_uri(uri)
        .ok()
        .map(|(path, _host)| path.to_string_lossy().into_owned())
}

/// Builds the argv and working directory for `request`.
///
/// `default_shell` is the discovered login shell, if any; `is_shell`
/// decides whether a program is a shell (and thus safe to start in an
/// inherited directory).
pub fn build_plan(
    request: SpawnRequest,
    default_shell: Option<&str>,
    is_shell: impl Fn(&str) -> bool,
) -> Result<SpawnPlan, SpawnError> {
    let mut preserve_directory_safe = false;
    let explicit_command = request.command.filter(|argv| !argv.is_empty());
    let has_explicit_command = explicit_command.is_some();

    // Whether the first argument is a program to test for being a shell.
    let (mut argv, has_arg0) = if let Some(command) = explicit_command {
        (command, false)
    } else if let Some(custom) = request.custom_command {
        let argv: Vec<String> = glib::shell_parse_argv(custom.as_str())
            .map_err(|source| SpawnError::InvalidCustomCommand {
                command: custom,
                source,
            })?
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let has_arg0 = !argv.is_empty();
        (argv, has_arg0)
    } else if let Some(shell) = default_shell.filter(|s| !s.is_empty()) {
        (vec![shell.to_owned()], true)
    } else {
        preserve_directory_safe = true;
        (Vec::from(FALLBACK_SHELL_ARGV.map(str::to_owned)), false)
    };

    if has_arg0 {
        preserve_directory_safe |= is_shell(&argv[0]);
        if request.login_shell && shell_supports_dash_l(&argv[0]) {
            argv.push(LOGIN_FLAG.to_owned());
        }
    }

    let requested_path = request.cwd_uri.as_deref().and_then(path_from_uri);
    let cwd = if has_explicit_command && requested_path.is_some() {
        requested_path
    } else {
        match request.preserve_directory {
            PreserveDirectory::Never => None,
            PreserveDirectory::Safe if !preserve_directory_safe => None,
            PreserveDirectory::Safe | PreserveDirectory::Always => requested_path,
        }
    };

    Ok(SpawnPlan { argv, cwd })
}

/// Called once with the spawn result.
pub type SpawnCallback = Box<dyn FnOnce(Result<glib::Pid, SpawnError>)>;
/// Called once when a successfully spawned process terminates.
pub type ExitCallback = Box<dyn FnOnce(ChildExit)>;

/// A backend able to run a [`SpawnRequest`] inside a terminal's PTY.
pub trait Spawner {
    fn spawn(
        &self,
        terminal: &vte::Terminal,
        request: SpawnRequest,
        on_spawned: SpawnCallback,
        on_exit: ExitCallback,
    );
}

/// Extracts the login shell field from a `passwd` entry.
pub fn parse_passwd_shell(entry: &str) -> Option<&str> {
    entry
        .lines()
        .next()?
        .split(':')
        .nth(6)
        .map(str::trim)
        .filter(|shell| !shell.is_empty())
}

fn is_executable(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Host facts needed to build a plan; gathering them touches the
/// filesystem and runs `getent`, so it happens off the main thread.
#[derive(Debug, Clone, Default)]
struct HostShellInfo {
    user_shell: Option<String>,
    configured_shells: &'static [String],
}

/// Discovers the user's login shell like the agent does: the passwd
/// entry first (via `getent`, so NSS sources work), then `$SHELL`.
fn discover_user_shell() -> Option<String> {
    let user = glib::user_name();
    let from_passwd = Command::new("getent")
        .arg("passwd")
        .arg(&user)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            let stdout = String::from_utf8_lossy(&out.stdout);
            parse_passwd_shell(&stdout).map(str::to_owned)
        });

    from_passwd
        .into_iter()
        .chain(std::env::var("SHELL").ok())
        .find(|shell| is_executable(shell))
}

fn discover_host_shell_info() -> HostShellInfo {
    HostShellInfo {
        user_shell: discover_user_shell(),
        configured_shells: configured_shells(),
    }
}

/// Returns the cached host shell info, discovering it on a worker thread
/// the first time. Concurrent first calls may each discover; that is
/// harmless and rare.
async fn host_shell_info() -> Rc<HostShellInfo> {
    thread_local! {
        static INFO: OnceCell<Rc<HostShellInfo>> = const { OnceCell::new() };
    }
    if let Some(info) = INFO.with(|cell| cell.get().map(Rc::clone)) {
        return info;
    }
    let info = gio::spawn_blocking(discover_host_shell_info)
        .await
        .unwrap_or_else(|_| {
            glib::g_warning!("tty-rex", "Shell discovery failed; using fallback shell");
            HostShellInfo::default()
        });
    INFO.with(|cell| Rc::clone(cell.get_or_init(|| Rc::new(info))))
}

fn vte_version_number() -> u32 {
    // SAFETY: these functions only return compile-time constants of the
    // loaded libvte and have no preconditions.
    let (major, minor, micro) = unsafe {
        (
            vte::ffi::vte_get_major_version(),
            vte::ffi::vte_get_minor_version(),
            vte::ffi::vte_get_micro_version(),
        )
    };
    major * 10_000 + minor * 100 + micro
}

/// Environment additions for child processes, as set by the C client.
pub fn child_environment(profile_uuid: &str, vte_version: u32) -> Vec<String> {
    vec![
        format!("PTYXIS_PROFILE={profile_uuid}"),
        format!("PTYXIS_VERSION={}", config::PACKAGE_VERSION),
        "COLORTERM=truecolor".to_owned(),
        "TERM=xterm-256color".to_owned(),
        format!("VTE_VERSION={vte_version}"),
    ]
}

/// Sets each `KEY=VALUE` of `pairs` in `env`, replacing a variable of the
/// same name, like the `g_environ_setenv()` loops of
/// `ptyxis_client_spawn_async()`. A pair without `=` sets an empty value.
///
/// Pairs without a name (`=VALUE`) are dropped: Ptyxis's agent passes them
/// on, but VTE refuses the whole environment over one and then never
/// finishes the spawn, which would leave the tab stuck. They can come from
/// any `org.freedesktop.Terminal1` caller.
pub fn set_environment<'a>(env: &mut Vec<String>, pairs: impl IntoIterator<Item = &'a str>) {
    fn key(pair: &str) -> &str {
        pair.split_once('=').map_or(pair, |(key, _)| key)
    }
    for pair in pairs.into_iter().filter(|pair| !key(pair).is_empty()) {
        let pair = if pair.contains('=') {
            pair.to_owned()
        } else {
            format!("{pair}=")
        };
        match env.iter_mut().find(|var| key(var) == key(&pair)) {
            Some(var) => *var = pair,
            None => env.push(pair),
        }
    }
}

/// Everything added to a child's environment: the proxy variables when
/// the profile asks for them, then [`child_environment`], then the tab's
/// own variables.
fn spawn_environment(request: &SpawnRequest) -> Vec<String> {
    let mut env = if request.use_proxy {
        proxy::environment()
    } else {
        Vec::new()
    };
    let own = child_environment(&request.profile_uuid, vte_version_number());
    set_environment(&mut env, own.iter().map(String::as_str));
    set_environment(&mut env, request.environment.iter().map(String::as_str));
    env
}

/// Spawns processes directly on the host using `vte_terminal_spawn_async()`.
#[derive(Debug, Default)]
pub struct HostSpawner;

impl Spawner for HostSpawner {
    fn spawn(
        &self,
        terminal: &vte::Terminal,
        request: SpawnRequest,
        on_spawned: SpawnCallback,
        on_exit: ExitCallback,
    ) {
        let weak_terminal = terminal.downgrade();
        glib::spawn_future_local(async move {
            let info = host_shell_info().await;
            let prepared = if request.container.is_session() {
                Ok(None)
            } else {
                prepare_container(&request.container, &info).await.map(Some)
            };
            // The tab may have closed while the shell was being discovered.
            let Some(terminal) = weak_terminal.upgrade() else {
                return;
            };
            match prepared {
                Ok(prepared) => spawn_on_host(
                    &terminal,
                    request,
                    &info,
                    prepared.as_ref(),
                    on_spawned,
                    on_exit,
                ),
                Err(e) => on_spawned(Err(e)),
            }
        });
    }
}

/// Starts the container and finds the user's shell in it, on a worker
/// thread since that runs podman.
async fn prepare_container(
    container: &Container,
    info: &HostShellInfo,
) -> Result<container::Prepared, SpawnError> {
    let shell_name = basename(info.user_shell.as_deref().unwrap_or(DEFAULT_SHELL_NAME)).to_owned();
    let target = container.clone();
    gio::spawn_blocking(move || container::prepare(&target, &shell_name))
        .await
        .unwrap_or_else(|_| Err("the setup thread panicked".to_owned()))
        .map_err(|message| SpawnError::Container {
            name: container.display_name().to_owned(),
            message,
        })
}

fn home_dir() -> Option<String> {
    glib::home_dir().to_str().map(str::to_owned)
}

/// `prepared` is set when spawning into a container other than the
/// session.
fn spawn_on_host(
    terminal: &vte::Terminal,
    mut request: SpawnRequest,
    info: &HostShellInfo,
    prepared: Option<&container::Prepared>,
    on_spawned: SpawnCallback,
    on_exit: ExitCallback,
) {
    // Like the C client, the shell is the user's one as found in the
    // container, but whether a program is a shell is decided on the host.
    let default_shell = match prepared {
        Some(prepared) => prepared.shell.as_deref(),
        None => info.user_shell.as_deref(),
    };
    let mut env = spawn_environment(&request);
    let container = std::mem::take(&mut request.container);
    let plan = match build_plan(request, default_shell, |arg0| {
        is_shell_in(arg0, info.configured_shells)
    }) {
        Ok(plan) => plan,
        Err(e) => {
            on_spawned(Err(e));
            return;
        }
    };

    let (argv, cwd) = match prepared {
        None => (plan.argv, plan.cwd),
        Some(prepared) => {
            // Variables go to the container through the wrapper, so the
            // host command needs none of its own.
            let cwd = plan.cwd.or_else(home_dir).unwrap_or_default();
            // A caller-supplied PWD wins, as in the Ptyxis agent.
            if !env.iter().any(|var| var.starts_with("PWD=")) {
                env.push(format!("PWD={cwd}"));
            }
            let env_in_container =
                container::with_minimal_environment(&env, |key| std::env::var(key).ok());
            let user = glib::user_name();
            let exec = container::exec_plan(
                &container,
                &plan.argv,
                &env_in_container,
                &ExecContext {
                    user: &user.to_string_lossy(),
                    cwd: &cwd,
                    cwd_on_host: Path::new(&cwd).is_dir(),
                    detach_keys: prepared.detach_keys,
                },
            );
            env.clear();
            (exec.argv, exec.host_cwd)
        }
    };
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let cwd = cwd.filter(|dir| Path::new(dir).is_dir()).or_else(home_dir);

    let weak_terminal = terminal.downgrade();
    terminal.spawn_async(
        vte::PtyFlags::DEFAULT,
        cwd.as_deref(),
        &argv,
        &env,
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gio::Cancellable>,
        move |result| {
            let result = result.map_err(SpawnError::from);
            if result.is_ok()
                && let Some(terminal) = weak_terminal.upgrade()
            {
                watch_child_exit(&terminal, on_exit);
            }
            on_spawned(result);
        },
    );
}

/// Delivers the next `child-exited` emission to `on_exit` exactly once.
fn watch_child_exit(terminal: &vte::Terminal, on_exit: ExitCallback) {
    let handler: Rc<Cell<Option<glib::SignalHandlerId>>> = Rc::default();
    let on_exit = Cell::new(Some(on_exit));
    let handler_slot = Rc::clone(&handler);
    let id = terminal.connect_child_exited(move |terminal, status| {
        if let Some(id) = handler_slot.take() {
            terminal.disconnect(id);
        }
        if let Some(on_exit) = on_exit.take() {
            on_exit(ChildExit::from_wait_status(status));
        }
    });
    handler.set(Some(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn always_shell(_: &str) -> bool {
        true
    }

    fn never_shell(_: &str) -> bool {
        false
    }

    #[test]
    fn argv_selection() {
        struct Case {
            name: &'static str,
            request: SpawnRequest,
            default_shell: Option<&'static str>,
            expected: Vec<String>,
        }
        let cases = [
            Case {
                name: "default shell",
                request: SpawnRequest::default(),
                default_shell: Some("/bin/bash"),
                expected: strings(&["/bin/bash"]),
            },
            Case {
                name: "login shell",
                request: SpawnRequest {
                    login_shell: true,
                    ..Default::default()
                },
                default_shell: Some("/usr/bin/zsh"),
                expected: strings(&["/usr/bin/zsh", "-l"]),
            },
            Case {
                name: "login flag only for known shells",
                request: SpawnRequest {
                    login_shell: true,
                    ..Default::default()
                },
                default_shell: Some("/usr/bin/nu"),
                expected: strings(&["/usr/bin/nu"]),
            },
            Case {
                name: "custom command is shell parsed",
                request: SpawnRequest {
                    custom_command: Some("tmux new -s 'my session'".into()),
                    ..Default::default()
                },
                default_shell: Some("/bin/bash"),
                expected: strings(&["tmux", "new", "-s", "my session"]),
            },
            Case {
                name: "explicit command wins and never gets -l",
                request: SpawnRequest {
                    command: Some(strings(&["bash", "-c", "sleep 3"])),
                    custom_command: Some("fish".into()),
                    login_shell: true,
                    ..Default::default()
                },
                default_shell: Some("/bin/bash"),
                expected: strings(&["bash", "-c", "sleep 3"]),
            },
            Case {
                name: "empty explicit command falls back to shell",
                request: SpawnRequest {
                    command: Some(vec![]),
                    ..Default::default()
                },
                default_shell: Some("/bin/bash"),
                expected: strings(&["/bin/bash"]),
            },
            Case {
                name: "no shell known",
                request: SpawnRequest::default(),
                default_shell: None,
                expected: strings(&FALLBACK_SHELL_ARGV),
            },
        ];
        for case in cases {
            let plan = build_plan(case.request, case.default_shell, always_shell)
                .unwrap_or_else(|e| panic!("{}: {e}", case.name));
            assert_eq!(plan.argv, case.expected, "{}", case.name);
        }
    }

    #[test]
    fn invalid_custom_command_is_reported() {
        let request = SpawnRequest {
            custom_command: Some("echo 'unterminated".into()),
            ..Default::default()
        };
        let err = build_plan(request, None, always_shell).unwrap_err();
        assert!(err.to_string().contains("echo 'unterminated"), "{err}");
    }

    #[test]
    fn working_directory_policy() {
        let uri = Some("file:///home/user/src".to_owned());
        let dir = Some("/home/user/src".to_owned());
        let cmd = Some(strings(&["vim"]));
        type Case = (
            &'static str,
            PreserveDirectory,
            Option<Vec<String>>,
            bool,
            Option<String>,
        );
        let cases: Vec<Case> = vec![
            ("never", PreserveDirectory::Never, None, true, None),
            (
                "always",
                PreserveDirectory::Always,
                None,
                false,
                dir.clone(),
            ),
            (
                "safe with shell",
                PreserveDirectory::Safe,
                None,
                true,
                dir.clone(),
            ),
            (
                "safe without shell",
                PreserveDirectory::Safe,
                None,
                false,
                None,
            ),
            (
                "explicit command",
                PreserveDirectory::Never,
                cmd,
                false,
                dir.clone(),
            ),
        ];
        for (name, preserve, command, shell, expected) in cases {
            let request = SpawnRequest {
                command,
                preserve_directory: preserve,
                cwd_uri: uri.clone(),
                ..Default::default()
            };
            let is_shell = if shell { always_shell } else { never_shell };
            let plan = build_plan(request, Some("/bin/bash"), is_shell).unwrap();
            assert_eq!(plan.cwd, expected, "{name}");
        }
    }

    #[test]
    fn fallback_shell_counts_as_safe() {
        let request = SpawnRequest {
            cwd_uri: Some("file:///tmp".into()),
            ..Default::default()
        };
        let plan = build_plan(request, None, never_shell).unwrap();
        assert_eq!(plan.cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn uri_paths() {
        let cases = [
            ("file:///tmp/a%20b", Some("/tmp/a b")),
            ("/already/a/path", Some("/already/a/path")),
            ("sftp://host/tmp", None),
            ("not a uri", None),
        ];
        for (uri, expected) in cases {
            assert_eq!(path_from_uri(uri).as_deref(), expected, "{uri}");
        }
    }

    #[test]
    fn shell_detection() {
        let configured = parse_etc_shells("# comment\n/bin/bash\n\n  /usr/bin/nu  \n");
        assert_eq!(configured, strings(&["/bin/bash", "/usr/bin/nu"]));
        let cases = [
            ("bash", true),
            ("/usr/local/bin/fish", true),
            ("tmux", true),
            ("/usr/bin/nu", true),
            ("nu", false),
            ("vim", false),
        ];
        for (arg0, expected) in cases {
            assert_eq!(is_shell_in(arg0, &configured), expected, "{arg0}");
        }
    }

    #[test]
    fn passwd_parsing() {
        let cases = [
            (
                "tony:x:1000:1000:Tony:/home/tony:/bin/zsh\n",
                Some("/bin/zsh"),
            ),
            ("root:x:0:0:root:/root:\n", None),
            ("short:x:1", None),
            ("", None),
        ];
        for (entry, expected) in cases {
            assert_eq!(parse_passwd_shell(entry), expected, "{entry:?}");
        }
    }

    #[test]
    fn wait_status_decoding() {
        let cases = [
            (0, ChildExit::Exited(0)),
            (1 << 8, ChildExit::Exited(1)),
            (9, ChildExit::Signaled(9)),
        ];
        for (status, expected) in cases {
            assert_eq!(ChildExit::from_wait_status(status), expected);
        }
        assert!(ChildExit::Exited(0).is_success());
        assert!(!ChildExit::Signaled(15).is_success());
    }

    #[test]
    fn environment_matches_c_client() {
        let env = child_environment("abc", 8401);
        for expected in [
            "PTYXIS_PROFILE=abc",
            "COLORTERM=truecolor",
            "TERM=xterm-256color",
            "VTE_VERSION=8401",
            &format!("PTYXIS_VERSION={}", config::PACKAGE_VERSION),
        ] {
            assert!(env.iter().any(|e| e == expected), "{expected}");
        }
    }

    #[test]
    fn set_environment_replaces_by_name() {
        let cases: [(&[&str], &[&str], &[&str]); 6] = [
            (&[], &["A=1"], &["A=1"]),
            (&["A=1", "B=2"], &["A=3"], &["A=3", "B=2"]),
            (&["A=1"], &["B=2", "B=3"], &["A=1", "B=3"]),
            // Only the first `=` separates the name.
            (&["A=1"], &["A=x=y"], &["A=x=y"]),
            (&["A=1"], &["A", "C"], &["A=", "C="]),
            // VTE rejects variables without a name.
            (&["A=1"], &["=x", "", "="], &["A=1"]),
        ];
        for (env, pairs, expected) in cases {
            let mut env = strings(env);
            set_environment(&mut env, pairs.iter().copied());
            assert_eq!(env, strings(expected), "{pairs:?}");
        }
    }
}
