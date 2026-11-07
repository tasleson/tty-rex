// leader.rs
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

//! Classifies the process leading a terminal, so the window can be
//! highlighted while e.g. `sudo` or `ssh` runs in the foreground.
//!
//! The C implementation asks `ptyxis-agent` (`get_leader_kind()` in
//! `agent/ptyxis-process-impl.c`); the port spawns on the host, so it
//! inspects `/proc` directly with the same rules. An agent-backed
//! spawner would take this over through its `HasForegroundProcess` call.

use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const ROOT_UID: u32 = 0;
const PROC_DIR: &str = "/proc";

/// What kind of program leads the terminal (`PtyxisProcessLeaderKind`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LeaderKind {
    #[default]
    Unknown,
    Superuser,
    Remote,
    Container,
}

impl LeaderKind {
    /// Classifies a program by its executable's file name.
    pub fn from_exe_name(name: &str) -> Self {
        match name {
            "mosh" | "mosh-client" | "rlogin" | "scp" | "sftp" | "slogin" | "ssh" | "telnet" => {
                Self::Remote
            }
            "docker" | "flatpak" | "podman" | "toolbox" => Self::Container,
            _ => Self::Unknown,
        }
    }
}

/// Icon of the container runtime a shell reported through VTE's
/// `vte.container.runtime` termprop, matching the icons the agent's
/// container providers set (distrobox containers are podman ones).
pub fn container_icon_name(runtime: &str) -> Option<&'static str> {
    match runtime {
        "podman" | "distrobox" => Some("container-podman-symbolic"),
        "toolbox" => Some("container-toolbox-symbolic"),
        "jhbuild" => Some("container-jhbuild-symbolic"),
        _ => None,
    }
}

/// Icon shown on the tab, as in `ptyxis_tab_dup_icon()`: superuser and
/// remote leaders win, otherwise the container the shell runs in, if any.
///
/// `container_runtime` is the runtime the shell reported, or else the
/// provider of the container the tab was started in (see
/// `Tab::update_icon`).
pub fn tab_icon_name(kind: LeaderKind, container_runtime: Option<&str>) -> Option<&'static str> {
    match kind {
        LeaderKind::Superuser => Some("process-superuser-symbolic"),
        LeaderKind::Remote => Some("process-remote-symbolic"),
        LeaderKind::Unknown | LeaderKind::Container => {
            container_runtime.and_then(container_icon_name)
        }
    }
}

/// Classifies process `pid`: anything owned by root is a superuser
/// process (this also covers setuid `sudo` while it asks for a password),
/// otherwise its executable decides.
pub fn classify_pid(proc_dir: &Path, pid: i32) -> LeaderKind {
    if pid <= 0 {
        return LeaderKind::Unknown;
    }
    let dir = proc_dir.join(pid.to_string());
    if std::fs::metadata(&dir).is_ok_and(|m| m.uid() == ROOT_UID) {
        return LeaderKind::Superuser;
    }
    std::fs::read_link(dir.join("exe"))
        .ok()
        .and_then(|exe| exe.file_name()?.to_str().map(LeaderKind::from_exe_name))
        .unwrap_or_default()
}

/// The foreground process group leading the terminal on `pty`.
pub fn foreground_pgrp(pty: BorrowedFd<'_>) -> Option<i32> {
    // SAFETY: tcgetpgrp() only queries the borrowed, open descriptor.
    let pgrp = unsafe { libc::tcgetpgrp(pty.as_raw_fd()) };
    (pgrp > 0).then_some(pgrp)
}

/// The foreground process group of the terminal on `pty` and its kind.
pub fn foreground_leader(pty: BorrowedFd<'_>) -> Option<(i32, LeaderKind)> {
    foreground_pgrp(pty).map(|pgrp| (pgrp, classify_pid(Path::new(PROC_DIR), pgrp)))
}

/// `get_cmdline_for_pid()` reads at most this much of `/proc/PID/cmdline`.
const CMDLINE_MAX: usize = 1024;

/// A program other than the tab's own child in the terminal's foreground,
/// as the agent's `HasForegroundProcess` call reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    /// The foreground process group.
    pub pid: i32,
    /// Its command line with arguments separated by spaces, if readable.
    pub cmdline: Option<String>,
}

impl Foreground {
    /// The program name, up to the first space (`ptyxis_tab_poll_agent_cb()`).
    /// Every argument read from `/proc` ends in one, so only a command line
    /// cut short at [`CMDLINE_MAX`] lacks it, and then there is none.
    pub fn program_name(&self) -> Option<&str> {
        self.cmdline
            .as_deref()?
            .split_once(' ')
            .map(|(name, _)| name)
    }

    /// Whether a program other than a shell runs in the foreground, the
    /// test behind close confirmation (`ptyxis_tab_is_running()`).
    pub fn is_running(&self, is_shell: impl Fn(&str) -> bool) -> bool {
        self.program_name().is_some_and(|name| !is_shell(name))
    }
}

/// Turns the raw contents of `/proc/PID/cmdline` into a printable line
/// like `get_cmdline_for_pid()`: separators and control characters become
/// spaces and invalid UTF-8 is replaced. Empty contents give `None`.
pub fn cmdline_from_bytes(bytes: &[u8]) -> Option<String> {
    let bytes = &bytes[..bytes.len().min(CMDLINE_MAX)];
    if bytes.is_empty() {
        return None;
    }
    let spaced: Vec<u8> = bytes
        .iter()
        .map(|&b| if b.is_ascii_control() { b' ' } else { b })
        .collect();
    Some(String::from_utf8_lossy(&spaced).into_owned())
}

fn read_cmdline(proc_dir: &Path, pid: i32) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(CMDLINE_MAX);
    std::fs::File::open(proc_dir.join(pid.to_string()).join("cmdline"))
        .ok()?
        .take(CMDLINE_MAX as u64)
        .read_to_end(&mut buf)
        .ok()?;
    cmdline_from_bytes(&buf)
}

/// What runs in the foreground of the terminal on `pty`, unless it is
/// `child_pid`, the program the tab started (`has_foreground_process()` in
/// `agent/ptyxis-process-impl.c`). A tab without a child has none.
pub fn foreground_process(pty: BorrowedFd<'_>, child_pid: Option<i32>) -> Option<Foreground> {
    foreground_of(foreground_pgrp(pty)?, child_pid)
}

/// What [`foreground_process`] reports once the foreground process group
/// `pgrp` is known, for callers that already looked it up.
pub fn foreground_of(pgrp: i32, child_pid: Option<i32>) -> Option<Foreground> {
    (pgrp != child_pid?).then(|| Foreground {
        pid: pgrp,
        cmdline: read_cmdline(Path::new(PROC_DIR), pgrp),
    })
}

/// The command line of process `pid` as printed by
/// `get_cmdline_for_pid()`, or `None` if it cannot be read.
pub fn process_cmdline(pid: i32) -> Option<String> {
    read_cmdline(Path::new(PROC_DIR), pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn executable_names() {
        let cases = [
            ("ssh", LeaderKind::Remote),
            ("mosh-client", LeaderKind::Remote),
            ("telnet", LeaderKind::Remote),
            ("podman", LeaderKind::Container),
            ("toolbox", LeaderKind::Container),
            ("bash", LeaderKind::Unknown),
            ("sshd", LeaderKind::Unknown),
            ("", LeaderKind::Unknown),
        ];
        for (name, expected) in cases {
            assert_eq!(LeaderKind::from_exe_name(name), expected, "{name}");
        }
    }

    #[test]
    fn icons() {
        use LeaderKind::*;
        let cases = [
            (
                Superuser,
                Some("toolbox"),
                Some("process-superuser-symbolic"),
            ),
            (Remote, None, Some("process-remote-symbolic")),
            (Unknown, None, None),
            (Unknown, Some("toolbox"), Some("container-toolbox-symbolic")),
            (Container, Some("podman"), Some("container-podman-symbolic")),
            (
                Unknown,
                Some("distrobox"),
                Some("container-podman-symbolic"),
            ),
            (Unknown, Some("jhbuild"), Some("container-jhbuild-symbolic")),
            (Container, Some("lxc"), None),
            (Container, None, None),
        ];
        for (kind, runtime, expected) in cases {
            assert_eq!(
                tab_icon_name(kind, runtime),
                expected,
                "{kind:?} {runtime:?}"
            );
        }
    }

    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ptyxis-leader-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn classification_from_proc() {
        // A fake /proc owned by the test user, so the executable link
        // decides; as root every entry would be a superuser process.
        let proc_dir = scratch_dir("proc");
        if std::fs::metadata(&proc_dir).unwrap().uid() == ROOT_UID {
            std::fs::remove_dir_all(&proc_dir).unwrap();
            return;
        }
        for (pid, exe) in [(10, "/usr/bin/ssh"), (11, "/usr/bin/bash")] {
            let dir = proc_dir.join(pid.to_string());
            std::fs::create_dir(&dir).unwrap();
            std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
        }
        std::fs::create_dir(proc_dir.join("12")).unwrap();

        let cases = [
            (10, LeaderKind::Remote),
            (11, LeaderKind::Unknown),
            // No exe link (e.g. a kernel thread or no permission).
            (12, LeaderKind::Unknown),
            // Gone already.
            (13, LeaderKind::Unknown),
            (0, LeaderKind::Unknown),
            (-1, LeaderKind::Unknown),
        ];
        for (pid, expected) in cases {
            assert_eq!(classify_pid(&proc_dir, pid), expected, "pid {pid}");
        }
        std::fs::remove_dir_all(&proc_dir).unwrap();
    }

    #[test]
    fn own_process_is_classified_by_owner() {
        let proc_dir = Path::new(PROC_DIR);
        let expected = if std::fs::metadata(proc_dir.join("self")).unwrap().uid() == ROOT_UID {
            LeaderKind::Superuser
        } else {
            // The test binary is neither a remote nor a container tool.
            LeaderKind::Unknown
        };
        let pid = i32::try_from(std::process::id()).unwrap();
        assert_eq!(classify_pid(proc_dir, pid), expected);
    }

    #[test]
    fn non_terminal_has_no_foreground_group() {
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(foreground_pgrp(file.as_fd()), None);
        assert_eq!(foreground_leader(file.as_fd()), None);
        assert_eq!(foreground_process(file.as_fd(), Some(1)), None);
    }

    #[test]
    fn foreground_of_child() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let cases = [(None, false), (Some(pid), false), (Some(pid + 1), true)];
        for (child_pid, expected) in cases {
            let foreground = foreground_of(pid, child_pid);
            assert_eq!(foreground.is_some(), expected, "{child_pid:?}");
            if let Some(foreground) = foreground {
                assert_eq!(foreground.pid, pid);
                assert!(foreground.cmdline.is_some());
            }
        }
    }

    #[test]
    fn cmdlines() {
        let long = vec![b'x'; CMDLINE_MAX + 10];
        let cases: [(&[u8], Option<&str>); 6] = [
            (b"", None),
            (b"vim\0", Some("vim ")),
            (b"vim\0file.txt\0", Some("vim file.txt ")),
            (b"a\tb\nc\0", Some("a b c ")),
            (b"caf\xc3\xa9\0", Some("café ")),
            (b"bad\xff\0", Some("bad\u{fffd} ")),
        ];
        for (bytes, expected) in cases {
            assert_eq!(cmdline_from_bytes(bytes).as_deref(), expected, "{bytes:?}");
        }
        assert_eq!(cmdline_from_bytes(&long).unwrap().len(), CMDLINE_MAX);
    }

    #[test]
    fn running_programs() {
        let is_shell = |name: &str| matches!(name, "bash" | "/bin/zsh");
        let cases = [
            (Some("vim file.txt "), Some("vim"), true),
            (Some("bash "), Some("bash"), false),
            (Some("/bin/zsh -l "), Some("/bin/zsh"), false),
            (Some("/usr/bin/top "), Some("/usr/bin/top"), true),
            // Cut short without a separator, or unreadable.
            (Some("truncated"), None, false),
            (None, None, false),
        ];
        for (cmdline, name, running) in cases {
            let foreground = Foreground {
                pid: 42,
                cmdline: cmdline.map(str::to_owned),
            };
            assert_eq!(foreground.program_name(), name, "{cmdline:?}");
            assert_eq!(foreground.is_running(is_shell), running, "{cmdline:?}");
        }
    }

    #[test]
    fn own_cmdline_is_readable() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let cmdline = read_cmdline(Path::new(PROC_DIR), pid).unwrap();
        assert!(cmdline.ends_with(' '), "{cmdline:?}");
    }
}
