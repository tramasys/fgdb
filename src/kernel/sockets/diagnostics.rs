//! Bounded, read-only INET_DIAG queries. No calls execute in the inferior.

use std::{
    fs::{self, File},
    net::{IpAddr, SocketAddr},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::Path,
    time::Duration,
};

use nix::{
    errno::Errno,
    sched::{CloneFlags, setns},
    sys::socket::{
        AddressFamily, MsgFlags, NetlinkAddr, SockFlag, SockProtocol, SockType, recvfrom, sendto,
        socket,
    },
};

use super::{Protocol, SocketInfo};
use crate::kernel::{KernelFact, WorkDeadline, fact, read_verified_local_proc};

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub pid: u32,
    pub debugger_pid: u32,
    pub start_time: u64,
    pub fd: u32,
    pub inode: u64,
    pub socket: std::sync::Arc<SocketInfo>,
}

/// Call only from a disposable worker: setns affects this thread until it exits.
pub(crate) fn read_on_worker(
    request: &Request,
    work: &WorkDeadline,
) -> Result<Vec<KernelFact>, String> {
    read_verified_local_proc(request.pid, request.debugger_pid, |target| {
        if target.start_time() != request.start_time {
            return Err("The selected process has changed".into());
        }

        let fd = target.root().join("fd").join(request.fd.to_string());
        let expected = format!("socket:[{}]", request.inode);
        let check = || {
            if fs::read_link(&fd).ok().as_deref() == Some(Path::new(&expected)) {
                Ok(())
            } else {
                Err(String::from("The selected descriptor was closed or reused"))
            }
        };

        work.check()
            .map_err(|_| "TCP diagnostics timed out or were cancelled".to_owned())?;
        check()?;
        let namespace =
            File::open(target.root().join("ns/net")).map_err(|error| error.to_string())?;
        let target_ns = namespace.metadata().map_err(|error| error.to_string())?;
        let current_ns =
            fs::metadata("/proc/thread-self/ns/net").map_err(|error| error.to_string())?;

        if (target_ns.dev(), target_ns.ino()) != (current_ns.dev(), current_ns.ino()) {
            setns(&namespace, CloneFlags::CLONE_NEWNET)
                .map_err(|error| format!("Cannot inspect the target network namespace: {error}"))?;
        }

        let result = query(&request.socket, request.inode, work)?;
        check()?;
        Ok(result)
    })
}

fn query(info: &SocketInfo, inode: u64, work: &WorkDeadline) -> Result<Vec<KernelFact>, String> {
    let request = encode_request(info)?;
    let fd = socket(
        AddressFamily::Netlink,
        SockType::Raw,
        SockFlag::SOCK_CLOEXEC | SockFlag::SOCK_NONBLOCK,
        SockProtocol::NetlinkSockDiag,
    )
    .map_err(|error| format!("Socket diagnostics unavailable: {error}"))?;

    loop {
        work.check()
            .map_err(|_| "TCP diagnostics timed out or were cancelled".to_owned())?;

        match sendto(
            fd.as_raw_fd(),
            &request,
            &NetlinkAddr::new(0, 0),
            MsgFlags::empty(),
        ) {
            Ok(size) if size == request.len() => break,
            Err(Errno::EINTR) => continue,
            result => return Err(format!("Cannot request socket diagnostics: {result:?}")),
        }
    }

    let mut bytes = vec![0; 64 * 1024];
    let mut received = 0;

    loop {
        work.check()
            .map_err(|_| "TCP diagnostics timed out or were cancelled".to_owned())?;

        match recvfrom::<NetlinkAddr>(fd.as_raw_fd(), &mut bytes) {
            Ok((size, Some(sender))) if sender.pid() == 0 => {
                received += size;

                if size == 0 || size == bytes.len() || received > 16 * 1024 * 1024 {
                    return Err("Socket diagnostic response exceeded its capture limit".into());
                }

                if let Some(facts) = decode_datagram(&bytes[..size], inode)? {
                    return Ok(facts);
                }
            }

            Ok(_) => return Err("Socket diagnostics returned an unexpected sender".into()),
            Err(Errno::EINTR) => continue,
            Err(Errno::EAGAIN) => std::thread::sleep(Duration::from_millis(5)),
            Err(error) => return Err(format!("Cannot receive socket diagnostics: {error}")),
        }
    }
}

fn encode_request(info: &SocketInfo) -> Result<[u8; 72], String> {
    if !info.protocol.is_tcp() {
        return Err("TCP diagnostics require a TCP socket".into());
    }

    let local = info.local.ok_or("Local socket address unavailable")?;
    let peer = info.peer.unwrap_or_else(|| {
        SocketAddr::new(
            if info.protocol == Protocol::Tcp {
                IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
            } else {
                IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
            },
            0,
        )
    });

    // Linux UAPI: nlmsghdr (16) + inet_diag_req_v2 (56).
    let mut bytes = [0_u8; 72];
    bytes[0..4].copy_from_slice(&72_u32.to_ne_bytes());
    bytes[4..6].copy_from_slice(&20_u16.to_ne_bytes()); // SOCK_DIAG_BY_FAMILY
    // Exact endpoint lookups can select another SO_REUSEPORT listener.
    bytes[6..8].copy_from_slice(&0x301_u16.to_ne_bytes()); // NLM_F_REQUEST | NLM_F_DUMP
    bytes[8..12].copy_from_slice(&1_u32.to_ne_bytes());
    bytes[16] = if info.protocol == Protocol::Tcp {
        2
    } else {
        10
    };
    bytes[17] = 6; // IPPROTO_TCP
    bytes[18] = (1 << 1) | (1 << 3) | (1 << 6); // INFO, CONG, SKMEMINFO
    bytes[20..24].copy_from_slice(&u32::MAX.to_ne_bytes());
    bytes[24..26].copy_from_slice(&local.port().to_be_bytes());
    bytes[26..28].copy_from_slice(&peer.port().to_be_bytes());

    for (offset, address) in [(28, local.ip()), (44, peer.ip())] {
        match address {
            IpAddr::V4(address) => bytes[offset..offset + 4].copy_from_slice(&address.octets()),
            IpAddr::V6(address) => bytes[offset..offset + 16].copy_from_slice(&address.octets()),
        }
    }

    bytes[64..72].fill(0xff); // INET_DIAG_NOCOOKIE
    Ok(bytes)
}

fn word(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_ne_bytes)
        .ok_or_else(|| "Truncated socket diagnostics".into())
}

fn decode_datagram(mut bytes: &[u8], inode: u64) -> Result<Option<Vec<KernelFact>>, String> {
    while !bytes.is_empty() {
        let length = word(bytes, 0)? as usize;

        if length < 16 || length > bytes.len() || word(bytes, 8)? != 1 {
            return Err("Invalid socket diagnostic message".into());
        }

        let message = &bytes[..length];
        let kind = u16::from_ne_bytes(message[4..6].try_into().unwrap());
        let flags = u16::from_ne_bytes(message[6..8].try_into().unwrap());

        if flags & 0x10 != 0 {
            return Err("Socket diagnostic dump was interrupted; retry the inspection".into());
        }

        match kind {
            2 => return decode_reply(message, inode).map(Some), // NLMSG_ERROR
            3 => return Err("Socket diagnostics did not return the selected socket".into()),
            20 if length >= 88 => {
                if u64::from(word(message, 84)?) == inode {
                    return decode_reply(message, inode).map(Some);
                }
            }
            _ => return Err("Unexpected socket diagnostic message".into()),
        }

        let aligned = (length + 3) & !3;

        if length == bytes.len() {
            break;
        }

        bytes = bytes
            .get(aligned..)
            .ok_or("Truncated socket diagnostic padding")?;
    }

    Ok(None)
}

fn decode_reply(bytes: &[u8], inode: u64) -> Result<Vec<KernelFact>, String> {
    let length = word(bytes, 0)? as usize;

    if length < 16 || length > bytes.len() || word(bytes, 8)? != 1 {
        return Err("Invalid socket diagnostic message".into());
    }

    let kind = u16::from_ne_bytes(bytes[4..6].try_into().unwrap());

    if kind == 2 {
        let error = word(&bytes[..length], 16)? as i32;
        return Err(format!(
            "Socket diagnostics unavailable: {}",
            std::io::Error::from_raw_os_error(error.saturating_neg())
        ));
    }

    if kind != 20 || length < 88 {
        return Err("Socket diagnostics did not return the selected socket".into());
    }

    let message = &bytes[16..length];

    if u64::from(word(message, 68)?) != inode {
        return Err("The socket identity changed while collecting diagnostics".into());
    }

    let listening = message[1] == 10;
    let mut facts = vec![
        fact("Source", "Linux socket diagnostics; captured on request"),
        fact("TCP state", super::tcp_state(message[1])),
        fact(
            if listening {
                "Pending connections"
            } else {
                "Receive queue (bytes)"
            },
            word(message, 56)?.to_string(),
        ),
        fact(
            if listening {
                "Maximum backlog"
            } else {
                "Send queue (bytes)"
            },
            word(message, 60)?.to_string(),
        ),
    ];

    let mut memory_facts = Vec::new();
    let mut attributes = &message[72..];

    while !attributes.is_empty() {
        if attributes.len() < 4 {
            return Err("Truncated socket diagnostic attribute".into());
        }

        let length = usize::from(u16::from_ne_bytes(attributes[..2].try_into().unwrap()));
        let kind = u16::from_ne_bytes(attributes[2..4].try_into().unwrap()) & 0x3fff;

        if length < 4 || length > attributes.len() {
            return Err("Invalid socket diagnostic attribute length".into());
        }

        let value = &attributes[4..length];

        match kind {
            2 if !listening => {
                for (offset, label, unit) in [
                    (8, "Retransmission timeout", "µs"),
                    (24, "Unacknowledged segments", "segments"),
                    (32, "Lost segments", "segments"),
                    (68, "RTT", "µs"),
                    (72, "RTT variation", "µs"),
                    (80, "Congestion window", "segments"),
                    (100, "Total retransmissions", "segments"),
                ] {
                    if let Ok(value) = word(value, offset) {
                        facts.push(fact(label, format!("{value} {unit}")));
                    }
                }
            }

            4 => facts.push(fact(
                "Congestion algorithm",
                String::from_utf8_lossy(value.split(|byte| *byte == 0).next().unwrap_or_default()),
            )),
            7 => {
                for (index, label) in [
                    "Receive memory",
                    "Receive buffer limit",
                    "Send memory",
                    "Send buffer limit",
                    "Forward allocation",
                    "Queued send memory",
                    "Option memory",
                    "Backlog memory",
                    "Socket drops",
                ]
                .into_iter()
                .enumerate()
                {
                    if let Ok(value) = word(value, index * 4) {
                        memory_facts.push(fact(
                            label,
                            if index == 8 {
                                value.to_string()
                            } else {
                                format!("{value} B")
                            },
                        ));
                    }
                }
            }

            _ => {}
        }

        let aligned = (length + 3) & !3;

        if aligned > attributes.len() {
            if length == attributes.len() {
                break;
            }

            return Err("Truncated socket diagnostic padding".into());
        }

        attributes = &attributes[aligned..];
    }

    facts.extend(memory_facts);
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_diagnostic_lengths_identity_and_tcp_fields() {
        let mut bytes = vec![0_u8; 196];
        bytes[..4].copy_from_slice(&196_u32.to_ne_bytes());
        bytes[4..6].copy_from_slice(&20_u16.to_ne_bytes());
        bytes[8..12].copy_from_slice(&1_u32.to_ne_bytes());
        bytes[17] = 1;
        bytes[84..88].copy_from_slice(&42_u32.to_ne_bytes());
        bytes[88..90].copy_from_slice(&108_u16.to_ne_bytes());
        bytes[90..92].copy_from_slice(&2_u16.to_ne_bytes());
        bytes[160..164].copy_from_slice(&125_u32.to_ne_bytes());
        let facts = decode_reply(&bytes, 42).unwrap();
        assert!(
            facts
                .iter()
                .any(|fact| fact.label == "RTT" && fact.value == "125 µs")
        );
        assert!(decode_reply(&bytes, 43).is_err());
        assert!(decode_datagram(&bytes, 43).unwrap().is_none());
        let mut other = bytes.clone();
        other[84..88].copy_from_slice(&43_u32.to_ne_bytes());
        other[160..164].copy_from_slice(&250_u32.to_ne_bytes());
        let datagram = [bytes.as_slice(), other.as_slice()].concat();
        let facts = decode_datagram(&datagram, 43).unwrap().unwrap();
        assert!(
            facts
                .iter()
                .any(|fact| fact.label == "RTT" && fact.value == "250 µs")
        );

        let mut done = vec![0_u8; 20];
        done[..4].copy_from_slice(&20_u32.to_ne_bytes());
        done[4..6].copy_from_slice(&3_u16.to_ne_bytes());
        done[8..12].copy_from_slice(&1_u32.to_ne_bytes());
        assert!(decode_datagram(&done, 43).is_err());
        other[6..8].copy_from_slice(&0x10_u16.to_ne_bytes());
        assert!(decode_datagram(&other, 43).is_err());
        other[6..8].fill(0);
        other[8..12].copy_from_slice(&2_u32.to_ne_bytes());
        assert!(decode_datagram(&other, 43).is_err());

        for end in 0..bytes.len() {
            assert!(decode_reply(&bytes[..end], 42).is_err());

            if end != 0 {
                assert!(decode_datagram(&bytes[..end], 42).is_err());
            }
        }

        bytes[88..90].copy_from_slice(&3_u16.to_ne_bytes());
        assert!(decode_reply(&bytes, 42).is_err());
        bytes[88..90].copy_from_slice(&200_u16.to_ne_bytes());
        assert!(decode_reply(&bytes, 42).is_err());
    }
}
