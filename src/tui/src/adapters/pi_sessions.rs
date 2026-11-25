//! Native Pi session headers, scoped to an activated Daedalus launch plan.
//! UUIDs and transcript paths stay distinct. History bodies are never read.
use crate::{
    app::{hook_launch::Plan, hook_runtime::Registration, hooks::Harness},
    core::{identity::TerminalId, layouts, native_metadata::Guard},
};
use serde_json::Value;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
    registration: PathBuf,
    registration_bytes: Vec<u8>,
    plan: PathBuf,
    plan_bytes: Vec<u8>,
    header: Vec<u8>,
    device: u64,
    inode: u64,
}
fn bytes(path: &Path, guard: &mut Guard<'_>) -> Result<Vec<u8>> {
    guard
        .read(path)?
        .ok_or_else(|| "Pi launch evidence is missing".into())
}
fn header(path: &Path, guard: &mut Guard<'_>) -> Result<(Vec<u8>, u64, u64)> {
    guard.check()?;
    layouts::check_path(path)?;
    #[cfg(unix)]
    let (file, device, inode) = {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "Cannot open Pi session header")?;
        let metadata = file.metadata().map_err(|_| "Cannot inspect Pi session")?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err("Pi session must be a private regular file".into());
        }
        let device = metadata.dev();
        let inode = metadata.ino();
        (file, device, inode)
    };
    #[cfg(windows)]
    let (file, device, inode) = {
        let file = crate::platform::open_private(path, false, false)
            .map_err(|_| "Cannot open private Pi session")?;
        let (device, inode) =
            crate::platform::file_identity(&file).map_err(|_| "Cannot identify Pi session")?;
        (file, device, inode)
    };
    let line = guard.header(file)?;
    Ok((line, device, inode))
}
/// Metadata proof for an explicitly selected Pi fork file. No directory search,
/// filename ID inference or transcript parsing is involved.
pub struct Parent {
    pub id: String,
    path: PathBuf,
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
}
impl Parent {
    pub fn verify(&self, guard: &mut Guard<'_>) -> Result<()> {
        let (bytes, device, inode) = header(&self.path, guard)?;
        if bytes != self.bytes || (device, inode) != (self.device, self.inode) {
            return Err("Pi fork parent header changed; refresh".into());
        }
        Ok(())
    }
}
pub fn fork_parent(argv: &[String], cwd: &Path, guard: &mut Guard<'_>) -> Result<Option<Parent>> {
    guard.check()?;
    let Some(selector) = crate::core::checkpoints::pi_fork_selector(argv)? else {
        return Ok(None);
    };
    let path = Path::new(&selector);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    layouts::check_path(&path)?;
    if !path.exists()
        && !Path::new(&selector).is_absolute()
        && Path::new(&selector).components().count() == 1
    {
        return Ok(None);
    }
    let (bytes, device, inode) = header(&path, guard)?;
    guard.json(&bytes)?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Malformed Pi fork parent header")?;
    if value["type"] != "session" || value["version"] != 3 {
        return Err("Unsupported Pi fork parent header".into());
    }
    let id = value["id"].as_str().ok_or("Missing Pi fork parent ID")?;
    let declared = ["pi".into(), "--session-id".into(), id.into()];
    crate::core::checkpoints::resolve_sid(&declared, "pi", "")?
        .ok_or("Invalid Pi fork parent ID")?;
    Ok(Some(Parent {
        id: id.into(),
        path,
        bytes,
        device,
        inode,
    }))
}

struct Evidence {
    plan_path: PathBuf,
    plan_bytes: Vec<u8>,
    plan: Plan,
    reg_path: PathBuf,
    reg_bytes: Vec<u8>,
}
fn planned(directory: &Path, id: &str, guard: &mut Guard<'_>) -> Result<Vec<Evidence>> {
    let root = directory.join("hook-launch");
    layouts::check_path(&root)?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(_) => return Err("Cannot inspect Pi launch plans".into()),
    };
    let mut found = Vec::new();
    for entry in entries {
        guard.entries(1)?;
        let path = entry.map_err(|_| "Cannot inspect launch entry")?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let raw = bytes(&path, guard)?;
        guard.json(&raw)?;
        let plan: Plan = serde_json::from_slice(&raw).map_err(|_| "Invalid saved launch plan")?;
        if plan.spec.registration.scope.harness != Harness::Pi
            || plan.spec.registration.scope.conversation != id
        {
            continue;
        }
        if plan.path() != path
            || plan.spec.registration.directory != directory
            || bytes(&path.with_extension("claimed"), guard)? != b"claimed\n"
        {
            return Err("Pi launch evidence is not activated or has moved".into());
        }
        let reg_path = plan.spec.registration.path();
        let reg_bytes = bytes(&reg_path, guard)?;
        guard.json(&reg_bytes)?;
        let registration: Registration =
            serde_json::from_slice(&reg_bytes).map_err(|_| "Invalid Pi registration")?;
        registration
            .validate()
            .map_err(|_| "Invalid Pi registration")?;
        if registration.scope != plan.spec.registration.scope
            || registration.id != plan.spec.registration.id
            || registration.directory != directory
            || registration.harness_process.is_none()
        {
            return Err("Pi registration differs from activated launch".into());
        }
        found.push(Evidence {
            plan_path: path,
            plan_bytes: raw,
            plan,
            reg_path,
            reg_bytes,
        });
    }
    Ok(found)
}
/// Resolve an exact UUID in its reviewed isolated directory, never by filename,
/// newest timestamp, partial UUID, terminal title, or unrelated user sessions.
pub fn saved(directory: &Path, id: &str, guard: &mut Guard<'_>) -> Result<Session> {
    let mut matched = None;
    for Evidence {
        plan_path,
        plan_bytes,
        plan,
        reg_path,
        reg_bytes,
    } in planned(directory, id, guard)?
    {
        let root = plan.plugin().join("sessions");
        layouts::check_path(&root)?;
        let entries = fs::read_dir(&root).map_err(|_| "Pi session directory is unavailable")?;
        for entry in entries {
            guard.entries(1)?;
            let path = entry.map_err(|_| "Cannot inspect Pi session entry")?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let (raw, device, inode) = header(&path, guard)?;
            guard.json(&raw)?;
            let value: Value =
                serde_json::from_slice(&raw).map_err(|_| "Malformed Pi session header")?;
            if value["type"] != "session" || value["version"] != 3 || value["id"].as_str().is_none()
            {
                return Err("Unsupported Pi session header".into());
            }
            if value["id"] != id {
                continue;
            }
            if value["cwd"].as_str() != plan.spec.working_directory.to_str() {
                return Err("Pi session project differs from launch plan".into());
            }
            if matched.is_some() {
                return Err("Pi session header is ambiguous".into());
            }
            matched = Some(Session {
                id: id.into(),
                path,
                cwd: plan.spec.working_directory.clone(),
                registration: reg_path.clone(),
                registration_bytes: reg_bytes.clone(),
                plan: plan_path.clone(),
                plan_bytes: plan_bytes.clone(),
                header: raw,
                device,
                inode,
            });
        }
    }
    guard.check()?;
    matched.ok_or_else(|| {
        "Pi session header is missing; wait for the provider to persist its session".into()
    })
}
/// The caller supplies already verified foreground evidence and rechecks it
/// after discovery. Registration PID/start and the complete launch argv must agree.
pub fn live(
    directory: &Path,
    terminal: &TerminalId,
    pid: &str,
    start: &str,
    argv: &[String],
    guard: &mut Guard<'_>,
) -> Result<Option<Session>> {
    let ids = argv
        .windows(2)
        .filter(|pair| pair[0] == "--session-id")
        .map(|pair| pair[1].as_str())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(None);
    }
    if ids.len() != 1 {
        return Err("Ambiguous Pi session launch ID".into());
    }
    let session = saved(directory, ids[0], guard)?;
    let plan: Plan =
        serde_json::from_slice(&session.plan_bytes).map_err(|_| "Invalid launch plan")?;
    let registration: Registration =
        serde_json::from_slice(&session.registration_bytes).map_err(|_| "Invalid registration")?;
    let process = registration
        .harness_process
        .ok_or("Pi process is not pinned")?;
    let mut expected = vec![plan.spec.native_binary.to_string_lossy().into_owned()];
    expected.extend(plan.command());
    if &registration.scope.terminal != terminal
        || process.pid.to_string() != pid
        || process.start_ticks.to_string() != start
        || argv != expected
    {
        return Err("Pi foreground identity differs from its launch registration".into());
    }
    session.verify(guard)?;
    Ok(Some(session))
}
impl Session {
    pub fn verify(&self, guard: &mut Guard<'_>) -> Result<()> {
        if bytes(&self.registration, guard)? != self.registration_bytes
            || bytes(&self.plan, guard)? != self.plan_bytes
        {
            return Err("Pi launch evidence changed; review again".into());
        }
        let (header, device, inode) = header(&self.path, guard)?;
        if header != self.header || (device, inode) != (self.device, self.inode) {
            return Err("Pi session file changed; review again".into());
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        app::{
            hook_runtime::{Options, ProcessIdentity},
            hooks::EventKind,
        },
        core::{goal_store::Scope, identity::Backend},
    };
    use serde_json::json;
    use std::os::unix::fs::symlink;
    fn guard() -> Guard<'static> {
        Guard::new(crate::core::native_metadata::Policy::default()).unwrap()
    }
    fn saved(directory: &Path, id: &str) -> Result<Session> {
        super::saved(directory, id, &mut guard())
    }
    fn live(
        directory: &Path,
        terminal: &TerminalId,
        pid: &str,
        start: &str,
        argv: &[String],
    ) -> Result<Option<Session>> {
        super::live(directory, terminal, pid, start, argv, &mut guard())
    }
    const SID: &str = "12345678-1234-1234-1234-123456789abc";
    struct Fixture {
        root: PathBuf,
        registration: Registration,
        plan: Plan,
        file: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT_FIXTURE: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "daedalus-pi-session-{}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                crate::test_support::unique_nanos()
            ));
            layouts::write_private(&root.join("marker"), b"").unwrap();
            let registration = Registration {
                version: 1,
                id: "fixture".into(),
                directory: root.clone(),
                scope: Scope {
                    instance: "example".into(),
                    terminal: TerminalId {
                        backend: Backend::Herdr,
                        endpoint: "fixture".into(),
                        pane: "pane".into(),
                        generation: "generation".into(),
                    },
                    conversation: SID.into(),
                    harness: Harness::Pi,
                },
                goal_id: None,
                harness_process: Some(ProcessIdentity {
                    pid: 17,
                    start_ticks: 42,
                }),
                server_emitter: None,
                transcript: None,
                events: vec![EventKind::Stop],
                options: Options::default(),
                activity_lease_ms: 30000,
                pending_timeout_ms: 30000,
                hook_timeout_ms: 12000,
            };
            let mut template = registration.clone();
            template.harness_process = None;
            let executable = json!({"device":0,"inode":0,"size":0,"modified":0,"modified_ns":0,"changed":0,"changed_ns":0});
            let plan:Plan=serde_json::from_value(json!({"spec":{"registration":template,"native_binary":"/fixture/node","entrypoint":"/fixture/cli.js","daedalus_binary":"/fixture/daedalus","working_directory":root,"observed_version":"1.1.4","expires_ms":1,"model":null},"native":executable,"daemon":executable,"entrypoint":executable,"hooks":[]})).unwrap();
            layouts::write_private(&plan.path(), &serde_json::to_vec(&plan).unwrap()).unwrap();
            layouts::write_private(&plan.path().with_extension("claimed"), b"claimed\n").unwrap();
            layouts::write_private(
                &registration.path(),
                &serde_json::to_vec(&registration).unwrap(),
            )
            .unwrap();
            let file = plan.plugin().join("sessions/arbitrary-name.jsonl");
            layouts::write_private(
                &file,
                format!(
                    "{}\n{{\"body\":\"DO NOT READ\"}}\n",
                    json!({"type":"session","version":3,"id":SID,"cwd":root})
                )
                .as_bytes(),
            )
            .unwrap();
            Self {
                root,
                registration,
                plan,
                file,
            }
        }
        fn argv(&self) -> Vec<String> {
            let mut args = vec!["/fixture/node".into()];
            args.extend(self.plan.command());
            args
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn saved_identity_shares_budget_with_rechecks_and_counts_ignored_files() {
        let f = Fixture::new();
        let original = fs::read(&f.file).unwrap();
        let session = saved(&f.root, SID).unwrap();
        let work = fs::metadata(f.plan.path()).unwrap().len()
            + fs::metadata(f.registration.path()).unwrap().len()
            + b"claimed\n".len() as u64
            + session.header.len() as u64;
        let mut budget = Guard::new(crate::core::native_metadata::Policy {
            work_bytes: work,
            ..Default::default()
        })
        .unwrap();
        let observed = super::saved(&f.root, SID, &mut budget).unwrap();
        assert!(
            observed
                .verify(&mut budget)
                .unwrap_err()
                .contains("work_limit")
        );
        fs::write(f.plan.path().with_extension("ignored"), b"").unwrap();
        let mut budget = Guard::new(crate::core::native_metadata::Policy {
            entries: 2,
            ..Default::default()
        })
        .unwrap();
        assert!(
            super::saved(&f.root, SID, &mut budget)
                .unwrap_err()
                .contains("entry_limit")
        );
        assert_eq!(fs::read(&f.file).unwrap(), original);
        layouts::write_private(
            &f.file,
            format!("{{\"type\":\"session\",\"version\":3,\"id\":\"other\",\"id\":\"{SID}\"}}\n")
                .as_bytes(),
        )
        .unwrap();
        assert!(saved(&f.root, SID).unwrap_err().contains("ambiguous JSON"));
    }
    #[test]
    fn discovers_header_not_filename_and_requires_pinned_process() {
        let f = Fixture::new();
        let s = live(
            &f.root,
            &f.registration.scope.terminal,
            "17",
            "42",
            &f.argv(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.id, SID);
        assert_eq!(s.path, f.file);
        assert!(!s.header.windows(12).any(|w| w == b"DO NOT READ"));
        assert!(
            live(
                &f.root,
                &f.registration.scope.terminal,
                "17",
                "43",
                &f.argv()
            )
            .is_err()
        );
        let mut args = f.argv();
        args.push("--unreviewed".into());
        assert!(live(&f.root, &f.registration.scope.terminal, "17", "42", &args).is_err());
        fs::remove_file(f.plan.path().with_extension("claimed")).unwrap();
        assert!(saved(&f.root, SID).is_err());
    }
    #[test]
    fn saved_session_rejects_replacement_duplicate_and_changed_registration() {
        let f = Fixture::new();
        let s = saved(&f.root, SID).unwrap();
        let original = fs::read(&f.file).unwrap();
        layouts::write_private(&f.file, &original).unwrap();
        assert!(s.verify(&mut guard()).is_err());
        let s = saved(&f.root, SID).unwrap();
        layouts::write_private(&f.file.with_file_name("duplicate.jsonl"), &original).unwrap();
        assert!(saved(&f.root, SID).is_err());
        let mut registration = f.registration.clone();
        registration.harness_process.as_mut().unwrap().start_ticks += 1;
        layouts::write_private(
            &registration.path(),
            &serde_json::to_vec(&registration).unwrap(),
        )
        .unwrap();
        assert!(s.verify(&mut guard()).is_err());
    }
    #[test]
    fn missing_malformed_oversized_and_symlink_headers_fail_closed() {
        let f = Fixture::new();
        fs::remove_file(&f.file).unwrap();
        assert!(saved(&f.root, SID).is_err());
        layouts::write_private(&f.file, b"not-json\n").unwrap();
        assert!(saved(&f.root, SID).is_err());
        layouts::write_private(&f.file, &vec![b'x'; 16385]).unwrap();
        assert!(saved(&f.root, SID).is_err());
        fs::remove_file(&f.file).unwrap();
        symlink(f.root.join("marker"), &f.file).unwrap();
        assert!(saved(&f.root, SID).is_err());
    }
}

#[cfg(test)]
mod parent_metadata_tests {
    use super::*;
    fn guard() -> Guard<'static> {
        Guard::new(crate::core::native_metadata::Policy::default()).unwrap()
    }
    fn fork_parent(argv: &[String], cwd: &Path) -> Result<Option<Parent>> {
        super::fork_parent(argv, cwd, &mut guard())
    }
    #[test]
    fn selected_parent_header_proves_child_identity_and_rechecks_replacement() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-pi-parent-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let file = root.join("arbitrary-file-name.jsonl");
        let id = "12345678-abcd-4000-8000-123456789abc";
        let raw = format!(
            "{}\n{{\"body\":\"NOT_METADATA\"}}\n",
            serde_json::json!({"type":"session","version":3,"id":id,"cwd":root})
        );
        layouts::write_private(&file, raw.as_bytes()).unwrap();
        let argv = [
            "pi".into(),
            "--fork".into(),
            "arbitrary-file-name.jsonl".into(),
        ];
        let parent = fork_parent(&argv, &root).unwrap().unwrap();
        assert_eq!(parent.id, id);
        assert!(!String::from_utf8_lossy(&parent.bytes).contains("NOT_METADATA"));
        parent.verify(&mut guard()).unwrap();
        assert_eq!(
            crate::core::checkpoints::resolve_sid_with_pi_parent(&argv, "pi", id, Some(&parent.id))
                .unwrap(),
            None
        );
        assert_eq!(
            crate::core::checkpoints::resolve_sid_with_pi_parent(
                &argv,
                "pi",
                "actual-child",
                Some(&parent.id)
            )
            .unwrap()
            .as_deref(),
            Some("actual-child")
        );
        let case_file = root.join("case-parent.jsonl");
        layouts::write_private(
            &case_file,
            format!(
                "{}\n",
                serde_json::json!({"type":"session","version":3,"id":"OwnedParent","cwd":root})
            )
            .as_bytes(),
        )
        .unwrap();
        let case_argv = ["pi".into(), "--fork".into(), "case-parent.jsonl".into()];
        let case_parent = fork_parent(&case_argv, &root).unwrap().unwrap();
        assert_eq!(
            crate::core::checkpoints::resolve_sid_with_pi_parent(
                &case_argv,
                "pi",
                "ownedparent",
                Some(&case_parent.id)
            )
            .unwrap()
            .as_deref(),
            Some("ownedparent")
        );
        let mut creation = case_argv.to_vec();
        creation.extend(["--session-id".into(), "ownedparent".into()]);
        assert_eq!(
            crate::core::checkpoints::resolve_sid_with_pi_parent(
                &creation,
                "pi",
                "",
                Some(&case_parent.id)
            )
            .unwrap()
            .as_deref(),
            Some("ownedparent")
        );
        layouts::write_private(&file, raw.as_bytes()).unwrap();
        assert!(parent.verify(&mut guard()).is_err());
        layouts::write_private(
            &file,
            b"{\"type\":\"session\",\"version\":3,\"id\":\"one\",\"id\":\"two\"}\n",
        )
        .unwrap();
        assert!(fork_parent(&argv, &root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
