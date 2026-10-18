//! Start suspended so every descendant belongs to our kill-on-close Job before
//! user code executes. Pipe reads use PeekNamedPipe and bounded work per tick.
use std::{
    io::{self, Read, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle},
        process::CommandExt,
    },
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Diagnostics::ToolHelp::*, JobObjects::*, Pipes::PeekNamedPipe, Threading::*},
};
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
struct Job(HANDLE);
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Owned {
    child: Child,
    job: Job,
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            TerminateJobObject(self.job.0, 1);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn error() -> io::Error {
    io::Error::last_os_error()
}
fn create_job(allow_breakaway: bool) -> io::Result<Job> {
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return Err(error());
        }
        let job = Job(handle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | if allow_breakaway {
                JOB_OBJECT_LIMIT_BREAKAWAY_OK
            } else {
                0
            };
        if SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
        {
            return Err(error());
        }
        Ok(job)
    }
}
fn resume(pid: u32) -> io::Result<()> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(error());
        }
        let snapshot = std::fs::File::from_raw_handle(snapshot);
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut more = Thread32First(snapshot.as_raw_handle(), &mut entry);
        while more != 0 {
            if entry.th32OwnerProcessID == pid {
                let handle = OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                );
                if handle.is_null() {
                    return Err(error());
                }
                let thread = std::fs::File::from_raw_handle(handle);
                if GetProcessIdOfThread(thread.as_raw_handle()) != pid {
                    return Err(io::Error::other("Owned query thread identity changed"));
                }
                if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                    return Err(error());
                }
                return Ok(());
            }
            more = Thread32Next(snapshot.as_raw_handle(), &mut entry);
        }
        Err(io::Error::other("Owned query thread unavailable"))
    }
}
fn drain(
    stream: &mut (impl Read + AsRawHandle),
    bytes: &mut Vec<u8>,
    remaining: &mut usize,
    empty_is_drained: bool,
) -> io::Result<bool> {
    for _ in 0..8 {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                stream.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let e = error();
            if matches!(e.raw_os_error(), Some(x) if x == ERROR_BROKEN_PIPE as i32 || x == ERROR_PIPE_NOT_CONNECTED as i32)
            {
                return Ok(true);
            }
            return Err(e);
        }
        if available == 0 {
            return Ok(empty_is_drained);
        }
        let mut buffer = [0; 8192];
        let count = stream.read(&mut buffer[..(available as usize).min(8192)])?;
        if count == 0 {
            return Ok(true);
        }
        if count > *remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Adapter output exceeds limit",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
        *remaining -= count;
    }
    Ok(false)
}
pub fn run(command: &mut Command, timeout: Duration, max_bytes: usize) -> io::Result<Output> {
    run_with_input(command, timeout, max_bytes, &[])
}
/// Trusted mux queries may explicitly create persistent server processes using
/// CREATE_BREAKAWAY_FROM_JOB. Ordinary descendants remain owned and bounded.
pub fn run_terminal(
    command: &mut Command,
    timeout: Duration,
    max_bytes: usize,
) -> io::Result<Output> {
    run_inner(command, timeout, max_bytes, &[], true)
}
pub fn run_with_input(
    command: &mut Command,
    timeout: Duration,
    max_bytes: usize,
    input: &[u8],
) -> io::Result<Output> {
    run_inner(command, timeout, max_bytes, input, false)
}
fn run_inner(
    command: &mut Command,
    timeout: Duration,
    max_bytes: usize,
    input: &[u8],
    allow_breakaway: bool,
) -> io::Result<Output> {
    if input.len() > 65536 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Adapter input exceeds limit",
        ));
    }
    let start = Instant::now();
    let job = create_job(allow_breakaway)?;
    command
        .stdin(if input.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut owned = Owned {
        child: command.spawn()?,
        job,
    };
    if unsafe { AssignProcessToJobObject(owned.job.0, owned.child.as_raw_handle()) } == 0 {
        return Err(error());
    }
    resume(owned.child.id())?;
    let mut out = owned
        .child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing adapter stdout"))?;
    let mut err = owned
        .child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing adapter stderr"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let writer = owned.child.stdin.take().map(|mut stdin| {
        let input = input.to_vec();
        thread::spawn(move || {
            let result = stdin.write_all(&input);
            drop(stdin);
            let _ = sender.send(result);
        })
    });
    let mut written = writer.is_none();
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut remaining = max_bytes;
    let result = (|| {
        loop {
            if start.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Adapter query timed out",
                ));
            }
            if !written {
                match receiver.try_recv() {
                    Ok(Ok(())) => written = true,
                    Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Adapter input was not accepted",
                        ));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            // Observe exit before draining to include all output from the query.
            // An explicitly detached server may retain a pipe handle indefinitely.
            let status = owned.child.try_wait()?;
            let done = drain(&mut out, &mut output, &mut remaining, allow_breakaway)?
                & drain(&mut err, &mut errors, &mut remaining, allow_breakaway)?;
            if done
                && written
                && let Some(status) = status
            {
                return Ok(Output {
                    status,
                    stdout: output,
                    stderr: errors,
                });
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    // Killing the owned job closes every inherited pipe before joining stdin.
    drop(owned);
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn command(script: &str) -> Command {
        let mut c = Command::new("cmd.exe");
        c.args(["/D", "/C", script]);
        c
    }
    #[test]
    fn bounded_capture_records_exit_status() {
        let out = run(
            &mut command("echo output& echo error 1>&2& exit /b 7"),
            Duration::from_secs(5),
            4096,
        )
        .unwrap();
        assert_eq!(out.status.code(), Some(7));
        assert!(String::from_utf8_lossy(&out.stdout).contains("output"));
        assert!(String::from_utf8_lossy(&out.stderr).contains("error"));
    }
    #[test]
    fn output_limit_terminates_owned_job() {
        let result = run(
            &mut command("for /L %i in (1,1,100000) do @echo 1234567890"),
            Duration::from_secs(5),
            20,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidData);
    }
    #[test]
    fn timeout_terminates_owned_job_without_waiting_for_pipe_eof() {
        let start = Instant::now();
        let result = run(
            &mut command("ping -n 20 127.0.0.1 >nul"),
            Duration::from_millis(100),
            100,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn breakaway_fixture() {
        if std::env::var_os("DAEDALUS_TEST_BREAKAWAY").is_none() {
            return;
        }
        let child = Command::new("ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NO_WINDOW)
            .spawn();
        match child {
            Ok(child) => println!("owned-fixture-pid:{}", child.id()),
            Err(_) => println!("breakaway-denied"),
        }
    }
    #[test]
    fn only_terminal_runner_allows_explicit_server_breakaway() {
        fn fixture_command() -> Command {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "adapters::process::windows::tests::breakaway_fixture",
                    "--nocapture",
                ])
                .env("DAEDALUS_TEST_BREAKAWAY", "1");
            command
        }
        let strict = run(&mut fixture_command(), Duration::from_secs(10), 8192).unwrap();
        assert!(String::from_utf8_lossy(&strict.stdout).contains("breakaway-denied"));
        let terminal = run_terminal(&mut fixture_command(), Duration::from_secs(10), 8192).unwrap();
        let text = String::from_utf8(terminal.stdout).unwrap();
        let pid: u32 = text
            .lines()
            .find_map(|line| line.strip_prefix("owned-fixture-pid:"))
            .expect("explicit breakaway child")
            .parse()
            .unwrap();
        // Reap only the exact fixture child that this invocation created.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
            assert!(!handle.is_null());
            let live = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
            TerminateProcess(handle, 0);
            WaitForSingleObject(handle, 5000);
            CloseHandle(handle);
            assert!(live, "explicit terminal server must outlive query Job");
        }
    }
    #[test]
    fn large_private_input_cannot_block_deadline() {
        let start = Instant::now();
        let result = run_with_input(
            &mut command("ping -n 20 127.0.0.1 >nul"),
            Duration::from_millis(100),
            100,
            &[42; 65536],
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
