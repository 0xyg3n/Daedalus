//! Bootstrap existing Codex sessions from their still-open native rollout.
//! This is local lifecycle evidence, never a synthetic hook or delivery receipt.
use super::hook_runtime::Registration;
use std::path::Path;

#[cfg(not(target_os = "linux"))]
pub fn idle_proof(_: &Registration, _: &Path) -> Result<Option<String>, String> {
    Ok(None)
}

#[cfg(target_os = "linux")]
pub fn idle_proof(r: &Registration, proc_root: &Path) -> Result<Option<String>, String> {
    linux::proof(r, proc_root)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::app::{
        hook_runtime,
        hooks::{EventKind, Harness},
    };
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File, Metadata, OpenOptions},
        io::{Read, Seek, SeekFrom},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    const LIMIT: u64 = 262144;
    fn stamp(m: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    }
    fn open(path: &Path) -> Result<File, String> {
        crate::core::layouts::check_path(path)?;
        let mut private_ancestor = false;
        let mut part = std::path::PathBuf::new();
        for c in path.components() {
            part.push(c);
            let m = fs::symlink_metadata(&part).map_err(|_| "Native rollout unavailable")?;
            if m.file_type().is_symlink() {
                return Err("Native rollout symlink forbidden".into());
            }
            if m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0 {
                private_ancestor = true;
            }
        }
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "Native rollout unavailable")?;
        let m = f.metadata().map_err(|_| "Native rollout unavailable")?;
        if !m.is_file()
            || m.uid() != unsafe { libc::geteuid() }
            || m.nlink() != 1
            || m.mode() & 0o002 != 0
            || (!private_ancestor && m.mode() & 0o077 != 0)
        {
            return Err("Native rollout must be owned and private".into());
        }
        Ok(f)
    }
    // Only known trailing metadata is ignorable. Any newer prompt, tool call,
    // unknown lifecycle, partial write or malformed record keeps input blocked.
    fn completed(tail: &[u8], clipped: bool) -> Option<String> {
        if !tail.ends_with(b"\n") {
            return None;
        }
        let mut rows = tail.split(|b| *b == b'\n').collect::<Vec<_>>();
        rows.pop();
        if clipped && !rows.is_empty() {
            rows.remove(0);
        }
        for row in rows.into_iter().rev() {
            let v: Value = serde_json::from_slice(row).ok()?;
            match (v["type"].as_str()?, v["payload"]["type"].as_str()) {
                ("token_usage_record", _)
                | ("event_msg", Some("token_count" | "thread_settings_applied")) => continue,
                ("event_msg", Some("task_complete")) => {
                    return v["payload"]["turn_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                }
                _ => return None,
            }
        }
        None
    }
    pub(super) fn proof(r: &Registration, proc_root: &Path) -> Result<Option<String>, String> {
        if r.scope.harness != Harness::Codex
            || !r.events.contains(&EventKind::UserPromptSubmit)
            || !r.events.contains(&EventKind::Stop)
        {
            return Ok(None);
        }
        let Some(process) = &r.harness_process else {
            return Ok(None);
        };
        hook_runtime::verify_process(process, proc_root).map_err(|_| "Native process changed")?;
        let fd_dir = proc_root.join(process.pid.to_string()).join("fd");
        let entries = fs::read_dir(&fd_dir).map_err(|_| "Native open files unavailable")?;
        let mut found = None;
        for (index, entry) in entries.enumerate() {
            if index >= 4096 {
                return Ok(None);
            }
            let entry = entry.map_err(|_| "Native open files unavailable")?;
            if !entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
            {
                continue;
            }
            let Ok(path) = fs::read_link(entry.path()) else {
                continue;
            };
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if r.transcript
                .as_ref()
                .is_some_and(|expected| *expected != path)
            {
                continue;
            }
            let Ok(mut f) = open(&path) else { continue };
            let before = f.metadata().map_err(|_| "Native rollout unavailable")?;
            let Ok(fd) = fs::metadata(entry.path()) else {
                continue;
            };
            if (before.dev(), before.ino()) != (fd.dev(), fd.ino()) {
                continue;
            }
            let mut head = Vec::new();
            (&mut f)
                .take(LIMIT)
                .read_to_end(&mut head)
                .map_err(|_| "Native rollout unavailable")?;
            let Some(line) = head.split(|b| *b == b'\n').next() else {
                continue;
            };
            let Ok(header) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            if header["type"] != "session_meta"
                || header["payload"]["id"].as_str() != Some(r.scope.conversation.as_str())
            {
                continue;
            }
            // Two different open rollouts for the same session are ambiguous.
            if found.is_some() {
                return Ok(None);
            }
            let start = before.len().saturating_sub(LIMIT);
            f.seek(SeekFrom::Start(start))
                .map_err(|_| "Native rollout unavailable")?;
            let mut tail = Vec::new();
            (&mut f)
                .take(LIMIT + 1)
                .read_to_end(&mut tail)
                .map_err(|_| "Native rollout unavailable")?;
            let Some(turn) = completed(&tail, start > 0) else {
                return Ok(None);
            };
            let current = open(&path)?
                .metadata()
                .map_err(|_| "Native rollout unavailable")?;
            let held = fs::metadata(entry.path()).map_err(|_| "Native rollout closed")?;
            if stamp(&before) != stamp(&current)
                || stamp(&before) != stamp(&f.metadata().map_err(|_| "Native rollout unavailable")?)
                || (held.dev(), held.ino()) != (before.dev(), before.ino())
            {
                return Ok(None);
            }
            found = Some(format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&(process, &r.scope, stamp(&before), turn))
                        .map_err(|_| "Invalid native proof")?
                )
            ));
        }
        hook_runtime::verify_process(process, proc_root).map_err(|_| "Native process changed")?;
        Ok(found)
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn only_complete_native_turn_without_new_input_is_idle() {
            let done=b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"turn-1\"}}\n";
            assert_eq!(completed(done, false), Some("turn-1".into()));
            for after in [
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"unknown\"}}\n",
                "{",
                "bad\n",
            ] {
                assert!(completed(&[done.as_slice(), after.as_bytes()].concat(), false).is_none());
            }
            assert_eq!(completed(&[done.as_slice(),b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\"}}\n"].concat(),false),Some("turn-1".into()));
            assert!(completed(&done[..done.len() - 1], false).is_none());
            assert!(completed(done, true).is_none());
        }
    }
}
