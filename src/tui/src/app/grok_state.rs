//! Read-only Grok idle evidence from an exact registered native event log.
//! Settled is an installer-reviewed 1.0.52+ contract marker, not an independent
//! runtime version check. This proof is not a hook event or terminal authority.
//! Native hooks register updates.jsonl; idle evidence uses its exact sibling
//! events.jsonl. Explicit events.jsonl registrations remain readable for legacy
//! proofs, but are not the native hook transcript contract.
use super::hook_runtime::Registration;
use std::path::Path;

#[cfg(not(unix))]
pub fn idle_proof(
    _registration: &Registration,
    _proc_root: &Path,
) -> Result<Option<String>, String> {
    Ok(None)
}
#[cfg(unix)]
pub fn idle_proof(registration: &Registration, proc_root: &Path) -> Result<Option<String>, String> {
    unix::proof(registration, proc_root, || {})
}

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::app::{
        hook_runtime,
        hooks::{EventKind, Harness},
    };
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File, Metadata, OpenOptions},
        io::{Read, Seek, SeekFrom},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    const LIMIT: u64 = 65536;
    fn stamp(meta: &Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
        (
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        )
    }
    fn private(path: &Path) -> Result<File, String> {
        crate::core::layouts::check_path(path)?;
        // Reject symlinks in every existing component, not merely the final file.
        let mut ancestor = std::path::PathBuf::new();
        for part in path.components() {
            ancestor.push(part);
            if fs::symlink_metadata(&ancestor)
                .map_err(|_| "Native state unavailable")?
                .file_type()
                .is_symlink()
            {
                return Err("Native state symlink forbidden".into());
            }
        }
        // Native providers may atomically recreate summary files with 0644 or
        // 0664. The owned 0700 session directory supplies the privacy boundary;
        // this exception is local to native evidence, never Daedalus state.
        let parent = path.parent().ok_or("Native session directory missing")?;
        let parent_before =
            fs::symlink_metadata(parent).map_err(|_| "Native session directory unavailable")?;
        if !parent_before.is_dir()
            || parent_before.uid() != unsafe { libc::geteuid() }
            || parent_before.mode() & 0o777 != 0o700
        {
            return Err("Native session directory must be owned and private".into());
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "Native state unavailable")?;
        let meta = file.metadata().map_err(|_| "Native state unavailable")?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || (meta.mode() & 0o077 != 0 && !matches!(meta.mode() & 0o777, 0o644 | 0o664))
            || meta.nlink() != 1
        {
            return Err("Native state must be an owned private regular file".into());
        }
        let parent_after =
            fs::symlink_metadata(parent).map_err(|_| "Native session directory unavailable")?;
        if !parent_after.is_dir()
            || parent_after.uid() != parent_before.uid()
            || parent_after.mode() & 0o777 != 0o700
            || parent_after.dev() != parent_before.dev()
            || parent_after.ino() != parent_before.ino()
        {
            return Err("Native session directory changed".into());
        }
        Ok(file)
    }
    fn fd_matches(proc_dir: &Path, meta: &Metadata) -> bool {
        let directory = proc_dir.join("fd");
        let Ok(info) = fs::symlink_metadata(&directory) else {
            return false;
        };
        if !info.is_dir() || info.uid() != unsafe { libc::geteuid() } {
            return false;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            return false;
        };
        let mut found = false;
        for (index, entry) in entries.enumerate() {
            if index >= 4096 {
                return false;
            }
            let Ok(entry) = entry else {
                return false;
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            // Following /proc/PID/fd/N is intentional: kernel-held inode evidence.
            if let Ok(open) = fs::metadata(entry.path())
                && open.is_file()
                && open.dev() == meta.dev()
                && open.ino() == meta.ino()
            {
                found = true;
            }
        }
        found
    }
    pub(super) fn proof(
        r: &Registration,
        proc_root: &Path,
        after_read: impl FnOnce(),
    ) -> Result<Option<String>, String> {
        if r.scope.harness != Harness::Grok || !r.events.contains(&EventKind::Settled) {
            return Ok(None);
        }
        let (Some(process), Some(transcript)) = (&r.harness_process, &r.transcript) else {
            return Ok(None);
        };
        let events_path = match transcript.file_name().and_then(|v| v.to_str()) {
            Some("updates.jsonl") => transcript.with_file_name("events.jsonl"),
            Some("events.jsonl") => transcript.clone(),
            _ => return Ok(None),
        };
        let path = events_path.as_path();
        if path
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            != Some(r.scope.conversation.as_str())
        {
            return Ok(None);
        }
        r.validate()
            .map_err(|_| "Invalid native proof registration")?;
        let proc_dir = proc_root.join(process.pid.to_string());
        let info = fs::symlink_metadata(&proc_dir).map_err(|_| "Native process unavailable")?;
        if !info.is_dir() || info.uid() != unsafe { libc::geteuid() } {
            return Ok(None);
        }
        hook_runtime::verify_process(process, proc_root)
            .map_err(|_| "Native process incarnation changed")?;
        let mut events = private(path)?;
        let before = events.metadata().map_err(|_| "Native log unavailable")?;
        if before.len() == 0 || !fd_matches(&proc_dir, &before) {
            return Ok(None);
        }
        let summary_path = path
            .parent()
            .ok_or("Native session directory missing")?
            .join("summary.json");
        let mut summary = private(&summary_path)?;
        let summary_before = summary
            .metadata()
            .map_err(|_| "Native summary unavailable")?;
        if summary_before.len() > LIMIT {
            return Ok(None);
        }
        let mut summary_bytes = Vec::new();
        (&mut summary)
            .take(LIMIT + 1)
            .read_to_end(&mut summary_bytes)
            .map_err(|_| "Native summary unavailable")?;
        let summary_value: serde_json::Value =
            serde_json::from_slice(&summary_bytes).map_err(|_| "Invalid native summary")?;
        if summary_value["info"]["id"].as_str() != Some(r.scope.conversation.as_str()) {
            return Ok(None);
        }
        let start = before.len().saturating_sub(LIMIT);
        events
            .seek(SeekFrom::Start(start))
            .map_err(|_| "Native log unavailable")?;
        let mut tail = Vec::new();
        (&mut events)
            .take(LIMIT + 1)
            .read_to_end(&mut tail)
            .map_err(|_| "Native log unavailable")?;
        if tail.len() > LIMIT as usize || tail.last() != Some(&b'\n') {
            return Ok(None);
        }
        let mut complete = &tail[..tail.len() - 1];
        if start > 0 {
            // The first tail fragment may begin inside a JSON record. Never
            // interpret it as lifecycle evidence, even if it happens to parse.
            let Some(boundary) = complete.iter().position(|b| *b == b'\n') else {
                return Ok(None);
            };
            complete = &complete[boundary + 1..];
        }
        let mut completed = None;
        let mut cursor = start + tail.len() as u64 - 1;
        for line in complete.rsplit(|b| *b == b'\n') {
            let offset = cursor - line.len() as u64;
            cursor = offset.saturating_sub(1);
            let record: serde_json::Value =
                serde_json::from_slice(line).map_err(|_| "Invalid final native event")?;
            if !record["ts"].as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
            }) {
                return Ok(None);
            }
            // Native hook reload emits health telemetry after a completed turn.
            // This exact event alone is benign; unknown or lifecycle events stop
            // the search and cannot uncover an older completion behind them.
            if record["type"] == "mcp_health_check" {
                continue;
            }
            if record["type"] == "turn_ended" && record["outcome"] == "completed" {
                completed = Some((offset, line));
            }
            break;
        }
        let Some((completed_offset, completed_record)) = completed else {
            return Ok(None);
        };
        after_read();
        let event_now = private(path)?
            .metadata()
            .map_err(|_| "Native log unavailable")?;
        let summary_now = private(&summary_path)?
            .metadata()
            .map_err(|_| "Native summary unavailable")?;
        if stamp(&before) != stamp(&event_now)
            || stamp(&summary_before) != stamp(&summary_now)
            || stamp(&before) != stamp(&events.metadata().map_err(|_| "Native log unavailable")?)
            || !fd_matches(&proc_dir, &event_now)
        {
            return Ok(None);
        }
        hook_runtime::verify_process(process, proc_root)
            .map_err(|_| "Native process incarnation changed")?;
        // The proof identifies the completed lifecycle boundary, not mutable
        // telemetry or summary timestamps. Those files must still be stable
        // throughout this read, and every trailing event was checked above.
        let encoded = serde_json::to_vec(&(
            "grok-native-idle-v2",
            r.id.as_str(),
            &r.scope,
            process,
            before.dev(),
            before.ino(),
            completed_offset,
            completed_record,
            summary_value["info"]["id"].as_str(),
        ))
        .map_err(|_| "Cannot encode native proof")?;
        Ok(Some(format!("{:x}", Sha256::digest(encoded))))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        app::hook_runtime::{self, Readiness},
        core::layouts,
    };
    use serde_json::json;
    use std::{
        fs,
        io::Write,
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
    };
    struct Fixture {
        root: PathBuf,
        proc: PathBuf,
        r: Registration,
    }
    impl Fixture {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("dd-grok-proof-{}-{unique}", std::process::id()));
            let transcript = root.join("native/session/events.jsonl");
            layouts::write_private(&transcript, b"{\"ts\":\"2026-10-01T00:00:00Z\",\"type\":\"turn_ended\",\"outcome\":\"completed\"}\n").unwrap();
            layouts::write_private(
                &transcript.with_file_name("summary.json"),
                br#"{"info":{"id":"session","cwd":"/fixture"}}"#,
            )
            .unwrap();
            let proc = root.join("proc");
            fs::create_dir_all(proc.join("42/fd")).unwrap();
            let mut fields = vec!["0"; 20];
            fields[0] = "S";
            fields[2] = "42";
            fields[5] = "42";
            fields[19] = "500";
            fs::write(
                proc.join("42/stat"),
                format!("42 (fixture) {}", fields.join(" ")),
            )
            .unwrap();
            symlink(&transcript, proc.join("42/fd/3")).unwrap();
            let r = serde_json::from_value(json!({"version":1,"id":"native-proof","directory":root.join("state"),"scope":{"instance":"fixture","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"harness":"grok","conversation":"session"},"goal_id":null,"harness_process":{"pid":42,"start_ticks":500},"transcript":transcript,"events":["Settled"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},"activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000})).unwrap();
            Self { root, proc, r }
        }
        fn install(&self) -> PathBuf {
            fs::create_dir(&self.r.directory).unwrap();
            fs::set_permissions(&self.r.directory, fs::Permissions::from_mode(0o700)).unwrap();
            hook_runtime::install(&self.r).unwrap()
        }
        fn log(&self, bytes: &[u8]) {
            layouts::write_private(self.r.transcript.as_ref().unwrap(), bytes).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn exact_completed_log_is_stable_and_read_only() {
        let f = Fixture::new();
        let first = idle_proof(&f.r, &f.proc).unwrap().unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(idle_proof(&f.r, &f.proc).unwrap(), Some(first));
        assert!(!f.r.directory.exists());
    }
    #[test]
    fn hook_updates_registration_uses_only_exact_sibling_native_events() {
        let f = Fixture::new();
        let expected = idle_proof(&f.r, &f.proc).unwrap().unwrap();
        let mut registration = f.r.clone();
        let history =
            f.r.transcript
                .as_ref()
                .unwrap()
                .with_file_name("updates.jsonl");
        layouts::write_private(&history, b"{\"text\":\"not lifecycle evidence\"}\n").unwrap();
        registration.transcript = Some(history);
        assert_eq!(idle_proof(&registration, &f.proc).unwrap(), Some(expected));
        registration.transcript = Some(f.root.join("native/other/updates.jsonl"));
        assert!(idle_proof(&registration, &f.proc).unwrap().is_none());
        registration.transcript = Some(
            f.r.transcript
                .as_ref()
                .unwrap()
                .with_file_name("unrecognized.jsonl"),
        );
        assert!(idle_proof(&registration, &f.proc).unwrap().is_none());
        registration.transcript = Some(
            f.r.transcript
                .as_ref()
                .unwrap()
                .with_file_name("chat_history.jsonl"),
        );
        assert!(idle_proof(&registration, &f.proc).unwrap().is_none());
        registration.transcript = Some(
            f.r.transcript
                .as_ref()
                .unwrap()
                .with_file_name("updates.jsonl"),
        );
        fs::remove_file(f.proc.join("42/fd/3")).unwrap();
        symlink(
            registration.transcript.as_ref().unwrap(),
            f.proc.join("42/fd/3"),
        )
        .unwrap();
        assert!(idle_proof(&registration, &f.proc).unwrap().is_none());
    }
    #[test]
    fn wrong_session_missing_fd_and_unsafe_files_are_rejected() {
        let f = Fixture::new();
        let mut wrong = f.r.clone();
        wrong.scope.conversation = "other".into();
        assert!(idle_proof(&wrong, &f.proc).unwrap().is_none());
        layouts::write_private(
            &f.r.transcript
                .as_ref()
                .unwrap()
                .with_file_name("summary.json"),
            br#"{"info":{"id":"other"}}"#,
        )
        .unwrap();
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_none());
        fs::remove_file(f.proc.join("42/fd/3")).unwrap();
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_none());
        symlink(f.r.transcript.as_ref().unwrap(), f.proc.join("42/fd/3")).unwrap();
        fs::set_permissions(
            f.r.transcript.as_ref().unwrap(),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::set_permissions(
            f.r.transcript.as_ref().unwrap().parent().unwrap(),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(idle_proof(&f.r, &f.proc).is_err());
    }
    #[test]
    fn native_summary_replacement_is_private_under_owned_session_directory() {
        let f = Fixture::new();
        let summary =
            f.r.transcript
                .as_ref()
                .unwrap()
                .with_file_name("summary.json");
        for mode in [0o644, 0o664] {
            let temporary = summary.with_file_name("summary-new.json");
            fs::write(&temporary, br#"{"info":{"id":"session","cwd":"/fixture"}}"#).unwrap();
            fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).unwrap();
            fs::rename(&temporary, &summary).unwrap();
            assert!(idle_proof(&f.r, &f.proc).unwrap().is_some());
            assert_eq!(
                std::os::unix::fs::MetadataExt::mode(&fs::metadata(&summary).unwrap()) & 0o777,
                mode
            );
        }
    }
    #[test]
    fn active_partial_and_unreviewed_outcomes_do_not_claim_idle() {
        let f = Fixture::new();
        for bytes in [
            b"{\"ts\":\"now\",\"type\":\"turn_ended\",\"outcome\":\"completed\"}".as_slice(),
            b"{\"ts\":\"now\",\"type\":\"turn_started\"}\n",
            b"{\"ts\":\"now\",\"type\":\"turn_ended\",\"outcome\":\"cancelled\"}\n",
            b"\n",
        ] {
            f.log(bytes);
            assert!(!matches!(idle_proof(&f.r, &f.proc), Ok(Some(_))));
        }
    }
    #[test]
    fn only_explicit_health_telemetry_can_follow_completed_turn() {
        let f = Fixture::new();
        let completed = b"{\"ts\":\"done\",\"type\":\"turn_ended\",\"outcome\":\"completed\"}\n";
        let health = b"{\"ts\":\"later\",\"type\":\"mcp_health_check\"}\n";
        let mut bytes = completed.to_vec();
        for _ in 0..16 {
            bytes.extend_from_slice(health);
        }
        f.log(&bytes);
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_some());
        for kind in [
            "unknown",
            "phase_changed",
            "turn_started",
            "permission_requested",
            "mcp_health_check_other",
        ] {
            let mut bytes = completed.to_vec();
            bytes.extend_from_slice(
                format!("{{\"ts\":\"later\",\"type\":\"{kind}\"}}\n").as_bytes(),
            );
            bytes.extend_from_slice(health);
            f.log(&bytes);
            assert!(idle_proof(&f.r, &f.proc).unwrap().is_none(), "{kind}");
        }
        f.log(health);
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_none());
        let mut partial = completed.to_vec();
        partial.extend_from_slice(&health[..health.len() - 1]);
        f.log(&partial);
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_none());
    }
    #[test]
    fn proof_ignores_health_and_summary_rewrites_but_tracks_lifecycle_boundary() {
        let f = Fixture::new();
        let original = idle_proof(&f.r, &f.proc).unwrap().unwrap();
        let path = f.r.transcript.as_ref().unwrap();
        let mut events = fs::OpenOptions::new().append(true).open(path).unwrap();
        events
            .write_all(b"{\"ts\":\"later\",\"type\":\"mcp_health_check\"}\n")
            .unwrap();
        let summary = path.with_file_name("summary.json");
        let replacement = path.with_file_name("summary-new.json");
        fs::write(
            &replacement,
            br#"{"info":{"id":"session","updated":"later"},"telemetry":17}"#,
        )
        .unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o644)).unwrap();
        fs::rename(&replacement, &summary).unwrap();
        assert_eq!(idle_proof(&f.r, &f.proc).unwrap(), Some(original.clone()));
        events
            .write_all(b"{\"ts\":\"later\",\"type\":\"turn_started\"}\n")
            .unwrap();
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_none());
        events.write_all(b"{\"ts\":\"2026-10-01T00:00:00Z\",\"type\":\"turn_ended\",\"outcome\":\"completed\"}\n").unwrap();
        // Identical completion bytes at a new offset represent another boundary.
        assert_ne!(idle_proof(&f.r, &f.proc).unwrap(), Some(original));
    }
    #[test]
    fn replaced_event_inode_without_matching_open_descriptor_is_rejected() {
        let f = Fixture::new();
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_some());
        let path = f.r.transcript.as_ref().unwrap();
        let original = path.with_file_name("original-events.jsonl");
        fs::rename(path, &original).unwrap();
        fs::remove_file(f.proc.join("42/fd/3")).unwrap();
        symlink(&original, f.proc.join("42/fd/3")).unwrap();
        layouts::write_private(path, &fs::read(&original).unwrap()).unwrap();
        assert!(idle_proof(&f.r, &f.proc).unwrap().is_none());
    }
    #[test]
    fn log_changes_during_read_and_recycled_process_fail_closed() {
        let f = Fixture::new();
        let result = super::unix::proof(&f.r, &f.proc, || {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(f.r.transcript.as_ref().unwrap())
                .unwrap();
            file.write_all(b"{\"ts\":\"later\",\"type\":\"turn_started\"}\n")
                .unwrap();
        })
        .unwrap();
        assert!(result.is_none());
        let f = Fixture::new();
        let result = super::unix::proof(&f.r, &f.proc, || {
            fs::write(f.proc.join("42/stat"), "42 (gone) X 0").unwrap();
        });
        assert!(result.is_err());
    }
    #[test]
    fn advanced_native_log_invalidates_bootstrap_and_reservation() {
        let f = Fixture::new();
        let registration = f.install();
        let ready = hook_runtime::ready_snapshot(&registration, &f.proc).unwrap();
        assert_eq!(ready.state, Readiness::Idle);
        f.log(b"{\"ts\":\"later\",\"type\":\"turn_started\"}\n");
        assert_eq!(
            hook_runtime::ready_snapshot(&registration, &f.proc)
                .unwrap()
                .state,
            Readiness::Unknown
        );
        assert!(!hook_runtime::reserve_ready(&registration, &f.proc, ready.sequence).unwrap());
        assert!(
            !hook_runtime::consume_ready_for_control(&registration, &f.proc, ready.sequence)
                .unwrap()
        );
    }
    #[test]
    fn outstanding_reservation_blocks_initial_native_bootstrap() {
        let f = Fixture::new();
        let registration = f.install();
        fs::remove_file(f.proc.join("42/fd/3")).unwrap();
        assert_eq!(
            hook_runtime::ready_snapshot(&registration, &f.proc)
                .unwrap()
                .state,
            Readiness::Unknown
        );
        let con =
            rusqlite::Connection::open(f.r.directory.join("hook-observations.sqlite3")).unwrap();
        con.execute(
            "INSERT INTO hook_reservations(scope,correlation) VALUES(?,'pending')",
            [serde_json::to_string(&f.r.scope).unwrap()],
        )
        .unwrap();
        symlink(f.r.transcript.as_ref().unwrap(), f.proc.join("42/fd/3")).unwrap();
        assert_eq!(
            hook_runtime::ready_snapshot(&registration, &f.proc)
                .unwrap()
                .state,
            Readiness::Unknown
        );
        let count: i64 = con
            .query_row("SELECT COUNT(*) FROM hook_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn bootstrap_is_distinct_and_never_overrides_existing_busy() {
        let f = Fixture::new();
        let registration = f.install();
        let ready = hook_runtime::ready_snapshot(&registration, &f.proc).unwrap();
        assert_eq!(ready.state, Readiness::Idle);
        let con =
            rusqlite::Connection::open(f.r.directory.join("hook-observations.sqlite3")).unwrap();
        let event: String = con
            .query_row("SELECT event_id FROM hook_observations", [], |r| r.get(0))
            .unwrap();
        assert!(event.starts_with("native-grok:"));
        con.execute("INSERT INTO hook_observations(scope,event_id,payload,state,observed_at,finalized) VALUES(?,'fixture-busy','fixture','\"busy\"',1,1)",[serde_json::to_string(&f.r.scope).unwrap()]).unwrap();
        assert_eq!(
            hook_runtime::ready_snapshot(&registration, &f.proc)
                .unwrap()
                .state,
            Readiness::Busy
        );
    }
}
