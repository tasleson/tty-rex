// telemetry/sock_diag.rs
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

//! Per-socket TCP byte counters through `NETLINK_SOCK_DIAG`, the
//! interface `ss -ti` uses. Dumping TCP sockets with `INET_DIAG_INFO`
//! needs no privileges.
//!
//! This speaks the small, stable netlink protocol directly over a `libc`
//! socket instead of pulling in netlink crates: it needs one request
//! layout and three structures, which keeps the dependency footprint at
//! the `libc` the crate already uses. The layouts follow
//! `<linux/netlink.h>`, `<linux/sock_diag.h>`, `<linux/inet_diag.h>` and
//! `<linux/tcp.h>`, in native byte order.
//!
//! Only sockets of tty-rex's network namespace are visible; processes in
//! another one (e.g. a container with its own network) are not counted.

use std::collections::{HashMap, HashSet};
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

/// `struct nlmsghdr`: length, type, flags, sequence number, port id.
const NLMSG_HDR_LEN: usize = 16;
/// Netlink messages and attributes are padded to four bytes.
const NL_ALIGN: usize = 4;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 0x01;
/// `NLM_F_ROOT | NLM_F_MATCH`.
const NLM_F_DUMP: u16 = 0x300;
/// `SOCK_DIAG_BY_FAMILY` from `<linux/sock_diag.h>`.
const SOCK_DIAG_BY_FAMILY: u16 = 20;

/// `struct inet_diag_req_v2` without its `inet_diag_sockid`.
const INET_DIAG_REQ_HEAD_LEN: usize = 8;
/// `struct inet_diag_sockid`: ports, addresses, interface and cookie.
const INET_DIAG_SOCKID_LEN: usize = 48;
const INET_DIAG_REQ_LEN: usize = INET_DIAG_REQ_HEAD_LEN + INET_DIAG_SOCKID_LEN;
/// `struct inet_diag_msg`, which the attributes follow.
const INET_DIAG_MSG_LEN: usize = 72;
/// Offset of `idiag_inode` in `struct inet_diag_msg`.
const INET_DIAG_MSG_INODE: usize = 68;
/// Attribute carrying `struct tcp_info`.
const INET_DIAG_INFO: u16 = 2;
/// `struct rtattr`: length and type.
const RTATTR_HDR_LEN: usize = 4;

/// Offsets of `tcpi_bytes_acked` and `tcpi_bytes_received` in
/// `struct tcp_info` (both since Linux 4.1).
const TCPI_BYTES_ACKED: usize = 120;
const TCPI_BYTES_RECEIVED: usize = 128;

/// `TCP_LISTEN` from `<netinet/tcp.h>`; listening sockets move no data.
const TCP_LISTEN: u32 = 10;
/// Every TCP state (`TCPF_ALL`) except listening.
const DUMPED_STATES: u32 = 0xfff & !(1 << TCP_LISTEN);

/// Large enough for the kernel's dump batches (it fills up to 32 KiB).
const RECV_BUFFER_LEN: usize = 64 * 1024;
/// A dump normally completes in milliseconds; this bounds a stuck one.
const RECV_TIMEOUT: Duration = Duration::from_secs(1);

/// Lifetime byte counters of one TCP socket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TcpCounters {
    /// Bytes sent and acknowledged by the peer.
    pub bytes_acked: u64,
    pub bytes_received: u64,
}

fn align(len: usize) -> usize {
    len.div_ceil(NL_ALIGN) * NL_ALIGN
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

/// The error a negative errno in an `NLMSG_ERROR` or `NLMSG_DONE`
/// payload reports, or `None` for 0 (success).
fn payload_error(payload: &[u8]) -> io::Result<Option<io::Error>> {
    let code = read_u32(payload, 0).ok_or_else(|| invalid("short status"))? as i32;
    Ok(match code {
        0 => None,
        // Anything else positive or i32::MIN breaks the protocol.
        code if code < 0 && code != i32::MIN => Some(io::Error::from_raw_os_error(-code)),
        _ => Some(invalid("bad status")),
    })
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed sock_diag reply: {what}"),
    )
}

/// A `SOCK_DIAG_BY_FAMILY` dump request for TCP sockets of `family`
/// (`AF_INET` or `AF_INET6`) asking for `tcp_info`.
pub fn build_request(family: u8, sequence: u32) -> Vec<u8> {
    let len = NLMSG_HDR_LEN + INET_DIAG_REQ_LEN;
    let mut request = Vec::with_capacity(len);
    request.extend_from_slice(&(len as u32).to_ne_bytes());
    request.extend_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    request.extend_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
    request.extend_from_slice(&sequence.to_ne_bytes());
    // Port id 0 addresses the kernel.
    request.extend_from_slice(&0u32.to_ne_bytes());
    // sdiag_family, sdiag_protocol, idiag_ext (a bit per attribute,
    // shifted by one), pad.
    request.extend_from_slice(&[
        family,
        libc::IPPROTO_TCP as u8,
        1 << (INET_DIAG_INFO - 1),
        0,
    ]);
    request.extend_from_slice(&DUMPED_STATES.to_ne_bytes());
    // An all-zero socket id matches every socket in a dump.
    request.resize(len, 0);
    request
}

/// The payload of the first `kind` attribute in `attributes`.
pub fn find_attribute(attributes: &[u8], kind: u16) -> Option<&[u8]> {
    let mut rest = attributes;
    while rest.len() >= RTATTR_HDR_LEN {
        let len = usize::from(read_u16(rest, 0)?);
        if len < RTATTR_HDR_LEN || len > rest.len() {
            return None;
        }
        if read_u16(rest, 2)? == kind {
            return Some(&rest[RTATTR_HDR_LEN..len]);
        }
        rest = rest.get(align(len)..).unwrap_or_default();
    }
    None
}

/// The byte counters of a `struct tcp_info` payload, or `None` when the
/// kernel's structure predates them.
pub fn tcp_info_counters(tcp_info: &[u8]) -> Option<TcpCounters> {
    Some(TcpCounters {
        bytes_acked: read_u64(tcp_info, TCPI_BYTES_ACKED)?,
        bytes_received: read_u64(tcp_info, TCPI_BYTES_RECEIVED)?,
    })
}

/// Whether a dump is complete after one received batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    More,
    Done,
}

/// Parses one batch of dump replies, passing each socket's inode and
/// attributes to `on_socket`.
pub fn parse_batch(batch: &[u8], mut on_socket: impl FnMut(u64, &[u8])) -> io::Result<Progress> {
    let mut rest = batch;
    while !rest.is_empty() {
        let len = read_u32(rest, 0).ok_or_else(|| invalid("short header"))? as usize;
        if len < NLMSG_HDR_LEN || len > rest.len() {
            return Err(invalid("bad message length"));
        }
        let message = &rest[..len];
        let payload = &message[NLMSG_HDR_LEN..];
        match read_u16(message, 4).ok_or_else(|| invalid("short header"))? {
            // A dump that failed part way ends with its errno in DONE.
            NLMSG_DONE => {
                return match payload_error(payload)? {
                    None => Ok(Progress::Done),
                    Some(error) => Err(error),
                };
            }
            // Only sent on failure, as the request asks for no ACK.
            NLMSG_ERROR => {
                return Err(
                    payload_error(payload)?.unwrap_or_else(|| invalid("acknowledgement in a dump"))
                );
            }
            SOCK_DIAG_BY_FAMILY => {
                if payload.len() < INET_DIAG_MSG_LEN {
                    return Err(invalid("short inet_diag_msg"));
                }
                let inode = read_u32(payload, INET_DIAG_MSG_INODE)
                    .ok_or_else(|| invalid("short inet_diag_msg"))?;
                on_socket(u64::from(inode), &payload[INET_DIAG_MSG_LEN..]);
            }
            _ => {}
        }
        rest = rest.get(align(len)..).unwrap_or_default();
    }
    Ok(Progress::More)
}

/// A netlink socket connected to the kernel's sock_diag.
#[derive(Debug)]
struct DiagSocket(OwnedFd);

impl DiagSocket {
    fn open() -> io::Result<Self> {
        // SAFETY: plain socket(2) call; the result is checked below.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                libc::NETLINK_SOCK_DIAG,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a freshly created descriptor nothing else owns.
        let socket = Self(unsafe { OwnedFd::from_raw_fd(fd) });
        let timeout = libc::timeval {
            tv_sec: RECV_TIMEOUT.as_secs() as libc::time_t,
            tv_usec: RECV_TIMEOUT.subsec_micros() as libc::suseconds_t,
        };
        // SAFETY: `timeout` is a valid timeval of the size passed.
        let result = unsafe {
            libc::setsockopt(
                socket.0.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const timeout).cast(),
                mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(socket)
    }

    fn kernel_address() -> libc::sockaddr_nl {
        // SAFETY: sockaddr_nl is plain data for which all zeroes is valid.
        let mut address: libc::sockaddr_nl = unsafe { mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address
    }

    fn send(&self, request: &[u8]) -> io::Result<()> {
        let address = Self::kernel_address();
        // SAFETY: `request` and `address` are valid for the lengths given.
        let sent = unsafe {
            libc::sendto(
                self.0.as_raw_fd(),
                request.as_ptr().cast(),
                request.len(),
                0,
                (&raw const address).cast(),
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        match usize::try_from(sent) {
            Ok(sent) if sent == request.len() => Ok(()),
            Ok(_) => Err(io::Error::other("short netlink send")),
            Err(_) => Err(io::Error::last_os_error()),
        }
    }

    /// Receives one batch from the kernel into `buffer`, returning its
    /// length; datagrams from anyone else are dropped.
    fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut address = Self::kernel_address();
            let mut address_len = mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            // SAFETY: `buffer` and `address` are valid for the lengths given.
            let received = unsafe {
                libc::recvfrom(
                    self.0.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_TRUNC,
                    (&raw mut address).cast(),
                    &mut address_len,
                )
            };
            let received = match usize::try_from(received) {
                Ok(received) => received,
                Err(_) => {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
            };
            if received > buffer.len() {
                return Err(invalid("batch larger than the receive buffer"));
            }
            if address.nl_pid == 0 {
                return Ok(received);
            }
        }
    }
}

/// Reads TCP byte counters, keeping its netlink socket and receive buffer
/// between calls instead of setting them up for every sample.
#[derive(Debug, Default)]
pub struct DiagClient {
    /// Opened on first use; dropped after an error, as an unfinished dump
    /// may leave replies queued on it.
    socket: Option<DiagSocket>,
    buffer: Vec<u8>,
}

impl DiagClient {
    /// Byte counters of the TCP sockets among `inodes` (IPv4 and IPv6).
    /// Sockets without `tcp_info` counters are left out.
    pub fn tcp_counters(&mut self, inodes: &HashSet<u64>) -> io::Result<HashMap<u64, TcpCounters>> {
        if inodes.is_empty() {
            return Ok(HashMap::new());
        }
        let result = self.dump(inodes);
        if result.is_err() {
            self.socket = None;
        }
        result
    }

    fn dump(&mut self, inodes: &HashSet<u64>) -> io::Result<HashMap<u64, TcpCounters>> {
        let socket = match &mut self.socket {
            Some(socket) => socket,
            none => none.insert(DiagSocket::open()?),
        };
        self.buffer.resize(RECV_BUFFER_LEN, 0);
        let mut counters = HashMap::new();
        for (sequence, family) in [libc::AF_INET, libc::AF_INET6].into_iter().enumerate() {
            socket.send(&build_request(family as u8, sequence as u32 + 1))?;
            loop {
                let len = socket.receive(&mut self.buffer)?;
                let progress = parse_batch(&self.buffer[..len], |inode, attributes| {
                    if inodes.contains(&inode)
                        && let Some(info) = find_attribute(attributes, INET_DIAG_INFO)
                        && let Some(socket_counters) = tcp_info_counters(info)
                    {
                        counters.insert(inode, socket_counters);
                    }
                })?;
                if progress == Progress::Done {
                    break;
                }
            }
        }
        Ok(counters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attribute(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = ((RTATTR_HDR_LEN + payload.len()) as u16)
            .to_ne_bytes()
            .to_vec();
        bytes.extend_from_slice(&kind.to_ne_bytes());
        bytes.extend_from_slice(payload);
        bytes.resize(align(bytes.len()), 0);
        bytes
    }

    fn tcp_info(acked: u64, received: u64, len: usize) -> Vec<u8> {
        let mut info = vec![0xaa; len.max(TCPI_BYTES_RECEIVED + 8)];
        info[TCPI_BYTES_ACKED..TCPI_BYTES_ACKED + 8].copy_from_slice(&acked.to_ne_bytes());
        info[TCPI_BYTES_RECEIVED..TCPI_BYTES_RECEIVED + 8].copy_from_slice(&received.to_ne_bytes());
        info.truncate(len);
        info
    }

    fn message(kind: u16, payload: &[u8]) -> Vec<u8> {
        let len = NLMSG_HDR_LEN + payload.len();
        let mut bytes = (len as u32).to_ne_bytes().to_vec();
        bytes.extend_from_slice(&kind.to_ne_bytes());
        bytes.extend_from_slice(&[0; 10]);
        bytes.extend_from_slice(payload);
        bytes.resize(align(bytes.len()), 0);
        bytes
    }

    fn diag_message(inode: u32, attributes: &[u8]) -> Vec<u8> {
        let mut payload = vec![0; INET_DIAG_MSG_LEN];
        payload[INET_DIAG_MSG_INODE..INET_DIAG_MSG_INODE + 4].copy_from_slice(&inode.to_ne_bytes());
        payload.extend_from_slice(attributes);
        message(SOCK_DIAG_BY_FAMILY, &payload)
    }

    #[test]
    fn request_layout() {
        let request = build_request(libc::AF_INET6 as u8, 7);
        assert_eq!(request.len(), 72);
        assert_eq!(read_u32(&request, 0), Some(72));
        assert_eq!(read_u16(&request, 4), Some(SOCK_DIAG_BY_FAMILY));
        assert_eq!(read_u16(&request, 6), Some(0x301));
        assert_eq!(read_u32(&request, 8), Some(7));
        assert_eq!(
            &request[16..20],
            &[libc::AF_INET6 as u8, libc::IPPROTO_TCP as u8, 0b10, 0]
        );
        assert_eq!(read_u32(&request, 20), Some(0xbff));
        assert!(request[24..].iter().all(|&b| b == 0));
    }

    #[test]
    fn tcp_info_extraction() {
        let cases = [
            (tcp_info(1, 2, 136), Some((1, 2))),
            // Newer kernels append fields.
            (tcp_info(u64::MAX, 0, 248), Some((u64::MAX, 0))),
            // Kernels before 4.1 stop short of the counters.
            (tcp_info(1, 2, 104), None),
            (tcp_info(1, 2, 135), None),
            (Vec::new(), None),
        ];
        for (info, expected) in cases {
            let expected = expected.map(|(bytes_acked, bytes_received)| TcpCounters {
                bytes_acked,
                bytes_received,
            });
            assert_eq!(tcp_info_counters(&info), expected, "len {}", info.len());
        }
    }

    #[test]
    fn attributes() {
        let info = tcp_info(5, 6, 136);
        let both = [attribute(1, &[1, 2, 3]), attribute(INET_DIAG_INFO, &info)].concat();
        type Case<'a> = (&'a [u8], u16, Option<&'a [u8]>);
        let cases: [Case; 5] = [
            (&both, INET_DIAG_INFO, Some(&info)),
            (&both, 1, Some(&[1, 2, 3])),
            (&both, 9, None),
            // A length running past the buffer is not trusted.
            (&[200, 0, 2, 0, 1, 2], INET_DIAG_INFO, None),
            (&[], INET_DIAG_INFO, None),
        ];
        for (attributes, kind, expected) in cases {
            assert_eq!(find_attribute(attributes, kind), expected, "kind {kind}");
        }
    }

    #[test]
    fn batches() {
        let info = attribute(INET_DIAG_INFO, &tcp_info(10, 20, 136));
        let socket = |inode| diag_message(inode, &info);
        let error = message(NLMSG_ERROR, &(-libc::EACCES).to_ne_bytes());
        let done = message(NLMSG_DONE, &0i32.to_ne_bytes());
        let failed_done = message(NLMSG_DONE, &(-libc::ENOBUFS).to_ne_bytes());
        let ack = message(NLMSG_ERROR, &0i32.to_ne_bytes());
        let bad_errno = message(NLMSG_ERROR, &i32::MIN.to_ne_bytes());
        type Case = (Vec<u8>, Result<Progress, Option<i32>>, &'static [u64]);
        let cases: [Case; 10] = [
            ([socket(1), socket(2)].concat(), Ok(Progress::More), &[1, 2]),
            ([socket(3), done.clone()].concat(), Ok(Progress::Done), &[3]),
            (done.clone(), Ok(Progress::Done), &[]),
            (error, Err(Some(libc::EACCES)), &[]),
            (
                [socket(4), failed_done].concat(),
                Err(Some(libc::ENOBUFS)),
                &[4],
            ),
            (ack, Err(None), &[]),
            (bad_errno, Err(None), &[]),
            // DONE without its status.
            (message(NLMSG_DONE, &[]), Err(None), &[]),
            (socket(1)[..20].to_vec(), Err(None), &[]),
            (message(SOCK_DIAG_BY_FAMILY, &[0; 8]), Err(None), &[]),
        ];
        for (batch, expected, inodes) in cases {
            let mut seen = Vec::new();
            let result = parse_batch(&batch, |inode, attributes| {
                assert!(find_attribute(attributes, INET_DIAG_INFO).is_some());
                seen.push(inode);
            });
            match (result, expected) {
                (Ok(progress), Ok(expected)) => assert_eq!(progress, expected),
                (Err(e), Err(errno)) => assert_eq!(e.raw_os_error(), errno),
                (result, expected) => panic!("{result:?} != {expected:?}"),
            }
            assert_eq!(seen, inodes);
        }
    }
}
