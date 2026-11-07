// telemetry/mod.rs
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

//! Resource readings for the processes started from a tab, shown by the
//! telemetry footer. This is a tty-rex addition with no Ptyxis
//! counterpart.
//!
//! A tab's process tree is its child process plus every descendant found
//! through the parent ids in `/proc`. Processes that daemonize are
//! reparented away and drop out of it. Each [`Sampler::sample`] reads:
//!
//! - network: per-socket TCP byte counters ([`sock_diag`]) for the
//!   sockets the tree holds. UDP (and so QUIC) is not counted, and
//!   whatever a socket moved between its last sample and closing is lost.
//! - CPU: user plus system time of each process, 100% being one core.
//! - memory: the proportional set size of each process, falling back to
//!   its resident size where that is unreadable.
//!
//! Rates come from the counters' growth since the previous sample of the
//! same tree, so the first sample only sets a baseline. A process or
//! socket that appeared since then counts all it accumulated, as it
//! started in between; one that went away drops out without a negative
//! contribution. Sampling does blocking I/O and belongs off the main
//! thread.

pub mod procfs;
pub mod sock_diag;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gtk::glib;

pub use procfs::Pid;

/// Shown for a reading that is not available (yet).
pub const PLACEHOLDER: &str = "—";
/// Separates the up and down rate of the network reading, which reads
/// e.g. "1.2KiB/s-3.4MiB/s"; numbers and units are not spaced either.
const NET_SEPARATOR: &str = "-";
const BINARY_UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
const UNIT_STEP: f64 = 1024.0;
/// A whole number at least this large rounds up to [`UNIT_STEP`], so it
/// moves to the next unit instead of showing as e.g. "1024KiB".
const NEXT_UNIT_FROM: f64 = UNIT_STEP - 0.5;
/// Values below this get one decimal; it is 10 less half the last digit,
/// so a value that rounds up to 10 is shown as "10", not "10.0".
const ONE_DECIMAL_BELOW: f64 = 9.95;
const PERCENT: f64 = 100.0;
/// Shorter intervals give no meaningful rate.
const MIN_RATE_INTERVAL: Duration = Duration::from_millis(10);
/// Used if `sysconf()` fails; the value on every mainstream Linux build.
const FALLBACK_CLOCK_TICKS: u64 = 100;
const FALLBACK_PAGE_SIZE: u64 = 4096;

/// The readings the footer shows, in display order. Adding one means a
/// variant here, its label and tooltip, and a field in [`Readings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingKind {
    Cpu,
    Memory,
    Network,
}

impl ReadingKind {
    pub const ALL: [Self; 3] = [Self::Cpu, Self::Memory, Self::Network];

    pub fn label(self) -> &'static str {
        match self {
            Self::Network => "Net ↑-↓",
            Self::Cpu => "CPU",
            Self::Memory => "Mem",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Network => "Network Up/Down, TCP only",
            Self::Cpu => "Processor use; 100% is one core",
            Self::Memory => "Proportional memory use",
        }
    }

    /// Characters to reserve for the value so the footer does not shift
    /// as numbers change.
    pub fn width_chars(self) -> i32 {
        match self {
            // "1023MiB/s-1023MiB/s"
            Self::Network => 19,
            Self::Cpu => 5,
            // "2048GiB", with room for one more digit.
            Self::Memory => 8,
        }
    }
}

/// Network throughput in bytes per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetRates {
    pub up: f64,
    pub down: f64,
}

/// One sample's results; `None` where a reading is unavailable, e.g. on
/// the baseline sample or after the tab's process exited.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Readings {
    pub net: Option<NetRates>,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
}

impl Readings {
    /// The text shown for `kind`.
    pub fn text(&self, kind: ReadingKind) -> String {
        let text = match kind {
            ReadingKind::Network => self.net.map(|net| {
                format!(
                    "{}{NET_SEPARATOR}{}",
                    format_rate(net.up),
                    format_rate(net.down)
                )
            }),
            ReadingKind::Cpu => self.cpu_percent.map(format_percent),
            ReadingKind::Memory => self.memory_bytes.map(|bytes| format_bytes(bytes as f64)),
        };
        text.unwrap_or_else(|| PLACEHOLDER.to_owned())
    }
}

/// `value` scaled to the largest binary unit keeping it at least 1, with
/// one decimal below 10 (except plain bytes) and none above, written
/// without a space before the unit, e.g. "340MiB".
pub fn format_bytes(value: f64) -> String {
    let mut value = value.max(0.0);
    let mut unit = 0;
    while value >= NEXT_UNIT_FROM && unit + 1 < BINARY_UNITS.len() {
        value /= UNIT_STEP;
        unit += 1;
    }
    let decimals = usize::from(unit > 0 && value < ONE_DECIMAL_BELOW);
    format!("{value:.decimals$}{}", BINARY_UNITS[unit])
}

pub fn format_rate(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_second))
}

pub fn format_percent(percent: f64) -> String {
    format!("{:.0}%", percent.max(0.0))
}

/// How much the counters grew from `previous` to `current`. A key absent
/// from `previous` appeared in between and counts in full; a key absent
/// from `current` is gone and counts nothing; a counter that went
/// backwards (it should not) counts nothing either.
pub fn counter_growth<K: Eq + Hash>(previous: &HashMap<K, u64>, current: &HashMap<K, u64>) -> u64 {
    current
        .iter()
        .map(|(key, &value)| value.saturating_sub(previous.get(key).copied().unwrap_or(0)))
        .fold(0, u64::saturating_add)
}

/// `amount` per second over `elapsed`, or `None` if it is too short.
pub fn per_second(amount: u64, elapsed: Duration) -> Option<f64> {
    (elapsed >= MIN_RATE_INTERVAL).then(|| amount as f64 / elapsed.as_secs_f64())
}

/// CPU use in percent of one core for `ticks` of CPU time over `elapsed`,
/// capped at what `cores` can deliver.
pub fn cpu_percent(ticks: u64, elapsed: Duration, clock_ticks: u64, cores: u64) -> Option<f64> {
    let ticks_per_second = per_second(ticks, elapsed)?;
    let percent = ticks_per_second / clock_ticks.max(1) as f64 * PERCENT;
    Some(percent.min(cores.max(1) as f64 * PERCENT))
}

/// Logs a warning the first time only, so a persistent failure does not
/// flood the journal at every interval.
struct WarnOnce(AtomicBool);

impl WarnOnce {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn warn(&self, message: impl FnOnce() -> String) {
        if !self.0.swap(true, Ordering::Relaxed) {
            glib::g_warning!("tty-rex", "{}", message());
        }
    }
}

static PROC_WARNING: WarnOnce = WarnOnce::new();
static SOCK_DIAG_WARNING: WarnOnce = WarnOnce::new();

/// Page size in bytes, which cannot change while running.
fn page_size() -> u64 {
    static PAGE_SIZE: OnceLock<u64> = OnceLock::new();
    *PAGE_SIZE.get_or_init(|| sysconf(libc::_SC_PAGESIZE, FALLBACK_PAGE_SIZE))
}

/// Clock ticks per second, which cannot change while running.
fn clock_ticks() -> u64 {
    static CLOCK_TICKS: OnceLock<u64> = OnceLock::new();
    *CLOCK_TICKS.get_or_init(|| sysconf(libc::_SC_CLK_TCK, FALLBACK_CLOCK_TICKS))
}

fn sysconf(name: libc::c_int, fallback: u64) -> u64 {
    // SAFETY: sysconf() has no preconditions.
    u64::try_from(unsafe { libc::sysconf(name) })
        .ok()
        .filter(|&value| value > 0)
        .unwrap_or(fallback)
}

/// Process identity robust against pid reuse: pid and start time.
type ProcessKey = (Pid, u64);

/// Cumulative counters of one sample, kept for the next one.
#[derive(Debug)]
struct Snapshot {
    root: Pid,
    taken: Instant,
    cpu_ticks: HashMap<ProcessKey, u64>,
    /// Bytes sent and received per socket inode; `None` if unreadable.
    sockets: Option<(HashMap<u64, u64>, HashMap<u64, u64>)>,
}

/// Samples one process tree repeatedly, remembering the previous sample
/// to turn counters into rates.
#[derive(Debug, Default)]
pub struct Sampler {
    baseline: Option<Snapshot>,
    /// Kept between samples so each one reuses the same allocations.
    diag: sock_diag::DiagClient,
    read_buffer: String,
}

impl Sampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the baseline, so the next sample starts a new one.
    pub fn reset(&mut self) {
        self.baseline = None;
    }

    /// Forgets the baseline and closes the netlink socket, for when
    /// sampling stops for a while.
    pub fn release(&mut self) {
        self.reset();
        self.diag.close();
    }

    /// Samples the tree rooted at `root`. Changing the root starts over
    /// with a baseline. Never fails: what cannot be read is `None`.
    pub fn sample(&mut self, root: Pid) -> Readings {
        let taken = Instant::now();
        let processes = match procfs::scan_processes(&mut self.read_buffer) {
            Ok(processes) => processes,
            Err(e) => {
                PROC_WARNING.warn(|| format!("Telemetry cannot read /proc: {e}"));
                self.baseline = None;
                return Readings::default();
            }
        };
        let tree = procfs::process_tree(root, processes.iter().map(|(&pid, s)| (pid, s.ppid)));
        if tree.is_empty() {
            self.baseline = None;
            return Readings::default();
        }

        let page_size = page_size();
        let mut cpu_ticks = HashMap::with_capacity(tree.len());
        let mut memory = None;
        let mut inodes = HashSet::new();
        for &pid in &tree {
            let Some(stat) = processes.get(&pid) else {
                continue;
            };
            cpu_ticks.insert((pid, stat.start_time), stat.cpu_ticks);
            if let Some(bytes) = procfs::memory_bytes(pid, page_size, &mut self.read_buffer) {
                memory = Some(memory.unwrap_or(0u64).saturating_add(bytes));
            }
            procfs::collect_socket_inodes(pid, &mut inodes);
        }
        let sockets = match self.diag.tcp_counters(&inodes) {
            Ok(counters) => Some(counters.into_iter().fold(
                (HashMap::new(), HashMap::new()),
                |(mut sent, mut received), (inode, c)| {
                    sent.insert(inode, c.bytes_acked);
                    received.insert(inode, c.bytes_received);
                    (sent, received)
                },
            )),
            Err(e) => {
                SOCK_DIAG_WARNING.warn(|| format!("Telemetry cannot read TCP statistics: {e}"));
                None
            }
        };
        let current = Snapshot {
            root,
            taken,
            cpu_ticks,
            sockets,
        };

        let mut readings = Readings {
            memory_bytes: memory,
            ..Readings::default()
        };
        if let Some(previous) = self.baseline.as_ref().filter(|b| b.root == root) {
            let elapsed = current.taken.saturating_duration_since(previous.taken);
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
            readings.cpu_percent = cpu_percent(
                counter_growth(&previous.cpu_ticks, &current.cpu_ticks),
                elapsed,
                clock_ticks(),
                cores,
            );
            if let (Some((sent0, received0)), Some((sent1, received1))) =
                (&previous.sockets, &current.sockets)
            {
                readings.net = per_second(counter_growth(sent0, sent1), elapsed)
                    .zip(per_second(counter_growth(received0, received1), elapsed))
                    .map(|(up, down)| NetRates { up, down });
            }
        }
        self.baseline = Some(current);
        readings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting() {
        const KI: f64 = 1024.0;
        const MI: f64 = KI * KI;
        const GI: f64 = MI * KI;
        let cases = [
            (0.0, "0B"),
            (-5.0, "0B"),
            (1.4, "1B"),
            (512.0, "512B"),
            (1023.0, "1023B"),
            // Would round to "1024B".
            (1023.6, "1.0KiB"),
            (KI, "1.0KiB"),
            (1.25 * KI, "1.2KiB"),
            (9.94 * KI, "9.9KiB"),
            (9.96 * KI, "10KiB"),
            (10.0 * KI, "10KiB"),
            (1023.4 * KI, "1023KiB"),
            (1023.6 * KI, "1.0MiB"),
            (340.0 * MI, "340MiB"),
            (3.4 * MI, "3.4MiB"),
            (1.5 * GI, "1.5GiB"),
            // GiB is the largest unit.
            (2048.0 * GI, "2048GiB"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_bytes(value), expected, "{value}");
        }
        assert_eq!(format_rate(1.25 * KI), "1.2KiB/s");
        assert_eq!(format_percent(12.4), "12%");
        assert_eq!(format_percent(-1.0), "0%");
    }

    #[test]
    fn reading_texts() {
        const MI: f64 = 1024.0 * 1024.0;
        let readings = |up: f64, down: f64, cpu: f64, memory: u64| Readings {
            net: Some(NetRates { up, down }),
            cpu_percent: Some(cpu),
            memory_bytes: Some(memory),
        };
        let typical = readings(1.2 * 1024.0, 3.4 * MI, 12.0, 340 * 1024 * 1024);
        // The widest values each reading can show.
        let widest = readings(1023.0 * MI, 1023.0 * MI, 400.0, 2048 << 30);
        let cases = [
            (typical, ReadingKind::Network, "1.2KiB/s-3.4MiB/s"),
            (typical, ReadingKind::Cpu, "12%"),
            (typical, ReadingKind::Memory, "340MiB"),
            (widest, ReadingKind::Network, "1023MiB/s-1023MiB/s"),
            (widest, ReadingKind::Cpu, "400%"),
            (widest, ReadingKind::Memory, "2048GiB"),
        ];
        for (readings, kind, expected) in cases {
            assert_eq!(readings.text(kind), expected, "{kind:?}");
            assert_eq!(Readings::default().text(kind), PLACEHOLDER, "{kind:?}");
            assert!(
                expected.chars().count() <= kind.width_chars() as usize,
                "{kind:?}"
            );
        }
        assert!(ReadingKind::Network.tooltip().contains("TCP only"));
    }

    #[test]
    fn growth() {
        let map = |entries: &[(u32, u64)]| entries.iter().copied().collect::<HashMap<_, _>>();
        type Counters = &'static [(u32, u64)];
        let cases: [(Counters, Counters, u64); 7] = [
            (&[], &[], 0),
            (&[(1, 100)], &[(1, 150)], 50),
            // New since the last sample: everything counts.
            (&[(1, 100)], &[(1, 100), (2, 30)], 30),
            // Gone since the last sample: no negative contribution.
            (&[(1, 100), (2, 500)], &[(1, 110)], 10),
            // A counter going backwards does not underflow.
            (&[(1, 100)], &[(1, 40)], 0),
            (&[(1, 10), (2, 20)], &[(1, 15), (2, 26), (3, 1)], 12),
            (&[(1, 0)], &[(1, u64::MAX), (2, 5)], u64::MAX),
        ];
        for (previous, current, expected) in cases {
            assert_eq!(
                counter_growth(&map(previous), &map(current)),
                expected,
                "{previous:?} -> {current:?}"
            );
        }
    }

    #[test]
    fn rates() {
        let second = Duration::from_secs(1);
        let cases = [
            (0, second, Some(0.0)),
            (1000, second * 2, Some(500.0)),
            (1, Duration::from_millis(500), Some(2.0)),
            (1000, Duration::ZERO, None),
            (1000, Duration::from_millis(1), None),
        ];
        for (amount, elapsed, expected) in cases {
            assert_eq!(
                per_second(amount, elapsed),
                expected,
                "{amount} {elapsed:?}"
            );
        }

        let cases = [
            // 50 ticks of 100 Hz in one second: half a core.
            (50, second, 100, 4, Some(50.0)),
            (400, second * 2, 100, 4, Some(200.0)),
            (250, second, 250, 1, Some(100.0)),
            // Capped at all cores busy.
            (10_000, second, 100, 4, Some(400.0)),
            (0, second, 0, 0, Some(0.0)),
            (5, Duration::ZERO, 100, 4, None),
        ];
        for (ticks, elapsed, clock, cores, expected) in cases {
            assert_eq!(
                cpu_percent(ticks, elapsed, clock, cores),
                expected,
                "{ticks} {elapsed:?} {clock} {cores}"
            );
        }
    }

    #[test]
    fn missing_process_has_no_readings() {
        let mut sampler = Sampler::new();
        // Pid 0 is never a process in /proc.
        assert_eq!(sampler.sample(0), Readings::default());
        assert_eq!(sampler.sample(0), Readings::default());
    }

    /// Samples this test process while it pushes data over loopback TCP
    /// and burns CPU. Real I/O and timing, so run it explicitly with
    /// `cargo test -- --ignored live_sampling --nocapture`.
    #[test]
    #[ignore]
    fn live_sampling() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};

        const TRANSFER: usize = 8 * 1024 * 1024;
        const CHUNK: usize = 64 * 1024;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();

        let root = Pid::try_from(std::process::id()).unwrap();
        let mut sampler = Sampler::new();
        let baseline = sampler.sample(root);
        assert!(baseline.net.is_none() && baseline.cpu_percent.is_none());

        // Both ends live in this process, so up and down both see it.
        let reader = std::thread::spawn(move || {
            let mut buffer = vec![0; CHUNK];
            let mut total = 0;
            while total < TRANSFER {
                total += server.read(&mut buffer).unwrap();
            }
            // Handed back: a socket closed before the next sample loses
            // its bytes.
            (server, total)
        });
        let chunk = vec![7u8; CHUNK];
        for _ in 0..TRANSFER / CHUNK {
            client.write_all(&chunk).unwrap();
        }
        let (server, received) = reader.join().unwrap();
        assert_eq!(received, TRANSFER);
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut spin = 0u64;
        while Instant::now() < deadline {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }

        let readings = sampler.sample(root);
        eprintln!("live readings: {readings:?}");
        for kind in ReadingKind::ALL {
            eprintln!("  {}: {}", kind.label(), readings.text(kind));
        }
        let net = readings.net.expect("network reading");
        let elapsed_upper_bound = 30.0;
        let min_rate = TRANSFER as f64 / elapsed_upper_bound;
        assert!(net.up >= min_rate, "{net:?}");
        assert!(net.down >= min_rate, "{net:?}");
        let cpu = readings.cpu_percent.expect("cpu reading");
        assert!(cpu > 1.0, "{cpu}");
        let memory = readings.memory_bytes.expect("memory reading");
        assert!(memory > 1024 * 1024, "{memory}");
        drop((client, server));
    }
}
