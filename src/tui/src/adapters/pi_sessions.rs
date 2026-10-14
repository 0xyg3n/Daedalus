//! Native Pi session headers, scoped to an activated Daedalus launch plan.
//! UUIDs and transcript paths stay distinct. History bodies are never read.
use crate::{
    app::{hook_launch::Plan, hook_runtime::Registration, hooks::Harness},
    core::{identity::TerminalId, layouts},
};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
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
fn bytes(path: &Path) -> Result<Vec<u8>> {
    super::super::app::telegram_config::read_private(path, 1024 * 1024)?
        .ok_or_else(|| "Pi launch evidence is missing".into())
}
fn header(path: &Path) -> Result<(Vec<u8>, u64, u64)> {
    layouts::check_path(path)?;
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
    let mut line = Vec::new();
    BufReader::new(file.take(16385))
        .read_until(b'\n', &mut line)
        .map_err(|_| "Cannot read Pi session header")?;
    if line.len() > 16384 || !line.ends_with(b"\n") {
        return Err("Pi session header is incomplete or oversized".into());
    }
    Ok((line, metadata.dev(), metadata.ino()))
}
struct Evidence {
    plan_path: PathBuf,
    plan_bytes: Vec<u8>,
    plan: Plan,
    reg_path: PathBuf,
    reg_bytes: Vec<u8>,
}
fn planned(directory: &Path, id: &str) -> Result<Vec<Evidence>> {
    let root = directory.join("hook-launch");
    layouts::check_path(&root)?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(_) => return Err("Cannot inspect Pi launch plans".into()),
    };
    let mut found = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 1024 {
            return Err("Pi launch discovery limit exceeded".into());
        }
        let path = entry.map_err(|_| "Cannot inspect launch entry")?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let raw = bytes(&path)?;
        let plan: Plan = serde_json::from_slice(&raw).map_err(|_| "Invalid saved launch plan")?;
        if plan.spec.registration.scope.harness != Harness::Pi
            || plan.spec.registration.scope.conversation != id
        {
            continue;
        }
        if plan.path() != path
            || plan.spec.registration.directory != directory
            || bytes(&path.with_extension("claimed"))? != b"claimed\n"
        {
            return Err("Pi launch evidence is not activated or has moved".into());
        }
        let reg_path = plan.spec.registration.path();
        let reg_bytes = bytes(&reg_path)?;
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
pub fn saved(directory: &Path, id: &str) -> Result<Session> {
    let mut matches = Vec::new();
    for Evidence {
        plan_path,
        plan_bytes,
        plan,
        reg_path,
        reg_bytes,
    } in planned(directory, id)?
    {
        let root = plan.plugin().join("sessions");
        layouts::check_path(&root)?;
        let entries = fs::read_dir(&root).map_err(|_| "Pi session directory is unavailable")?;
        for (index, entry) in entries.enumerate() {
            if index >= 1024 {
                return Err("Pi session discovery limit exceeded".into());
            }
            let path = entry.map_err(|_| "Cannot inspect Pi session entry")?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let (raw, device, inode) = header(&path)?;
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
            matches.push(Session {
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
    if matches.len() != 1 {
        return Err("Pi session header is missing or ambiguous; wait for the provider to persist its session".into());
    }
    Ok(matches.remove(0))
}
/// The caller supplies already verified foreground evidence and rechecks it
/// after discovery. Registration PID/start and the complete launch argv must agree.
pub fn live(
    directory: &Path,
    terminal: &TerminalId,
    pid: &str,
    start: &str,
    argv: &[String],
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
    let session = saved(directory, ids[0])?;
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
    session.verify()?;
    Ok(Some(session))
}
impl Session {
    pub fn verify(&self) -> Result<()> {
        if bytes(&self.registration)? != self.registration_bytes
            || bytes(&self.plan)? != self.plan_bytes
        {
            return Err("Pi launch evidence changed; review again".into());
        }
        let (header, device, inode) = header(&self.path)?;
        if header != self.header || (device, inode) != (self.device, self.inode) {
            return Err("Pi session file changed; review again".into());
        }
        Ok(())
    }
}

#[cfg(test)]
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
    const SID: &str = "12345678-1234-1234-1234-123456789abc";
    struct Fixture {
        root: PathBuf,
        registration: Registration,
        plan: Plan,
        file: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "daedalus-pi-session-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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
        assert!(s.verify().is_err());
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
        assert!(s.verify().is_err());
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
