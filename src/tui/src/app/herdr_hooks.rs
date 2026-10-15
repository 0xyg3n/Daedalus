//! Reviewed external Herdr integration installation (Linux Claude/Codex/Grok).
//! The selected Herdr executable is trusted code, not a sandboxed installer.
//! File snapshots catch stale review; same-user concurrent external edits cannot
//! be serialized by Daedalus's private writer lock. No running agent is changed.
use crate::adapters::process;
#[cfg(unix)]
use crate::core::layouts;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    io::Read,
    time::{SystemTime, UNIX_EPOCH},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
type Result<T> = std::result::Result<T, String>;
#[cfg(unix)]
const LIMIT: u64 = 8 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub binary: PathBuf,
    pub home: PathBuf,
    pub profile: PathBuf,
    pub directory: PathBuf,
    pub harness: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub note: String,
    pub path: PathBuf,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Snapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub request: Request,
    pub status: Status,
    pub paths: Vec<PathBuf>,
    binary_sha: String,
    before: Vec<Snapshot>,
}
impl Plan {
    pub fn review(&self) -> serde_json::Value {
        serde_json::json!({"harness":self.request.harness,"profile":self.request.profile,"before":self.status,
            "paths":self.paths,"action":"Run Herdr integration install after private backup; applies to new sessions",
            "files":self.before.iter().map(|s|serde_json::json!({"path":s.path,"exists":s.bytes.is_some(),"sha256":s.bytes.as_ref().map(|b|hash(b))})).collect::<Vec<_>>()})
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(hash(
            &serde_json::to_vec(self).map_err(|_| "Cannot encode review")?,
        ))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub current: bool,
    pub after: Status,
    pub backup: PathBuf,
    pub diagnostic: String,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[cfg(unix)]
fn files(r: &Request) -> Result<Vec<PathBuf>> {
    let names = match r.harness.as_str() {
        "claude" => vec!["hooks/herdr-agent-state.sh", "settings.json"],
        "codex" => vec!["herdr-agent-state.sh", "hooks.json", "config.toml"],
        "grok" => vec!["hooks/herdr-agent-state.sh", "hooks/herdr.json"],
        _ => return Err("Herdr installation supports Claude, Codex and Grok profiles only".into()),
    };
    for p in [&r.binary, &r.home, &r.profile, &r.directory] {
        layouts::check_path(p)?;
        if p.to_string_lossy().chars().any(char::is_control) {
            return Err("Invalid integration path".into());
        }
    }
    for p in [&r.home, &r.profile] {
        let m = fs::metadata(p).map_err(|_| "Profile and home directories must exist")?;
        if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o022 != 0 {
            return Err(
                "Profile and home directories must be owned and not writable by others".into(),
            );
        }
    }
    let paths: Vec<PathBuf> = names.into_iter().map(|p| r.profile.join(p)).collect();
    for path in &paths {
        layouts::check_path(path)?;
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if !directory.starts_with(&r.profile) {
                break;
            }
            match fs::metadata(directory) {
                Ok(meta)
                    if !meta.is_dir()
                        || meta.uid() != unsafe { libc::geteuid() }
                        || meta.mode() & 0o022 != 0 =>
                {
                    return Err("Integration parent directory is unsafe".into());
                }
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err("Cannot inspect integration directory".into()),
            }
            parent = directory.parent();
        }
    }
    Ok(paths)
}
#[cfg(not(unix))]
#[allow(unused_variables)]
fn files(r: &Request) -> Result<Vec<PathBuf>> {
    Err("External Herdr hook installation uses Unix shell profiles; configure native Daedalus hooks for the Windows session instead".into())
}
#[cfg(unix)]
fn snapshot(path: &Path) -> Result<Snapshot> {
    layouts::check_path(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let mut file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                path: path.into(),
                bytes: None,
                mode: None,
            });
        }
        Err(_) => return Err("Cannot read reviewed integration file".into()),
    };
    let m = file
        .metadata()
        .map_err(|_| "Cannot inspect integration file")?;
    if !m.is_file()
        || m.nlink() != 1
        || ![0, unsafe { libc::geteuid() }].contains(&m.uid())
        || m.mode() & 0o022 != 0
        || m.len() > LIMIT
    {
        return Err("Unsafe or oversized integration file".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read integration file")?;
    if bytes.len() as u64 > LIMIT {
        return Err("Integration file exceeds limit".into());
    }
    Ok(Snapshot {
        path: path.into(),
        bytes: Some(bytes),
        mode: Some(m.mode() & 0o777),
    })
}
#[cfg(not(unix))]
#[allow(unused_variables)]
fn snapshot(path: &Path) -> Result<Snapshot> {
    Err("External Herdr hook installation uses Unix shell profiles; configure native Daedalus hooks for the Windows session instead".into())
}
#[cfg(unix)]
fn binary_sha(r: &Request) -> Result<String> {
    let m = fs::metadata(&r.binary).map_err(|_| "Herdr executable is missing")?;
    if !m.is_file()
        || m.mode() & 0o111 == 0
        || m.mode() & 0o022 != 0
        || ![0, unsafe { libc::geteuid() }].contains(&m.uid())
    {
        return Err("Unsafe Herdr executable".into());
    }
    layouts::check_path(&r.binary)?;
    // Executables may exceed profile size limits; hash with bounded streaming.
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&r.binary)
        .map_err(|_| "Cannot pin Herdr executable")?;
    if m.len() > 512 * 1024 * 1024 {
        return Err("Herdr executable exceeds limit".into());
    }
    let mut h = Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0u64;
    loop {
        let n = f
            .read(&mut buffer)
            .map_err(|_| "Cannot hash Herdr executable")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > 512 * 1024 * 1024 {
            return Err("Herdr executable exceeds limit".into());
        }
        h.update(&buffer[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
#[cfg(not(unix))]
#[allow(unused_variables)]
fn binary_sha(r: &Request) -> Result<String> {
    Err("External Herdr hook installation uses Unix shell profiles; configure native Daedalus hooks for the Windows session instead".into())
}
fn command(r: &Request, args: &[&str], timeout: Duration) -> Result<process::Output> {
    let mut c = Command::new(&r.binary);
    c.args(args)
        .env_clear()
        .env("HOME", &r.home)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C")
        .env("XDG_STATE_HOME", r.directory.join("herdr-state"))
        .current_dir(&r.profile);
    let key = match r.harness.as_str() {
        "claude" => "CLAUDE_CONFIG_DIR",
        "codex" => "CODEX_HOME",
        "grok" => "GROK_CONFIG_DIR",
        _ => return Err("Unsupported Herdr integration".into()),
    };
    c.env(key, &r.profile);
    process::run(&mut c, timeout, 128 * 1024)
        .map_err(|_| "Herdr query failed or exceeded its limits".into())
}
fn parse(bytes: &[u8], harness: &str) -> Result<Status> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Herdr status is not UTF-8")?;
    let prefix = format!("{harness}: ");
    let mut found = None;
    for line in text.lines().filter_map(|l| l.strip_prefix(&prefix)) {
        if found.is_some() || line.chars().any(char::is_control) {
            return Err("Ambiguous Herdr integration status".into());
        }
        let (description, path) = line
            .rsplit_once(" (")
            .ok_or("Herdr status has no exact path")?;
        let path = path.strip_suffix(')').ok_or("Malformed Herdr status")?;
        let (state, note) = description
            .split_once(" (")
            .map_or((description, ""), |(s, n)| {
                (s, n.strip_suffix(')').unwrap_or(n))
            });
        if !matches!(
            state,
            "current" | "outdated" | "needs repair" | "not installed"
        ) {
            return Err("Unknown Herdr integration state".into());
        }
        found = Some(Status {
            state: state.into(),
            note: note.into(),
            path: PathBuf::from(path),
        });
    }
    found.ok_or("Herdr did not report this integration".into())
}
pub fn status(r: &Request) -> Result<Status> {
    let paths = files(r)?;
    binary_sha(r)?;
    let out = command(r, &["integration", "status"], Duration::from_secs(5))?;
    if !out.status.success() {
        return Err("Herdr status query failed".into());
    }
    let status = parse(&out.stdout, &r.harness)?;
    if status.path != paths[0] {
        return Err("Herdr reports a different profile path; nothing changed".into());
    }
    Ok(status)
}
pub fn prepare(r: &Request) -> Result<Plan> {
    let paths = files(r)?;
    let binary_sha = binary_sha(r)?;
    let before = paths
        .iter()
        .map(|p| snapshot(p))
        .collect::<Result<Vec<_>>>()?;
    let status = status(r)?;
    Ok(Plan {
        request: r.clone(),
        status,
        paths,
        binary_sha,
        before,
    })
}
#[cfg(unix)]
pub fn apply(plan: &Plan, approved: &str) -> Result<Receipt> {
    if plan.fingerprint()? != approved {
        return Err("Integration approval does not match review".into());
    }
    let _active = super::hook_runtime::active_guard(&plan.request.directory)
        .map_err(|_| "Integration state is retired, busy or unsafe")?;
    let _writer = super::hook_install::lock(&plan.request.directory)?;
    if prepare(&plan.request)?.fingerprint()? != approved {
        return Err("Herdr executable, profile or status changed; review again".into());
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Invalid clock")?
        .as_nanos();
    let backup = plan
        .request
        .directory
        .join(format!("herdr-backup-{}-{nonce}", std::process::id()));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&backup)
        .map_err(|_| "Cannot create private integration backup")?;
    layouts::write_private(
        &backup.join("before.json"),
        &serde_json::to_vec(plan).map_err(|_| "Cannot encode private backup")?,
    )?;
    // After this point failures may have changed the profile. Never retry install
    // automatically or overwrite a user's subsequent changes with a rollback.
    let installed = command(
        &plan.request,
        &["integration", "install", &plan.request.harness],
        Duration::from_secs(30),
    );
    let after = status(&plan.request).unwrap_or(Status {
        state: "unknown".into(),
        note: String::new(),
        path: plan.paths[0].clone(),
    });
    let current = installed.as_ref().is_ok_and(|o| o.status.success()) && after.state == "current";
    let receipt = Receipt {
        current,
        after,
        backup: backup.clone(),
        diagnostic: if current {
            "Verified by Herdr; new sessions use this integration"
        } else {
            "Installation not verified; inspect the private backup and profile before retrying"
        }
        .into(),
    };
    layouts::write_private(
        &backup.join("receipt.json"),
        &serde_json::to_vec(&receipt).map_err(|_| "Cannot encode installation receipt")?,
    )?;
    Ok(receipt)
}
#[cfg(not(unix))]
#[allow(unused_variables)]
pub fn apply(plan: &Plan, approved: &str) -> Result<Receipt> {
    Err("External Herdr hook installation uses Unix shell profiles; configure native Daedalus hooks for the Windows session instead".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(Request);
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "daedalus-herdr-hooks-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            let home = directory.join("home");
            let profile = home.join(".claude");
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&profile)
                .unwrap();
            let binary = directory.join("herdr");
            fs::write(&binary,r#"#!/bin/sh
umask 077
if [ "$1" != integration ]; then exit 2; fi
if [ "$2" = status ]; then
 state='not installed'; if [ -f "$CLAUDE_CONFIG_DIR/hooks/herdr-agent-state.sh" ]; then state='current (v8)'; fi
 printf 'claude: %s (%s/hooks/herdr-agent-state.sh)\n' "$state" "$CLAUDE_CONFIG_DIR"
else
 mkdir -p "$CLAUDE_CONFIG_DIR/hooks"
 printf 'fixture script' > "$CLAUDE_CONFIG_DIR/hooks/herdr-agent-state.sh"
 printf '{"hooks":{}}' > "$CLAUDE_CONFIG_DIR/settings.json"
fi
"#).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
            Self(Request {
                binary,
                home,
                profile,
                directory,
                harness: "claude".into(),
            })
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0.directory);
        }
    }
    #[test]
    fn review_backup_apply_and_observed_status() {
        let f = Fixture::new();
        fs::write(
            f.0.profile.join("settings.json"),
            br#"{"custom":"private-value"}"#,
        )
        .unwrap();
        if f.0.profile.join("settings.json").exists() {
            fs::set_permissions(
                f.0.profile.join("settings.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let plan = prepare(&f.0).unwrap();
        assert_eq!(plan.status.state, "not installed");
        assert!(!plan.review().to_string().contains("private-value"));
        let receipt = apply(&plan, &plan.fingerprint().unwrap()).unwrap();
        assert!(receipt.current);
        let restored: Plan =
            serde_json::from_slice(&fs::read(receipt.backup.join("before.json")).unwrap()).unwrap();
        assert_eq!(
            restored.before[1].bytes.as_deref(),
            Some(br#"{"custom":"private-value"}"#.as_slice())
        );
        assert_eq!(fs::metadata(&receipt.backup).unwrap().mode() & 0o777, 0o700);
        assert!(apply(&plan, &plan.fingerprint().unwrap()).is_err());
    }
    #[test]
    fn stale_profile_or_binary_never_installs() {
        let f = Fixture::new();
        if f.0.profile.join("settings.json").exists() {
            fs::set_permissions(
                f.0.profile.join("settings.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let plan = prepare(&f.0).unwrap();
        fs::write(f.0.profile.join("settings.json"), b"changed").unwrap();
        assert!(apply(&plan, &plan.fingerprint().unwrap()).is_err());
        assert!(!f.0.profile.join("hooks").exists());
        fs::set_permissions(
            f.0.profile.join("settings.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let next = prepare(&f.0).unwrap();
        let mut bytes = fs::read(&f.0.binary).unwrap();
        bytes.extend_from_slice(b"\n# revised\n");
        fs::write(&f.0.binary, bytes).unwrap();
        assert!(apply(&next, &next.fingerprint().unwrap()).is_err());
    }
    #[test]
    fn status_refuses_other_profile_unknown_duplicate_and_oversized_files() {
        let f = Fixture::new();
        assert!(
            parse(
                b"claude: current (v8) (/elsewhere/hook)\nclaude: current (v8) (/elsewhere/hook)",
                "claude"
            )
            .is_err()
        );
        assert!(parse(b"claude: maybe (/path)", "claude").is_err());
        std::os::unix::fs::symlink("/dev/null", f.0.profile.join("settings.json")).unwrap();
        assert!(prepare(&f.0).is_err());
        fs::remove_file(f.0.profile.join("settings.json")).unwrap();
        let file = fs::File::create(f.0.profile.join("settings.json")).unwrap();
        file.set_len(LIMIT + 1).unwrap();
        assert!(prepare(&f.0).is_err());
    }
    #[test]
    fn exit_success_without_current_status_is_not_verified() {
        let f = Fixture::new();
        let source = fs::read_to_string(&f.0.binary).unwrap();
        let source = source.replace(
            "mkdir -p \"$CLAUDE_CONFIG_DIR/hooks\"",
            "exit 0\nmkdir -p \"$CLAUDE_CONFIG_DIR/hooks\"",
        );
        fs::write(&f.0.binary, source).unwrap();
        if f.0.profile.join("settings.json").exists() {
            fs::set_permissions(
                f.0.profile.join("settings.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let plan = prepare(&f.0).unwrap();
        let receipt = apply(&plan, &plan.fingerprint().unwrap()).unwrap();
        assert!(!receipt.current);
        assert_eq!(receipt.after.state, "not installed");
        assert!(receipt.backup.join("before.json").is_file());
    }
}
