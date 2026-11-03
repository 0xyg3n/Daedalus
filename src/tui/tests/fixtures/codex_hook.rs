//! Inert native process fixture; never invokes Codex or a model provider.
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn wait(path: &str) {
    let started = Instant::now();
    while !Path::new(path).exists() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "fixture timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let index = args.iter().position(|arg| arg == "--fixture-hook").unwrap();
    let fields = &args[index + 1..];
    assert_eq!(fields.len(), 7);
    // The parent writes terminal observations before permitting the hook.
    wait(&fields[0]);
    let mut child = Command::new(&fields[1])
        .args([
            "hook",
            "run",
            "--registration",
            &fields[2],
            "--event-kind",
            "PostToolUse",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&fs::read(&fields[3]).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    fs::write(&fields[5], output.stderr).unwrap();
    let mut receipt = format!("{}\n", output.status.code().unwrap_or(-1)).into_bytes();
    receipt.extend(output.stdout);
    fs::write(&fields[4], receipt).unwrap();
    // Keep the exact process incarnation alive for the parent's assertions.
    wait(&fields[6]);
}
