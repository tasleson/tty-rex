// telemetry/procfs.rs
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

//! The `/proc` side of telemetry: the process table, a tab's process
//! tree, CPU time, memory and the sockets each process holds.
//!
//! Everything here reads files an unprivileged user may read. Files of
//! processes that belong to someone else (e.g. `sudo`) or that exit while
//! being read are skipped rather than reported, since that is routine.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

/// A process id as the kernel reports it.
pub type Pid = i32;

const PROC_DIR: &str = "/proc";
/// `/proc/<pid>/smaps_rollup` line holding the proportional set size.
const PSS_PREFIX: &str = "Pss:";
const KIB: u64 = 1024;
/// Link target prefix and suffix of a socket in `/proc/<pid>/fd`.
const SOCKET_LINK_PREFIX: &str = "socket:[";
const SOCKET_LINK_SUFFIX: &str = "]";

/// 1-based field numbers of `/proc/<pid>/stat` from proc(5).
mod stat_field {
    pub const PPID: usize = 4;
    pub const UTIME: usize = 14;
    pub const STIME: usize = 15;
    pub const STARTTIME: usize = 22;
    /// The first field after the `(comm)` field.
    pub const FIRST_AFTER_COMM: usize = 3;
}

/// What telemetry needs from `/proc/<pid>/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    pub ppid: Pid,
    /// User plus system time in clock ticks.
    pub cpu_ticks: u64,
    /// Start time in clock ticks after boot; together with the pid it
    /// identifies a process even when the pid gets reused.
    pub start_time: u64,
}

/// Parses `/proc/<pid>/stat`. The command name may contain spaces and
/// parentheses, so the fields are counted from its last `)`.
pub fn parse_stat(contents: &str) -> Option<ProcStat> {
    let rest = &contents[contents.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let field = |number: usize| -> Option<u64> {
        fields
            .get(number.checked_sub(stat_field::FIRST_AFTER_COMM)?)?
            .parse()
            .ok()
    };
    Some(ProcStat {
        ppid: Pid::try_from(field(stat_field::PPID)?).ok()?,
        cpu_ticks: field(stat_field::UTIME)?.checked_add(field(stat_field::STIME)?)?,
        start_time: field(stat_field::STARTTIME)?,
    })
}

/// Reads every process's stat. Only failing to list `/proc` is an error;
/// processes vanishing while it is scanned are left out.
pub fn scan_processes() -> io::Result<HashMap<Pid, ProcStat>> {
    let mut processes = HashMap::new();
    for entry in fs::read_dir(PROC_DIR)? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<Pid>().ok())
        else {
            continue;
        };
        if let Some(stat) = fs::read_to_string(entry.path().join("stat"))
            .ok()
            .as_deref()
            .and_then(parse_stat)
        {
            processes.insert(pid, stat);
        }
    }
    Ok(processes)
}

/// `root` and all its descendants, given each process's parent. Empty
/// when `root` itself is not running. Processes that daemonized (were
/// reparented away from the tree) are not found.
pub fn process_tree(root: Pid, parents: impl IntoIterator<Item = (Pid, Pid)>) -> Vec<Pid> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    let mut known = HashSet::new();
    for (pid, ppid) in parents {
        known.insert(pid);
        children.entry(ppid).or_default().push(pid);
    }
    if !known.contains(&root) {
        return Vec::new();
    }
    let mut tree = vec![root];
    let mut seen = HashSet::from([root]);
    let mut next = 0;
    while let Some(&pid) = tree.get(next) {
        next += 1;
        for &child in children.get(&pid).into_iter().flatten() {
            // Guards against a malformed table looping back on itself.
            if seen.insert(child) {
                tree.push(child);
            }
        }
    }
    tree
}

/// The `Pss:` total of `/proc/<pid>/smaps_rollup`, in bytes.
pub fn parse_pss(smaps_rollup: &str) -> Option<u64> {
    let line = smaps_rollup
        .lines()
        .find_map(|line| line.strip_prefix(PSS_PREFIX))?;
    let mut parts = line.split_whitespace();
    let kib: u64 = parts.next()?.parse().ok()?;
    (parts.next()? == "kB").then(|| kib.saturating_mul(KIB))
}

/// The resident page count of `/proc/<pid>/statm` (its second field).
pub fn parse_statm_resident(statm: &str) -> Option<u64> {
    statm.split_whitespace().nth(1)?.parse().ok()
}

/// A process's memory in bytes: its proportional set size, so pages
/// shared within the tree are not counted repeatedly, or its resident set
/// size where `smaps_rollup` is not readable (other users' processes,
/// kernels before 4.14).
pub fn memory_bytes(pid: Pid, page_size: u64) -> Option<u64> {
    let dir = Path::new(PROC_DIR).join(pid.to_string());
    fs::read_to_string(dir.join("smaps_rollup"))
        .ok()
        .as_deref()
        .and_then(parse_pss)
        .or_else(|| {
            let pages = parse_statm_resident(&fs::read_to_string(dir.join("statm")).ok()?)?;
            Some(pages.saturating_mul(page_size))
        })
}

/// The inode of a `/proc/<pid>/fd` link target naming a socket.
pub fn parse_socket_link(target: &str) -> Option<u64> {
    target
        .strip_prefix(SOCKET_LINK_PREFIX)?
        .strip_suffix(SOCKET_LINK_SUFFIX)?
        .parse()
        .ok()
}

/// Adds the inodes of the sockets `pid` has open to `inodes`. Processes
/// whose descriptors are not readable (other users') are skipped.
pub fn collect_socket_inodes(pid: Pid, inodes: &mut HashSet<u64>) {
    let dir = Path::new(PROC_DIR).join(pid.to_string()).join("fd");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(inode) = fs::read_link(entry.path())
            .ok()
            .and_then(|target| parse_socket_link(target.to_str()?))
        {
            inodes.insert(inode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stat line with `comm`, parent 7, utime 11, stime 22, starttime 99.
    fn stat_line(comm: &str) -> String {
        format!("42 ({comm}) S 7 42 42 0 -1 4194560 100 0 0 0 11 22 0 0 20 0 1 0 99 1000 50")
    }

    #[test]
    fn stat_parsing() {
        let expected = Some(ProcStat {
            ppid: 7,
            cpu_ticks: 33,
            start_time: 99,
        });
        let cases = [
            (stat_line("bash"), expected),
            (stat_line("tmux: server"), expected),
            (stat_line("a) b (c"), expected),
            (stat_line(")"), expected),
            (stat_line(""), expected),
            ("42 (bash) S 7 42".to_owned(), None),
            ("no parens at all".to_owned(), None),
            (
                "42 (x) S -1 0 0 0 0 0 0 0 0 0 1 1 0 0 0 0 0 0 5".to_owned(),
                None,
            ),
            (
                "42 (x) S 1 0 0 0 0 0 0 0 0 0 x 1 0 0 0 0 0 0 5".to_owned(),
                None,
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(parse_stat(&line), expected, "{line}");
        }
    }

    #[test]
    fn trees() {
        let table = [
            (1, 0),
            (10, 1),
            (11, 10),
            (12, 10),
            (13, 11),
            (20, 1),
            (21, 20),
        ];
        let sorted = |root| {
            let mut tree = process_tree(root, table);
            tree.sort_unstable();
            tree
        };
        let cases: [(Pid, &[Pid]); 5] = [
            (10, &[10, 11, 12, 13]),
            (11, &[11, 13]),
            (21, &[21]),
            (1, &[1, 10, 11, 12, 13, 20, 21]),
            (99, &[]),
        ];
        for (root, expected) in cases {
            assert_eq!(sorted(root), expected, "root {root}");
        }
        // Cycles in a malformed table do not loop forever.
        let mut cyclic = process_tree(5, [(5, 6), (6, 5)]);
        cyclic.sort_unstable();
        assert_eq!(cyclic, [5, 6]);
    }

    #[test]
    fn pss_parsing() {
        let rollup = "55d0-7ffc ---p 00000000 00:00 0 [rollup]\n\
                      Rss:                5000 kB\n\
                      Pss:                3000 kB\n\
                      Pss_Dirty:          1000 kB\n";
        let cases = [
            (rollup, Some(3000 * 1024)),
            ("Pss: 1 kB\n", Some(1024)),
            ("Rss: 1 kB\n", None),
            ("Pss: x kB\n", None),
            ("Pss: 1 MB\n", None),
            ("", None),
        ];
        for (contents, expected) in cases {
            assert_eq!(parse_pss(contents), expected, "{contents:?}");
        }
    }

    #[test]
    fn statm_parsing() {
        let cases = [
            ("1000 250 30 10 0 200 0\n", Some(250)),
            ("1000", None),
            ("1000 x", None),
        ];
        for (contents, expected) in cases {
            assert_eq!(parse_statm_resident(contents), expected, "{contents:?}");
        }
    }

    #[test]
    fn socket_links() {
        let cases = [
            ("socket:[12345]", Some(12345)),
            ("socket:[]", None),
            ("socket:[12", None),
            ("pipe:[12345]", None),
            ("/dev/pts/3", None),
            ("anon_inode:[eventpoll]", None),
        ];
        for (target, expected) in cases {
            assert_eq!(parse_socket_link(target), expected, "{target}");
        }
    }
}
