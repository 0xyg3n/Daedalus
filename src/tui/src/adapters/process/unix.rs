//! Bounded, owned subprocesses for short-lived adapter queries.
//! Never use this runner to own an attached terminal or a persistent agent.
use std::{
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

struct OwnedQuery(Child, bool);
impl Drop for OwnedQuery {
    fn drop(&mut self) {
        if self.1 {
            return;
        }
        // process_group(0) makes this group ours, never the caller's terminal.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn nonblocking(stream: &impl AsRawFd) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn drain(stream: &mut impl Read, bytes: &mut Vec<u8>, remaining: &mut usize) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    // Bounded work per tick so continuous output cannot bypass the deadline.
    for _ in 0..8 {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if count > *remaining {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Adapter output exceeds limit",
                    ));
                }
                bytes.extend_from_slice(&buffer[..count]);
                *remaining -= count;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// ETXTBSY means exec did not start. Retry only that pre-execution race,
/// within both the caller's timeout and a 50 ms ceiling. Never retry a child.
fn spawn_with_busy_retry<T>(
    mut spawn: impl FnMut() -> io::Result<T>,
    budget: Duration,
) -> io::Result<T> {
    let start = Instant::now();
    let budget = budget.min(Duration::from_millis(50));
    loop {
        match spawn() {
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY) && start.elapsed() < budget =>
            {
                let remaining = budget.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    return Err(error);
                }
                thread::sleep(remaining.min(Duration::from_millis(5)));
                if start.elapsed() >= budget {
                    return Err(error);
                }
            }
            outcome => return outcome,
        }
    }
}

/// The caller supplies an executable and argument vector, never shell text.
/// Errors intentionally omit argv and output, which can contain private data.
pub fn run(command: &mut Command, timeout: Duration, max_bytes: usize) -> io::Result<Output> {
    run_with_input(command, timeout, max_bytes, &[])
}
/// Bounded private stdin, concurrently drained with both output streams. Input
/// is never placed in argv/environment and error messages never contain it.
pub fn run_with_input(
    command: &mut Command,
    timeout: Duration,
    max_bytes: usize,
    input: &[u8],
) -> io::Result<Output> {
    if input.len() > 64 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Adapter input exceeds limit",
        ));
    }
    let start = Instant::now();
    command
        .stdin(if input.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = OwnedQuery(
        spawn_with_busy_retry(|| command.spawn(), timeout.saturating_sub(start.elapsed()))?,
        false,
    );
    let mut stdin = child.0.stdin.take();
    if let Some(stream) = &stdin {
        nonblocking(stream)?;
    }
    let mut sent = 0;
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing adapter stdout"))?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing adapter stderr"))?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut remaining = max_bytes;
    loop {
        if start.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Adapter query timed out",
            ));
        }
        if let Some(stream) = &mut stdin {
            match stream.write(&input[sent..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "Adapter input was not accepted",
                    ));
                }
                Ok(count) => sent += count,
                Err(error)
                    if [io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted]
                        .contains(&error.kind()) => {}
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Adapter input was not accepted",
                    ));
                }
            }
            if sent == input.len() {
                stdin = None;
            }
        }
        let out_done = drain(&mut stdout, &mut output, &mut remaining)?;
        let err_done = drain(&mut stderr, &mut errors, &mut remaining)?;
        // Inspect without reaping: retaining the PID prevents process-group ID
        // reuse between checking exit and killing any leftover descendants.
        if out_done && err_done {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.0.id(),
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { info.assume_init().si_pid() } != 0 {
                unsafe {
                    libc::kill(-(child.0.id() as i32), libc::SIGKILL);
                }
                let status = child.0.wait()?;
                // Disarm Drop before PID reuse is possible.
                child.1 = true;
                return Ok(Output {
                    status,
                    stdout: output,
                    stderr: errors,
                });
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collects_both_streams_and_exit_status() {
        let output = run(
            Command::new("sh").args(["-c", "printf output; printf error >&2; exit 7"]),
            Duration::from_secs(2),
            100,
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"output");
        assert_eq!(output.stderr, b"error");
    }
    #[test]
    fn enforces_combined_output_bound() {
        let result = run(
            Command::new("sh").args(["-c", "while :; do printf 1234567890; done"]),
            Duration::from_secs(2),
            20,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
    }
    #[test]
    fn descendant_holding_pipe_cannot_hang_runner() {
        let start = Instant::now();
        let result = run(
            Command::new("sh").args(["-c", "sleep 10 & exit 0"]),
            Duration::from_millis(50),
            100,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_terminates_owned_descendant() {
        let directory =
            std::env::temp_dir().join(format!("daedalus-query-child-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let pid_file = directory.join("child.pid");
        let result = run(
            Command::new("sh")
                .args([
                    "-c",
                    "sleep 10 & printf '%s' \"$!\" > \"$1\"; wait",
                    "query-test",
                ])
                .arg(&pid_file),
            Duration::from_millis(150),
            100,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            if stat.is_err()
                || stat.as_ref().is_ok_and(|s| {
                    s.rsplit_once(')').is_some_and(|(_, fields)| {
                        matches!(fields.split_whitespace().next(), Some("Z" | "X"))
                    })
                })
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Owned descendant survived query timeout"
            );
            thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn spawn_retry_is_limited_to_not_started_text_busy() {
        let mut calls = 0;
        let result = spawn_with_busy_retry(
            || {
                calls += 1;
                if calls < 3 {
                    Err(io::Error::from_raw_os_error(libc::ETXTBSY))
                } else {
                    Ok(42)
                }
            },
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(result, 42);
        assert_eq!(calls, 3);
        let mut calls = 0;
        let result: io::Result<()> = spawn_with_busy_retry(
            || {
                calls += 1;
                Err(io::Error::from_raw_os_error(libc::EACCES))
            },
            Duration::from_secs(1),
        );
        assert!(result.is_err());
        assert_eq!(calls, 1);
        let mut calls = 0;
        let result: io::Result<()> = spawn_with_busy_retry(
            || {
                calls += 1;
                Err(io::Error::from_raw_os_error(libc::ETXTBSY))
            },
            Duration::ZERO,
        );
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
    #[test]
    fn private_input_is_concurrent_bounded_and_closed() {
        let output = run_with_input(
            Command::new("sh").args(["-c", "printf ready; cat; printf done >&2"]),
            Duration::from_secs(2),
            100,
            b"secret-input",
        )
        .unwrap();
        assert_eq!(output.stdout, b"readysecret-input");
        assert_eq!(output.stderr, b"done");
        let error = run_with_input(
            Command::new("sh").args(["-c", "sleep 10"]),
            Duration::from_millis(50),
            100,
            &vec![42; 64 * 1024],
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            run_with_input(
                &mut Command::new("does-not-exist"),
                Duration::from_secs(1),
                100,
                &vec![0; 65537]
            )
            .is_err()
        );
    }
}
