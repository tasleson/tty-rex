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
    }
}
