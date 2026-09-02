use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Write},
    os::fd::OwnedFd,
    path::PathBuf,
};

use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    pty::openpty,
    sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr},
    unistd::ttyname,
};

pub(super) const MAX_QUEUED_MI_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_MI_WRITE_BATCH_BYTES: usize = 256 * 1024;

pub(super) struct OutgoingCommand {
    pub(super) token: u64,
    pub(super) priority: u8,
    pub(super) bytes: Vec<u8>,
    pub(super) written: usize,
}

#[derive(Default)]
pub(super) struct OutgoingQueue {
    pub(super) commands: VecDeque<OutgoingCommand>,
    pub(super) remaining_bytes: usize,
}

impl OutgoingQueue {
    pub(super) fn enqueue(&mut self, token: u64, priority: u8, command: &str) -> io::Result<()> {
        let capacity = command
            .len()
            .checked_add(21)
            .ok_or_else(|| io::Error::other("GDB/MI command size overflow"))?;

        let mut bytes = Vec::with_capacity(capacity);
        writeln!(&mut bytes, "{token}{command}")?;

        let new_size = self
            .remaining_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("GDB/MI output queue size overflow"))?;

        if new_size > MAX_QUEUED_MI_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "GDB/MI output queue exceeds the 8 MiB limit",
            ));
        }

        self.remaining_bytes = new_size;

        let command = OutgoingCommand {
            token,
            priority,
            bytes,
            written: 0,
        };

        // A command already partially written must remain at the front so MI
        // records are never interleaved. Everything else is stable-sorted by
        // class, allowing execution/control work to overtake queued bulk
        // inspection without reordering requests in the same class.
        let first_reorderable = usize::from(
            self.commands
                .front()
                .is_some_and(|queued| queued.written != 0),
        );

        let position = self
            .commands
            .iter()
            .enumerate()
            .skip(first_reorderable)
            .find_map(|(index, queued)| (queued.priority > priority).then_some(index));

        if let Some(position) = position {
            self.commands.insert(position, command);
        } else {
            self.commands.push_back(command);
        }

        Ok(())
    }

    pub(super) fn advance(&mut self, count: usize) {
        let Some(command) = self.commands.front_mut() else {
            return;
        };

        let count = count.min(command.bytes.len().saturating_sub(command.written));
        command.written += count;
        self.remaining_bytes = self.remaining_bytes.saturating_sub(count);

        if command.written == command.bytes.len() {
            self.commands.pop_front();
        }
    }

    pub(super) fn cancel_unstarted(&mut self, token: u64) -> bool {
        let Some(index) = self
            .commands
            .iter()
            .position(|command| command.token == token && command.written == 0)
        else {
            return false;
        };

        if let Some(command) = self.commands.remove(index) {
            self.remaining_bytes = self.remaining_bytes.saturating_sub(command.bytes.len());
        }

        true
    }

    pub(super) fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.commands.clear();
        self.remaining_bytes = 0;
    }
}

pub(super) fn drain_outgoing(
    writer: &mut impl Write,
    outgoing: &mut OutgoingQueue,
    byte_budget: usize,
) -> io::Result<bool> {
    let mut written_this_batch = 0_usize;

    while written_this_batch < byte_budget {
        let write_result = {
            let Some(command) = outgoing.commands.front() else {
                return Ok(true);
            };

            let remaining_budget = byte_budget - written_this_batch;
            let remaining = &command.bytes[command.written..];

            writer.write(&remaining[..remaining.len().min(remaining_budget)])
        };

        match write_result {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "could not write a GDB/MI command",
                ));
            }
            Ok(count) => {
                outgoing.advance(count);
                written_this_batch += count;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }

    Ok(outgoing.is_empty())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum IoSource {
    Read,
    Write,
}

pub(super) struct MiTransport {
    pub(super) master: File,
    pub(super) _slave: Option<OwnedFd>,
    pub(super) slave_path: PathBuf,
}

pub(super) fn open_transport() -> io::Result<MiTransport> {
    let pty = openpty(None, None).map_err(io::Error::other)?;
    let slave_path = ttyname(&pty.slave).map_err(io::Error::other)?;
    let mut terminal_settings = tcgetattr(&pty.slave).map_err(io::Error::other)?;
    cfmakeraw(&mut terminal_settings);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &terminal_settings).map_err(io::Error::other)?;
    let master = File::from(pty.master);
    let flags = nix::fcntl::fcntl(&master, FcntlArg::F_GETFL).map_err(io::Error::other)?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(&master, FcntlArg::F_SETFL(flags)).map_err(io::Error::other)?;

    Ok(MiTransport {
        master,
        _slave: Some(pty.slave),
        slave_path,
    })
}

#[cfg(test)]
pub(super) fn test_transport() -> io::Result<(MiTransport, std::os::unix::net::UnixStream)> {
    use std::os::fd::OwnedFd;

    use std::os::unix::net::UnixStream;

    let (client, peer) = UnixStream::pair()?;
    client.set_nonblocking(true)?;
    peer.set_nonblocking(true)?;
    let master = File::from(OwnedFd::from(client));

    Ok((
        MiTransport {
            master,
            _slave: None,
            slave_path: PathBuf::from("<injected-mi-transport>"),
        },
        peer,
    ))
}

pub(super) fn complete_input_end(incoming: &[u8]) -> Option<usize> {
    incoming
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|end| end + 1)
}
