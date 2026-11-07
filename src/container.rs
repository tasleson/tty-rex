// container.rs
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

//! Containers shells can run in, ported from `ptyxis-agent`'s providers
//! (`agent/ptyxis-podman-provider.c` and the container classes next to
//! it).
//!
//! Besides the host session, containers are discovered with
//! `podman ps --all --format=json` and classified by their labels as
//! distrobox (also apx), toolbox or plain podman containers. The list is
//! refreshed when podman's storage changes. Spawning into a container
//! wraps the shell's argv in `podman exec` or `distrobox enter`
//! ([`exec_plan`]), run on the host like any other command.
//!
//! The agent's JHBuild pseudo container is not ported.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};
use serde::Deserialize;

/// Id of the host session, the `default-container` schema default.
pub const SESSION_ID: &str = "session";
/// How the host session is titled in menus (`get_container_title()`).
pub const SESSION_TITLE: &str = "My Computer";

const PODMAN: &str = "podman";
const DISTROBOX: &str = "distrobox";
/// Delay coalescing storage changes into one `podman ps`
/// (`PODMAN_RELOAD_DELAY_SECONDS`).
const RELOAD_DELAY: Duration = Duration::from_secs(3);
/// Files podman rewrites when containers come and go, below
/// `$XDG_DATA_HOME/containers/storage`: `containers.json` in
/// `overlay-containers`, and the `db.sql` database quadlets update.
const STORAGE_DIR: [&str; 2] = ["containers", "storage"];
const CONTAINERS_DIR: &str = "overlay-containers";
const CONTAINERS_FILE: &str = "containers.json";
const DATABASE_FILE: &str = "db.sql";
/// podman creates its storage directories private to the user.
const STORAGE_DIR_MODE: u32 = 0o700;

/// Values of the `manager` label marking containers managed by distrobox
/// or apx.
const DISTROBOX_MANAGERS: &[&str] = &["distrobox", "apx"];

/// The first podman whose `exec` takes `--detach-keys=`, which keeps it
/// from stealing Ctrl+P (containers/toolbox#394).
const DETACH_KEYS_MIN_VERSION: (u32, u32, u32) = (1, 8, 1);

/// How a container runs commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerKind {
    /// The host itself.
    Session,
    Podman,
    Toolbox,
    /// A distrobox or apx container, entered through `distrobox enter`.
    Distrobox {
        unshare_groups: bool,
    },
}

/// A place a shell can run, like the agent's `PtyxisIpcContainer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    id: String,
    display_name: String,
    kind: ContainerKind,
}

impl Default for Container {
    fn default() -> Self {
        Self::session()
    }
}

impl Container {
    pub fn session() -> Self {
        Self {
            id: SESSION_ID.to_owned(),
            display_name: String::new(),
            kind: ContainerKind::Session,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The container's name; empty for the session.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn kind(&self) -> &ContainerKind {
        &self.kind
    }

    pub fn is_session(&self) -> bool {
        self.kind == ContainerKind::Session
    }

    /// The runtime name shells report in the `vte.container.runtime`
    /// termprop, which the agent calls the provider.
    pub fn provider(&self) -> &'static str {
        match self.kind {
            ContainerKind::Session => SESSION_ID,
            ContainerKind::Podman => PODMAN,
            ContainerKind::Toolbox => "toolbox",
            ContainerKind::Distrobox { .. } => DISTROBOX,
        }
    }

    /// The name shown in menus.
    pub fn title(&self) -> &str {
        if self.is_session() {
            SESSION_TITLE
        } else {
            &self.display_name
        }
    }
}

/// One element of `podman ps --format=json`; other fields are ignored.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PodmanEntry {
    id: String,
    #[serde(default)]
    names: Vec<String>,
    /// podman prints `null` for a container without labels.
    #[serde(default)]
    labels: Option<Labels>,
    #[serde(default)]
    is_infra: bool,
}

/// A label value: only strings are compared, but a label of any other
/// type still counts as present.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LabelValue {
    Text(String),
    Other(serde::de::IgnoredAny),
}

impl LabelValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Other(_) => None,
        }
    }
}

/// Keeps a `null` label as present; `Option` alone would read it as absent.
fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<LabelValue>, D::Error> {
    LabelValue::deserialize(deserializer).map(Some)
}

/// The labels [`classify`] reads, out of all a container carries. The
/// serde renames are the real label names; `podman_ps_parsing` feeds them
/// in as JSON.
#[derive(Debug, Default, Deserialize)]
struct Labels {
    #[serde(default, rename = "manager", deserialize_with = "present")]
    manager: Option<LabelValue>,
    #[serde(
        default,
        rename = "distrobox.unshare_groups",
        deserialize_with = "present"
    )]
    unshare_groups: Option<LabelValue>,
    #[serde(
        default,
        rename = "com.github.containers.toolbox",
        deserialize_with = "present"
    )]
    toolbox: Option<LabelValue>,
}

fn classify(labels: &Labels) -> ContainerKind {
    fn text(label: &Option<LabelValue>) -> Option<&str> {
        label.as_ref().and_then(LabelValue::as_str)
    }
    if text(&labels.manager).is_some_and(|manager| DISTROBOX_MANAGERS.contains(&manager)) {
        ContainerKind::Distrobox {
            unshare_groups: text(&labels.unshare_groups) == Some("1"),
        }
    } else if labels.toolbox.is_some() {
        ContainerKind::Toolbox
    } else {
        ContainerKind::Podman
    }
}

/// Parses `podman ps --all --format=json`, skipping pod infra containers
/// (`_ptyxis_podman_provider_parse_json()`).
pub fn parse_podman_ps(json: &str) -> Result<Vec<Container>, serde_json::Error> {
    let entries: Vec<PodmanEntry> = serde_json::from_str(json)?;
    Ok(entries
        .into_iter()
        .filter(|entry| !entry.is_infra)
        .map(|entry| Container {
            kind: classify(&entry.labels.unwrap_or_default()),
            display_name: entry.names.into_iter().next().unwrap_or_default(),
            id: entry.id,
        })
        .collect())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PodmanVersion {
    client: PodmanClient,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PodmanClient {
    version: String,
}

/// Extracts `major.minor.micro` from `podman version --format=json`.
pub fn parse_podman_version(json: &str) -> Option<(u32, u32, u32)> {
    let version: PodmanVersion = serde_json::from_str(json).ok()?;
    let mut parts = version.client.version.splitn(3, '.');
    let mut next = || -> Option<u32> {
        let part = parts.next()?;
        let digits = part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len());
        part[..digits].parse().ok()
    };
    Some((next()?, next()?, next()?))
}

/// What a spawn into a container needs besides the command itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecContext<'a> {
    /// The user to run as in toolboxes.
    pub user: &'a str,
    /// The working directory inside the container.
    pub cwd: &'a str,
    /// Whether `cwd` also exists on the host.
    pub cwd_on_host: bool,
    /// Whether podman understands `--detach-keys=`.
    pub detach_keys: bool,
}

/// A command to run on the host that runs a program in a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPlan {
    pub argv: Vec<String>,
    /// Directory to start the host command in, if it matters.
    pub host_cwd: Option<String>,
}

/// Wraps `argv`, run with the extra `env` (`KEY=VALUE`), so it runs in
/// `container`, following the agent's `podman exec` and `distrobox enter`
/// run-context layers. The session needs no wrapping.
pub fn exec_plan(
    container: &Container,
    argv: &[String],
    env: &[String],
    context: &ExecContext,
) -> ExecPlan {
    let mut wrapped: Vec<String> = Vec::new();
    let mut host_cwd = None;
    match container.kind {
        ContainerKind::Session => {
            return ExecPlan {
                argv: argv.to_vec(),
                host_cwd: Some(context.cwd.to_owned()),
            };
        }
        ContainerKind::Podman | ContainerKind::Toolbox => {
            // The PTY is always a TTY, so --tty is always wanted.
            wrapped.extend(
                [PODMAN, "exec", "--privileged", "--interactive", "--tty"].map(str::to_owned),
            );
            // Only toolboxes are known to have the user and their home.
            if container.kind == ContainerKind::Toolbox {
                wrapped.push(format!("--user={}", context.user));
                wrapped.push(format!("--workdir={}", context.cwd));
            }
            if context.detach_keys {
                wrapped.push("--detach-keys=".to_owned());
            }
            wrapped.extend(env.iter().map(|var| format!("--env={var}")));
            wrapped.push(container.id.clone());
        }
        ContainerKind::Distrobox { unshare_groups } => {
            wrapped.extend([DISTROBOX, "enter"].map(str::to_owned));
            // Without its own TTY distrobox does not mangle the PTY;
            // podman gets one through --additional-flags instead.
            if !unshare_groups {
                wrapped.push("--no-tty".to_owned());
            }
            wrapped.push(container.display_name.clone());
            if !unshare_groups {
                wrapped.extend(["--additional-flags", "--tty"].map(str::to_owned));
            }
            wrapped.extend(["--", "env"].map(str::to_owned));
            // distrobox shares the home directory, so a directory that
            // exists on the host carries over through the host command.
            if context.cwd_on_host {
                host_cwd = Some(context.cwd.to_owned());
            } else {
                wrapped.push(format!("--chdir={}", context.cwd));
            }
            wrapped.extend(env.iter().cloned());
        }
    }
    wrapped.extend(argv.iter().cloned());
    ExecPlan {
        argv: wrapped,
        host_cwd,
    }
}

/// Host variables a shell in a container still needs, such as the display
/// and session bus (`ptyxis_run_context_add_minimal_environment()`).
/// `podman exec` passes none of the host's environment, so without these
/// graphical programs and D-Bus clients in a toolbox cannot reach the
/// session. `HOME` is left out, as the agent does, because the container
/// sets its own.
const MINIMAL_ENVIRONMENT: &[&str] = &[
    "AT_SPI_BUS_ADDRESS",
    "COLUMNS",
    "DBUS_SESSION_BUS_ADDRESS",
    "DBUS_SYSTEM_BUS_ADDRESS",
    "DESKTOP_SESSION",
    "DISPLAY",
    "LANG",
    "LINES",
    "SHELL",
    "SSH_AUTH_SOCK",
    "USER",
    "VTE_VERSION",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_CURRENT_DESKTOP",
    "XDG_DATA_DIRS",
    "XDG_MENU_PREFIX",
    "XDG_RUNTIME_DIR",
    "XDG_SEAT",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_ID",
    "XDG_SESSION_TYPE",
    "XDG_VTNR",
];

/// The environment (`KEY=VALUE`) for a program in a container: the
/// minimal host variables found through `getenv`, then `env`, whose
/// variables win over host ones of the same name.
pub fn with_minimal_environment(
    env: &[String],
    getenv: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    let overridden = |key: &str| {
        env.iter()
            .any(|var| var.split_once('=').map_or(var.as_str(), |(k, _)| k) == key)
    };
    MINIMAL_ENVIRONMENT
        .iter()
        .filter(|key| !overridden(key))
        .filter_map(|key| getenv(key).map(|value| format!("{key}={value}")))
        .chain(env.iter().cloned())
        .collect()
}

/// Container ids already started by tty-rex (`has_started`), so each is
/// started once.
static STARTED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Mutex::default);

fn command_error(program: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("{program} failed with {}", output.status)
    } else {
        format!("{program} failed: {stderr}")
    }
}

/// Starts the container unless already done, as `maybe_start()` does
/// before every spawn. Distroboxes start themselves on entry, and starting
/// them with podman first races with that (Ptyxis #31).
fn ensure_started(container: &Container) -> Result<(), String> {
    if !matches!(
        container.kind,
        ContainerKind::Podman | ContainerKind::Toolbox
    ) {
        return Ok(());
    }
    let mut started = STARTED.lock().unwrap_or_else(|e| e.into_inner());
    if started.contains(&container.id) {
        return Ok(());
    }
    // Like the C flag, a failed start is not retried on later spawns.
    started.insert(container.id.clone());
    drop(started);
    let output = Command::new(PODMAN)
        .args(["start", &container.id])
        .output()
        .map_err(|e| format!("Failed to run {PODMAN}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(&format!("{PODMAN} start"), &output))
    }
}

/// Finds `program` on the container's `PATH` with `which`.
fn find_program(container: &Container, program: &str) -> Option<String> {
    let output = Command::new(PODMAN)
        .args(["exec", &container.id, "which", program])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !path.is_empty()).then_some(path)
}

/// Whether podman takes `--detach-keys=`, asked once per process.
fn supports_detach_keys() -> bool {
    static SUPPORTED: LazyLock<bool> = LazyLock::new(|| {
        Command::new(PODMAN)
            .args(["version", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| parse_podman_version(&String::from_utf8_lossy(&output.stdout)))
            .is_some_and(|version| version >= DETACH_KEYS_MIN_VERSION)
    });
    *SUPPORTED
}

/// What spawning into a container learned about it.
#[derive(Debug, Clone, Default)]
pub struct Prepared {
    /// The container's path for the user's shell, if it has one.
    pub shell: Option<String>,
    pub detach_keys: bool,
}

/// Starts `container` if needed and looks for the shell named
/// `shell_name` in it, as `ptyxis_application_spawn_async()` does before
/// spawning. Blocks, so it runs on a worker thread.
pub fn prepare(container: &Container, shell_name: &str) -> Result<Prepared, String> {
    ensure_started(container)?;
    Ok(Prepared {
        shell: find_program(container, shell_name),
        detach_keys: supports_detach_keys(),
    })
}

/// Runs `podman ps`, or returns `None` (logged) when that fails.
fn list_podman_containers() -> Option<Vec<Container>> {
    let output = match Command::new(PODMAN)
        .args(["ps", "--all", "--format=json"])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            glib::g_debug!("tty-rex", "Failed to run podman ps: {e}");
            return None;
        }
    };
    if !output.status.success() {
        glib::g_debug!("tty-rex", "{}", command_error("podman ps", &output));
        return None;
    }
    match parse_podman_ps(&String::from_utf8_lossy(&output.stdout)) {
        Ok(containers) => Some(containers),
        Err(e) => {
            glib::g_warning!("tty-rex", "Failed to parse podman containers: {e}");
            None
        }
    }
}

/// Lists podman's containers, trying a second time if podman fails, as
/// the agent does at startup (Ptyxis #62).
fn discover_with_retry() -> Option<Vec<Container>> {
    list_podman_containers().or_else(list_podman_containers)
}

/// Identifies a listener added with [`Containers::connect_changed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerId(u64);

type Listener = Rc<dyn Fn()>;

#[derive(Default)]
struct Inner {
    /// Discovered containers, without the session.
    discovered: RefCell<Vec<Container>>,
    loaded: Cell<bool>,
    waiting: RefCell<Vec<Box<dyn FnOnce()>>>,
    listeners: RefCell<Vec<(ListenerId, Listener)>>,
    next_listener: Cell<u64>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    queued_update: RefCell<Option<glib::SourceId>>,
    updating: Cell<bool>,
}

/// The containers known to the application (the agent's provider list).
#[derive(Clone)]
pub struct Containers {
    inner: Rc<Inner>,
}

impl Default for Containers {
    fn default() -> Self {
        Self::new()
    }
}

impl Containers {
    /// Starts discovering containers. Without podman only the session is
    /// listed, right away.
    pub fn new() -> Self {
        let containers = Self {
            inner: Rc::default(),
        };
        if glib::find_program_in_path(PODMAN).is_none() {
            containers.inner.loaded.set(true);
            return containers;
        }
        containers.monitor_storage();
        containers.update(true);
        containers
    }

    /// The session followed by the discovered containers.
    pub fn list(&self) -> Vec<Container> {
        std::iter::once(Container::session())
            .chain(self.inner.discovered.borrow().iter().cloned())
            .collect()
    }

    /// The container with `id` (`ptyxis_application_lookup_container()`).
    pub fn lookup(&self, id: &str) -> Option<Container> {
        if id == SESSION_ID {
            return Some(Container::session());
        }
        self.inner
            .discovered
            .borrow()
            .iter()
            .find(|c| c.id == id)
            .cloned()
    }

    /// The container a shell reported through the `vte.container.*`
    /// termprops (`ptyxis_application_find_container_by_name()`).
    pub fn find_by_name(&self, runtime: &str, name: &str) -> Option<Container> {
        self.list()
            .into_iter()
            .find(|c| c.provider() == runtime && c.display_name == name)
    }

    /// Calls `f` once the first discovery finished, right away if it has.
    pub fn when_loaded(&self, f: impl FnOnce() + 'static) {
        if self.inner.loaded.get() {
            f();
        } else {
            self.inner.waiting.borrow_mut().push(Box::new(f));
        }
    }

    /// Calls `f` whenever the list changes.
    pub fn connect_changed(&self, f: impl Fn() + 'static) -> ListenerId {
        let id = ListenerId(self.inner.next_listener.get());
        self.inner.next_listener.set(id.0 + 1);
        self.inner.listeners.borrow_mut().push((id, Rc::new(f)));
        id
    }

    pub fn disconnect(&self, id: ListenerId) {
        self.inner
            .listeners
            .borrow_mut()
            .retain(|(listener, _)| *listener != id);
    }

    fn set_discovered(&self, containers: Vec<Container>) {
        if *self.inner.discovered.borrow() == containers {
            return;
        }
        *self.inner.discovered.borrow_mut() = containers;
        // Listeners may (dis)connect, so call a copy of the list.
        let listeners: Vec<Listener> = self
            .inner
            .listeners
            .borrow()
            .iter()
            .map(|(_, f)| Rc::clone(f))
            .collect();
        for listener in listeners {
            listener();
        }
    }

    fn finish_loading(&self) {
        self.inner.loaded.set(true);
        let waiting = std::mem::take(&mut *self.inner.waiting.borrow_mut());
        for f in waiting {
            f();
        }
    }

    fn update(&self, initial: bool) {
        self.inner.updating.set(true);
        let weak = Rc::downgrade(&self.inner);
        glib::spawn_future_local(async move {
            let discovered = gio::spawn_blocking(move || {
                if initial {
                    discover_with_retry()
                } else {
                    list_podman_containers()
                }
            })
            .await
            .ok()
            .flatten();
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let containers = Containers { inner };
            containers.inner.updating.set(false);
            // A failed refresh keeps what was known.
            if let Some(discovered) = discovered {
                containers.set_discovered(discovered);
            }
            if initial {
                containers.finish_loading();
            }
        });
    }

    /// Schedules a refresh unless one is pending or running
    /// (`ptyxis_podman_provider_queue_update()`).
    fn queue_update(&self) {
        if self.inner.updating.get() || self.inner.queued_update.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(&self.inner);
        let source = glib::timeout_add_local_once(RELOAD_DELAY, move || {
            if let Some(inner) = weak.upgrade() {
                inner.queued_update.borrow_mut().take();
                Containers { inner }.update(false);
            }
        });
        *self.inner.queued_update.borrow_mut() = Some(source);
    }

    /// Watches podman's storage like the agent's provider does.
    fn monitor_storage(&self) {
        let storage_dir: PathBuf = STORAGE_DIR
            .iter()
            .fold(glib::user_data_dir(), |dir, part| dir.join(part));
        let containers_dir = storage_dir.join(CONTAINERS_DIR);
        // A missing directory cannot be monitored, so create it upfront the
        // way podman would.
        if let Err(e) = create_private_dir(&containers_dir) {
            glib::g_debug!("tty-rex", "Cannot create {}: {e}", containers_dir.display());
        }

        let mut monitors = Vec::new();
        let file = gio::File::for_path(containers_dir.join(CONTAINERS_FILE));
        match file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            Ok(monitor) => {
                let weak = Rc::downgrade(&self.inner);
                monitor.connect_changed(move |_, _, _, _| {
                    if let Some(inner) = weak.upgrade() {
                        Containers { inner }.queue_update();
                    }
                });
                monitors.push(monitor);
            }
            Err(e) => glib::g_debug!("tty-rex", "Cannot monitor podman containers: {e}"),
        }
        // db.sql may not exist yet, so watch its directory and filter out
        // unrelated churn there.
        let dir = gio::File::for_path(&storage_dir);
        match dir.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            Ok(monitor) => {
                let weak = Rc::downgrade(&self.inner);
                monitor.connect_changed(move |_, file, _, _| {
                    if file.basename().as_deref() == Some(Path::new(DATABASE_FILE))
                        && let Some(inner) = weak.upgrade()
                    {
                        Containers { inner }.queue_update();
                    }
                });
                monitors.push(monitor);
            }
            Err(e) => glib::g_debug!("tty-rex", "Cannot monitor podman storage: {e}"),
        }
        *self.inner.monitors.borrow_mut() = monitors;
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(STORAGE_DIR_MODE)
        .create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(id: &str, name: &str, kind: ContainerKind) -> Container {
        Container {
            id: id.into(),
            display_name: name.into(),
            kind,
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn podman_ps_parsing() {
        let json = r#"[
          {"Id": "aaa", "Names": ["fedora-toolbox-41"], "IsInfra": false,
           "Labels": {"com.github.containers.toolbox": "true"}},
          {"Id": "bbb", "Names": ["db"], "Labels": null},
          {"Id": "ccc", "Names": ["box"],
           "Labels": {"manager": "distrobox", "com.github.containers.toolbox": "true"}},
          {"Id": "ddd", "Names": ["vanilla"],
           "Labels": {"manager": "apx", "distrobox.unshare_groups": "1"}},
          {"Id": "eee", "Names": ["infra"], "IsInfra": true},
          {"Id": "fff", "Labels": {"manager": 3}}
        ]"#;
        let expected = [
            container("aaa", "fedora-toolbox-41", ContainerKind::Toolbox),
            container("bbb", "db", ContainerKind::Podman),
            container(
                "ccc",
                "box",
                ContainerKind::Distrobox {
                    unshare_groups: false,
                },
            ),
            container(
                "ddd",
                "vanilla",
                ContainerKind::Distrobox {
                    unshare_groups: true,
                },
            ),
            container("fff", "", ContainerKind::Podman),
        ];
        assert_eq!(parse_podman_ps(json).unwrap(), expected);
        assert_eq!(parse_podman_ps("[]").unwrap(), []);
        assert!(parse_podman_ps("{").is_err());
    }

    #[test]
    fn podman_versions() {
        let cases = [
            (r#"{"Client":{"Version":"6.1.3"}}"#, Some((6, 1, 3))),
            (r#"{"Client":{"Version":"5.0.0-rc1"}}"#, Some((5, 0, 0))),
            (r#"{"Client":{"Version":"1.8"}}"#, None),
            (r#"{"Server":{}}"#, None),
            ("garbage", None),
        ];
        for (json, expected) in cases {
            assert_eq!(parse_podman_version(json), expected, "{json}");
        }
        assert!((1, 8, 1) >= DETACH_KEYS_MIN_VERSION);
        assert!((1, 8, 0) < DETACH_KEYS_MIN_VERSION);
    }

    #[test]
    fn labels_of_any_type_classify() {
        let json = r#"[
            {"Id": "a", "Labels": {"manager": "distrobox", "distrobox.unshare_groups": "1"}},
            {"Id": "b", "Labels": {"com.github.containers.toolbox": null, "other": [1, {"x": 2}]}}
        ]"#;
        let kinds: Vec<_> = parse_podman_ps(json)
            .unwrap()
            .iter()
            .map(|c| c.kind().clone())
            .collect();
        assert_eq!(
            kinds,
            [
                ContainerKind::Distrobox {
                    unshare_groups: true
                },
                ContainerKind::Toolbox
            ]
        );
    }

    #[test]
    fn providers_and_titles() {
        let cases = [
            (Container::session(), "session", SESSION_TITLE),
            (
                container("a", "pod", ContainerKind::Podman),
                "podman",
                "pod",
            ),
            (
                container("b", "tb", ContainerKind::Toolbox),
                "toolbox",
                "tb",
            ),
            (
                container(
                    "c",
                    "db",
                    ContainerKind::Distrobox {
                        unshare_groups: false,
                    },
                ),
                "distrobox",
                "db",
            ),
        ];
        for (container, provider, title) in cases {
            assert_eq!(container.provider(), provider);
            assert_eq!(container.title(), title);
        }
    }

    #[test]
    fn minimal_environment() {
        let env = strings(&["VTE_VERSION=8200", "TERM=xterm-256color"]);
        let host = |key: &str| match key {
            "DISPLAY" => Some(":0".to_owned()),
            "HOME" => Some("/home/me".to_owned()),
            "VTE_VERSION" => Some("7000".to_owned()),
            "WAYLAND_DISPLAY" => Some("wayland-0".to_owned()),
            _ => None,
        };
        assert_eq!(
            with_minimal_environment(&env, host),
            [
                "DISPLAY=:0",
                "WAYLAND_DISPLAY=wayland-0",
                "VTE_VERSION=8200",
                "TERM=xterm-256color",
            ]
        );
    }

    #[test]
    fn exec_plans() {
        let argv = strings(&["/bin/bash", "-l"]);
        let env = strings(&["TERM=xterm-256color", "PWD=/home/me/src"]);
        let context = ExecContext {
            user: "me",
            cwd: "/home/me/src",
            cwd_on_host: true,
            detach_keys: true,
        };
        let no_tty_hack = ContainerKind::Distrobox {
            unshare_groups: true,
        };
        let cases = [
            (
                "session",
                Container::session(),
                context.clone(),
                ExecPlan {
                    argv: argv.clone(),
                    host_cwd: Some("/home/me/src".into()),
                },
            ),
            (
                "podman",
                container("id1", "pod", ContainerKind::Podman),
                ExecContext {
                    detach_keys: false,
                    ..context.clone()
                },
                ExecPlan {
                    argv: strings(&[
                        "podman",
                        "exec",
                        "--privileged",
                        "--interactive",
                        "--tty",
                        "--env=TERM=xterm-256color",
                        "--env=PWD=/home/me/src",
                        "id1",
                        "/bin/bash",
                        "-l",
                    ]),
                    host_cwd: None,
                },
            ),
            (
                "toolbox",
                container("id2", "tb", ContainerKind::Toolbox),
                context.clone(),
                ExecPlan {
                    argv: strings(&[
                        "podman",
                        "exec",
                        "--privileged",
                        "--interactive",
                        "--tty",
                        "--user=me",
                        "--workdir=/home/me/src",
                        "--detach-keys=",
                        "--env=TERM=xterm-256color",
                        "--env=PWD=/home/me/src",
                        "id2",
                        "/bin/bash",
                        "-l",
                    ]),
                    host_cwd: None,
                },
            ),
            (
                "distrobox",
                container(
                    "id3",
                    "box",
                    ContainerKind::Distrobox {
                        unshare_groups: false,
                    },
                ),
                context.clone(),
                ExecPlan {
                    argv: strings(&[
                        "distrobox",
                        "enter",
                        "--no-tty",
                        "box",
                        "--additional-flags",
                        "--tty",
                        "--",
                        "env",
                        "TERM=xterm-256color",
                        "PWD=/home/me/src",
                        "/bin/bash",
                        "-l",
                    ]),
                    host_cwd: Some("/home/me/src".into()),
                },
            ),
            (
                "distrobox with unshared groups, directory only inside",
                container("id4", "box", no_tty_hack),
                ExecContext {
                    cwd_on_host: false,
                    ..context.clone()
                },
                ExecPlan {
                    argv: strings(&[
                        "distrobox",
                        "enter",
                        "box",
                        "--",
                        "env",
                        "--chdir=/home/me/src",
                        "TERM=xterm-256color",
                        "PWD=/home/me/src",
                        "/bin/bash",
                        "-l",
                    ]),
                    host_cwd: None,
                },
            ),
        ];
        for (name, container, context, expected) in cases {
            assert_eq!(
                exec_plan(&container, &argv, &env, &context),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn registry_lookups() {
        let containers = Containers {
            inner: Rc::default(),
        };
        containers.inner.discovered.replace(vec![
            container("t1", "fedora", ContainerKind::Toolbox),
            container("p1", "fedora", ContainerKind::Podman),
        ]);
        assert_eq!(containers.list().len(), 3);
        assert!(containers.lookup(SESSION_ID).unwrap().is_session());
        assert_eq!(containers.lookup("p1").unwrap().provider(), "podman");
        assert!(containers.lookup("nope").is_none());
        assert_eq!(
            containers.find_by_name("toolbox", "fedora").unwrap().id(),
            "t1"
        );
        assert!(containers.find_by_name("distrobox", "fedora").is_none());
    }

    #[test]
    fn registry_notifies() {
        let containers = Containers {
            inner: Rc::default(),
        };
        let calls = Rc::new(Cell::new(0));
        let counter = Rc::clone(&calls);
        let id = containers.connect_changed(move || counter.set(counter.get() + 1));
        let list = vec![container("p1", "db", ContainerKind::Podman)];
        containers.set_discovered(list.clone());
        // An unchanged list is no change.
        containers.set_discovered(list);
        assert_eq!(calls.get(), 1);
        containers.disconnect(id);
        containers.set_discovered(Vec::new());
        assert_eq!(calls.get(), 1);

        let loaded = Rc::new(Cell::new(false));
        let flag = Rc::clone(&loaded);
        containers.when_loaded(move || flag.set(true));
        assert!(!loaded.get());
        containers.finish_loading();
        assert!(loaded.get());
        let flag = Rc::clone(&loaded);
        loaded.set(false);
        containers.when_loaded(move || flag.set(true));
        assert!(loaded.get());
    }
}
