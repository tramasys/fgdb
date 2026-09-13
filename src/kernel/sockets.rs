//! Socket metadata from the target's network namespace, joined to its open FDs.

use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    sync::Arc,
};

use super::WorkDeadline;

pub(crate) mod diagnostics;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Protocol {
    Tcp,
    Tcp6,
    Udp,
    Udp6,
    Unix,
}

impl Protocol {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Tcp => "TCP",
            Self::Tcp6 => "TCP6",
            Self::Udp => "UDP",
            Self::Udp6 => "UDP6",
            Self::Unix => "UNIX",
        }
    }

    pub(crate) fn is_tcp(self) -> bool {
        matches!(self, Self::Tcp | Self::Tcp6)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum QueueUnit {
    Bytes,
    Memory,
    Connections,
}

impl QueueUnit {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Bytes => "B",
            Self::Memory => "B memory",
            Self::Connections => "connections",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Queue {
    pub unit: QueueUnit,
    pub value: u64,
}

impl std::fmt::Display for Queue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}", self.value, self.unit.label())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SocketInfo {
    pub protocol: Protocol,
    pub state: u8,
    pub local: Option<SocketAddr>,
    pub peer: Option<SocketAddr>,
    pub unix_name: Option<String>,
    pub socket_type: Option<u16>,
    pub listening: bool,
    pub receive: Option<Queue>,
    pub send: Option<Queue>,
    pub raw: String,
}

impl SocketInfo {
    pub(crate) fn state_name(&self) -> &'static str {
        match self.protocol {
            Protocol::Tcp | Protocol::Tcp6 => tcp_state(self.state),
            Protocol::Udp | Protocol::Udp6 => match self.state {
                1 => "Connected",
                7 => "Unconnected",
                _ => "Unknown",
            },
            Protocol::Unix if self.listening => "Listening",
            Protocol::Unix => match self.state {
                1 => "Unconnected",
                2 => "Connecting",
                3 => "Connected",
                4 => "Disconnecting",
                _ => "Unknown",
            },
        }
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self.socket_type {
            Some(1) => "Stream",
            Some(2) => "Datagram",
            Some(5) => "Seqpacket",
            _ => "Unknown",
        }
    }

    pub(crate) fn local_name(&self) -> String {
        self.local.map_or_else(
            || self.unix_name.clone().unwrap_or_else(|| "Unnamed".into()),
            |address| address.to_string(),
        )
    }

    pub(crate) fn peer_name(&self) -> String {
        self.peer.map_or_else(
            || {
                if self.protocol == Protocol::Unix {
                    "Unavailable"
                } else {
                    "-"
                }
                .into()
            },
            |address| address.to_string(),
        )
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "{} {} → {}  {}",
            self.protocol.name(),
            self.local_name(),
            self.peer_name(),
            self.state_name()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EpollWatch {
    pub fd: u32,
    pub inode: Option<u64>,
    pub device: Option<(u32, u32)>,
    pub events: u32,
    pub data: u64,
}

impl EpollWatch {
    pub(crate) fn matches(&self, descriptor: &super::KernelFileDescriptor) -> bool {
        // Anonymous-inode descriptors can share an inode even after FD reuse.
        // Pipe ends and separately opened files also share inodes; only socket
        // device/inode pairs identify the registered open file description.
        descriptor.kind == "socket"
            && self.fd == descriptor.number
            && self.inode.is_some_and(|inode| inode != 0)
            && self.inode == descriptor.inode
            && self.device.is_some()
            && self.device == descriptor.device
    }

    pub(crate) fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split_whitespace();
        let mut fd = None;
        let mut events = None;
        let mut data = None;
        let mut inode = None;
        let mut device = None;

        while let Some(field) = fields.next() {
            let (key, value) = field.split_once(':')?;
            let value = if value.is_empty() {
                fields.next()?
            } else {
                value
            };

            match key {
                "tfd" => fd = value.parse().ok(),
                "events" => events = u32::from_str_radix(value, 16).ok(),
                "data" => data = u64::from_str_radix(value, 16).ok(),
                "ino" => inode = u64::from_str_radix(value, 16).ok(),
                "sdev" => {
                    // fdinfo prints the kernel's 12-bit major / 20-bit minor encoding.
                    device = u32::from_str_radix(value, 16)
                        .ok()
                        .map(|device| (device >> 20, device & 0xfffff));
                }
                _ => {}
            }
        }

        Some(Self {
            fd: fd?,
            inode,
            device,
            events: events?,
            data: data?,
        })
    }

    pub(crate) fn interests(&self) -> String {
        let mut flags = self.events;
        let mut names = Vec::new();

        for (flag, name) in [
            (1, "EPOLLIN"),
            (2, "EPOLLPRI"),
            (4, "EPOLLOUT"),
            (8, "EPOLLERR"),
            (16, "EPOLLHUP"),
            (0x2000, "EPOLLRDHUP"),
            (1 << 28, "EPOLLEXCLUSIVE"),
            (1 << 29, "EPOLLWAKEUP"),
            (1 << 30, "EPOLLONESHOT"),
            (1 << 31, "EPOLLET"),
        ] {
            if flags & flag != 0 {
                names.push(name.to_owned());
                flags &= !flag;
            }
        }

        if flags != 0 || names.is_empty() {
            names.push(format!("0x{flags:x}"));
        }

        names.join(" | ")
    }
}

pub(super) fn read(
    root: &Path,
    wanted: &HashSet<u64>,
    work: &WorkDeadline,
) -> HashMap<u64, Arc<SocketInfo>> {
    let mut sockets = HashMap::new();

    if wanted.is_empty() {
        return sockets;
    }

    for (entry, protocol) in [
        ("tcp", Protocol::Tcp),
        ("tcp6", Protocol::Tcp6),
        ("udp", Protocol::Udp),
        ("udp6", Protocol::Udp6),
        ("unix", Protocol::Unix),
    ] {
        if work.should_stop() {
            break;
        }

        let Ok(input) =
            crate::bounded::read_string(&root.join("net").join(entry), 16 * 1024 * 1024)
        else {
            continue;
        };

        for (index, line) in input.lines().skip(1).enumerate() {
            if index % 256 == 0 && work.should_stop() {
                return sockets;
            }

            let inode_column = if protocol == Protocol::Unix { 6 } else { 9 };

            let Some(inode) = line
                .split_whitespace()
                .nth(inode_column)
                .and_then(|value| value.parse::<u64>().ok())
            else {
                continue;
            };

            if wanted.contains(&inode)
                && let Some(socket) = parse(line, protocol)
            {
                sockets.insert(inode, Arc::new(socket));
            }
        }
    }

    sockets
}

fn parse(line: &str, protocol: Protocol) -> Option<SocketInfo> {
    let mut fields = line.split_whitespace();

    if protocol == Protocol::Unix {
        let flags = u32::from_str_radix(fields.nth(3)?, 16).ok()?;
        let socket_type = u16::from_str_radix(fields.next()?, 16).ok()?;
        let state = u8::from_str_radix(fields.next()?, 16).ok()?;
        fields.next()?;
        // Preserve the pathname remainder, including internal and trailing spaces.
        let mut remainder = line;

        for _ in 0..7 {
            remainder = remainder.trim_start_matches(char::is_whitespace);
            let end = remainder
                .find(char::is_whitespace)
                .unwrap_or(remainder.len());
            remainder = &remainder[end..];
        }

        let name = remainder.strip_prefix(' ').unwrap_or(remainder);

        return Some(SocketInfo {
            protocol,
            state,
            local: None,
            peer: None,
            unix_name: (!name.is_empty()).then(|| name.to_owned()),
            socket_type: Some(socket_type),
            listening: flags & 0x10000 != 0,
            receive: None,
            send: None,
            raw: line.to_owned(),
        });
    }

    let ipv6 = matches!(protocol, Protocol::Tcp6 | Protocol::Udp6);
    let local = decode_endpoint(fields.nth(1)?, ipv6)?;
    let peer = decode_endpoint(fields.next()?, ipv6)?;
    let state = u8::from_str_radix(fields.next()?, 16).ok()?;
    let (send, receive) = fields.next()?.split_once(':')?;
    let listening = protocol.is_tcp() && state == 10;

    let unit = if listening {
        QueueUnit::Connections
    } else if protocol.is_tcp() {
        QueueUnit::Bytes
    } else {
        QueueUnit::Memory
    };

    Some(SocketInfo {
        protocol,
        state,
        local: Some(local),
        peer: (!(peer.ip().is_unspecified() && peer.port() == 0)).then_some(peer),
        unix_name: None,
        socket_type: Some(if protocol.is_tcp() { 1 } else { 2 }),
        listening,
        receive: u64::from_str_radix(receive, 16)
            .ok()
            .map(|value| Queue { unit, value }),
        send: (!listening)
            .then(|| {
                u64::from_str_radix(send, 16)
                    .ok()
                    .map(|value| Queue { unit, value })
            })
            .flatten(),
        raw: line.to_owned(),
    })
}

pub(super) fn decode_endpoint(value: &str, ipv6: bool) -> Option<SocketAddr> {
    let (address, port) = value.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;

    if ipv6 {
        let mut bytes = [0_u8; 16];
        let (chunks, remainder) = address.as_bytes().as_chunks::<8>();

        if !remainder.is_empty() || chunks.len() != 4 {
            return None;
        }

        for (index, chunk) in chunks.iter().enumerate() {
            let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
            bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }

        Some(SocketAddr::new(Ipv6Addr::from(bytes).into(), port))
    } else {
        let word = u32::from_str_radix(address, 16).ok()?;
        Some(SocketAddr::new(
            Ipv4Addr::from(word.to_ne_bytes()).into(),
            port,
        ))
    }
}

fn tcp_state(state: u8) -> &'static str {
    match state {
        1 => "ESTABLISHED",
        2 => "SYN_SENT",
        3 => "SYN_RECV",
        4 => "FIN_WAIT1",
        5 => "FIN_WAIT2",
        6 => "TIME_WAIT",
        7 => "CLOSE",
        8 => "CLOSE_WAIT",
        9 => "LAST_ACK",
        10 => "LISTEN",
        11 => "CLOSING",
        12 => "NEW_SYN_RECV",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_socket_states_queue_units_unix_names_and_epoll_interests() {
        let entry =
            "0: 0100007F:1F90 0100007F:C001 01 00000004:00000015 00:00000000 00000000 1000 0 42";
        let tcp = parse(entry, Protocol::Tcp).unwrap();
        assert_eq!(tcp.local.unwrap(), "127.0.0.1:8080".parse().unwrap());
        assert_eq!(
            tcp.receive,
            Some(Queue {
                unit: QueueUnit::Bytes,
                value: 21
            })
        );
        assert_eq!(tcp.state_name(), "ESTABLISHED");
        let udp = parse(&entry.replace(" 01 ", " 07 "), Protocol::Udp).unwrap();
        assert_eq!(udp.state_name(), "Unconnected");
        assert_eq!(udp.receive.unwrap().unit, QueueUnit::Memory);
        let listener = parse(
            &entry.replace("0100007F:C001 01", "00000000:0000 0A"),
            Protocol::Tcp,
        )
        .unwrap();
        assert!(listener.peer.is_none());
        assert_eq!(listener.receive.unwrap().unit, QueueUnit::Connections);
        assert!(listener.send.is_none());
        let address = "00000000000000000000000001000000:1F90";
        assert_eq!(
            decode_endpoint(address, true),
            Some("[::1]:8080".parse().unwrap())
        );

        for (path, flags, kind, state) in [
            ("@fgdb name ", "00010000", "0001", "01"),
            ("/tmp/name with spaces", "00000000", "0005", "03"),
            ("", "00000000", "0002", "01"),
        ] {
            let unix = parse(
                &format!("00000000: 00000002 00000000 {flags} {kind} {state} 42 {path}"),
                Protocol::Unix,
            )
            .unwrap();
            assert_eq!(
                unix.unix_name.as_deref(),
                (!path.is_empty()).then_some(path)
            );
            assert_eq!(unix.listening, flags == "00010000");
            assert_eq!(
                unix.type_name(),
                match kind {
                    "0001" => "Stream",
                    "0005" => "Seqpacket",
                    _ => "Datagram",
                }
            );
            assert!(unix.receive.is_none());
        }

        let watch =
            EpollWatch::parse("tfd: 5 events: 80002019 data: 1234 pos:0 ino:2a sdev:9").unwrap();
        assert_eq!((watch.fd, watch.inode, watch.data), (5, Some(42), 0x1234));
        assert_eq!(watch.device, Some((0, 9)));
        assert!(watch.interests().contains("EPOLLIN"));
        assert!(watch.interests().contains("EPOLLET"));
        assert!(EpollWatch::parse("tfd: missing events: 1").is_none());

        for line in [
            "",
            "short",
            "0: invalid invalid 01 0:0",
            "0: 0100007F:ZZZZ 0100007F:0001 01 0:0",
        ] {
            assert!(parse(line, Protocol::Tcp).is_none());
        }
    }
}
