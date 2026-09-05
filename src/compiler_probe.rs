//! Bounded, deadline-aware output collection for optional compiler discovery.
use std::{
    io::{self, Read},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::fcntl::{FcntlArg, OFlag, fcntl};

pub(crate) fn output(command: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    const LIMIT: usize = 64 * 1024;
    let deadline = Instant::now().checked_add(timeout)?;

    if timeout.is_zero() {
        return None;
    }

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let result = (|| {
        let mut stdout = child.stdout.take()?;
        let flags = fcntl(&stdout, FcntlArg::F_GETFL).ok()?;
        fcntl(
            &stdout,
            FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
        )
        .ok()?;
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        let mut eof = false;
        let mut status = None;

        loop {
            if Instant::now() >= deadline {
                return None;
            }

            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(count) => {
                        if output.len().saturating_add(count) > LIMIT {
                            return None;
                        }

                        output.extend_from_slice(&buffer[..count]);
                        continue;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => return None,
                }
            }

            if status.is_none() {
                status = child.try_wait().ok()?;
            }

            if let Some(status) = status {
                if !status.success() {
                    return None;
                }

                if eof {
                    return Some(output);
                }
            }

            // A descendant may retain stdout after the compiler exits. Never
            // switch to a blocking read or wait_with_output in that case.
            thread::sleep(
                Duration::from_millis(2).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    })();

    // Reap our child on every error path, without waiting for descendants to
    // close inherited pipe handles. The read end has already been dropped.
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drains_output_larger_than_a_pipe_before_waiting() {
        let bytes = output(
            Command::new("sh").args(["-c", "head -c 65536 /dev/zero"]),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(bytes.len(), 65536);
    }

    #[test]
    fn bounds_output_and_rejects_failed_probes() {
        assert!(
            output(
                Command::new("sh").args(["-c", "head -c 65537 /dev/zero"]),
                Duration::from_secs(2)
            )
            .is_none()
        );
        assert!(
            output(
                Command::new("sh").args(["-c", "printf partial; exit 1"]),
                Duration::from_secs(2)
            )
            .is_none()
        );
    }

    #[test]
    fn inherited_stdout_does_not_escape_the_deadline() {
        let started = Instant::now();
        assert!(
            output(
                Command::new("sh").args(["-c", "sleep 2 & printf partial"]),
                Duration::from_millis(50)
            )
            .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
