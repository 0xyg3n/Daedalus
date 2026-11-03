//! Capture only one Codex conversation's retained history. Native migration,
//! rather than writes to Codex's SQLite tables, rebuilds its paginated index.
use crate::core::{json_preflight, layouts};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
const INVALID: &str = "Codex fork history is invalid, incomplete or ambiguous";
const BUDGET: &str = "Codex fork history limit reached; review session_fork runtime settings";

pub(crate) struct Budget {
    pub(crate) deadline: Instant,
    bytes: u64,
    entries: u64,
    values: u64,
}
impl Budget {
    pub(crate) fn new(settings: &super::runtime_settings::Settings) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            deadline: Instant::now() + Duration::from_millis(settings.session_fork_timeout_ms),
            bytes: settings.session_fork_limit_bytes,
            entries: settings.session_fork_entry_limit,
            values: settings.session_fork_value_limit,
        })
    }
    pub(crate) fn remaining(&self) -> Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err("Codex fork deadline reached; review session_fork_timeout_ms".into())
        } else {
            Ok(remaining)
        }
    }
    fn entry(&mut self) -> Result<()> {
        self.remaining()?;
        self.entries = self.entries.checked_sub(1).ok_or(BUDGET)?;
        Ok(())
    }
    fn bytes(&mut self, count: u64) -> Result<()> {
        self.remaining()?;
        self.bytes = self.bytes.checked_sub(count).ok_or(BUDGET)?;
        Ok(())
    }
    fn json(&mut self, bytes: &[u8]) -> Result<Value> {
        self.entry()?;
        json_preflight::check(bytes, || {
            self.remaining()?;
            self.values = self.values.checked_sub(1).ok_or(BUDGET)?;
            Ok(())
        })
        .map_err(|error| {
            if error == json_preflight::INVALID {
                INVALID.into()
            } else {
                error
            }
        })?;
        serde_json::from_slice(bytes).map_err(|_| INVALID.into())
    }
}

pub(crate) struct Capture {
    pub(crate) id: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) records: usize,
}
pub(crate) fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn new_id() -> Result<String> {
    let mut bytes = [0; 16];
    crate::platform::random_bytes(&mut bytes).map_err(|_| "Cannot allocate fork identity")?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

// Inventory filenames, never the contents of unrelated conversations.
fn inventory(home: &Path, budget: &mut Budget) -> Result<BTreeMap<String, Vec<PathBuf>>> {
    layouts::check_path(home)?;
    let mut found = BTreeMap::<String, Vec<PathBuf>>::new();
    let mut pending = vec![home.join("sessions"), home.join("archived_sessions")];
    while let Some(directory) = pending.pop() {
        budget.entry()?;
        layouts::check_path(&directory)?;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Cannot inspect Codex history directory".into()),
        };
        for entry in entries {
            budget.entry()?;
            let entry = entry.map_err(|_| "Cannot inspect Codex history entry")?;
            let kind = entry
                .file_type()
                .map_err(|_| "Cannot inspect Codex history entry")?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() || kind.is_symlink() {
                let name = entry.file_name();
                let Some(name) = name.to_str().and_then(|s| s.strip_suffix(".jsonl")) else {
                    continue;
                };
                if let Some(id) = name.get(name.len().saturating_sub(36)..)
                    && uuid(id)
                {
                    found
                        .entry(id.to_ascii_lowercase())
                        .or_default()
                        .push(entry.path());
                }
            }
        }
    }
    Ok(found)
}
fn open(path: &Path) -> Result<File> {
    layouts::check_path(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "Cannot open selected Codex history")?;
        let meta = file.metadata().map_err(|_| INVALID)?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
            return Err("Selected Codex history must be a regular owned file".into());
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        crate::platform::open_private(path, false, false)
            .map_err(|_| "Cannot open selected private Codex history".into())
    }
}
fn read(path: &Path, length: Option<u64>, budget: &mut Budget) -> Result<Vec<u8>> {
    let mut file = open(path)?;
    let meta = file.metadata().map_err(|_| INVALID)?;
    let length = length.unwrap_or(meta.len());
    if length == 0 || length > meta.len() {
        return Err(INVALID.into());
    }
    // Two bounded reads ensure that the retained prefix did not change. Later
    // appends are allowed on ancestors, but are never copied past their cutoff.
    budget.bytes(length.checked_mul(2).ok_or(BUDGET)?)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(length)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 != length || !bytes.ends_with(b"\n") {
        return Err(INVALID.into());
    }
    let mut again = Vec::new();
    open(path)?
        .take(length)
        .read_to_end(&mut again)
        .map_err(|_| INVALID)?;
    if bytes != again {
        return Err("Selected Codex history changed during capture".into());
    }
    budget.remaining()?;
    Ok(bytes)
}
fn number(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| INVALID.into())
}

pub(crate) fn capture(home: &Path, sid: &str, budget: &mut Budget) -> Result<Capture> {
    if !uuid(sid) {
        return Err("Codex fork needs an exact conversation UUID".into());
    }
    let files = inventory(home, budget)?;
    let mut current = sid.to_owned();
    let mut prefix = None;
    let mut ordinal_end = None;
    let mut visited = BTreeSet::new();
    let mut segments = Vec::<Vec<Value>>::new();
    loop {
        budget.entry()?;
        if !visited.insert(current.to_ascii_lowercase()) {
            return Err("Codex fork history contains a cycle".into());
        }
        let paths = files
            .get(&current.to_ascii_lowercase())
            .ok_or("Required Codex fork ancestor is missing")?;
        if paths.len() != 1 {
            return Err("Multiple rollouts report the selected Codex conversation".into());
        }
        let bytes = read(&paths[0], prefix, budget)?;
        let mut rows = bytes.split(|b| *b == b'\n').collect::<Vec<_>>();
        rows.pop();
        let rows = rows
            .into_iter()
            .map(|row| budget.json(row))
            .collect::<Result<Vec<_>>>()?;
        let first = rows.first().ok_or(INVALID)?;
        let meta = &first["payload"];
        if first["type"] != "session_meta"
            || !meta["id"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(&current))
            || rows.iter().skip(1).any(|row| row["type"] == "session_meta")
        {
            return Err(INVALID.into());
        }
        let paginated = match meta.get("history_mode") {
            Some(Value::String(mode)) if mode == "paginated" => true,
            Some(Value::String(mode)) if mode == "legacy" => false,
            None | Some(Value::Null) => false,
            _ => return Err("Unsupported Codex history format".into()),
        };
        let base = meta.get("history_base").filter(|v| !v.is_null());
        if base.is_none() && meta.get("forked_from_ordinal_exclusive").is_some() {
            return Err("Codex fork ancestor metadata is incomplete".into());
        }
        if paginated {
            let start = if let Some(base) = base {
                number(base, "end_ordinal_exclusive")?
            } else {
                0
            };
            for (index, row) in rows.iter().enumerate() {
                if number(row, "ordinal")? != start.checked_add(index as u64).ok_or(INVALID)? {
                    return Err(INVALID.into());
                }
            }
            if let Some(end) = ordinal_end
                && number(rows.last().ok_or(INVALID)?, "ordinal")?.checked_add(1) != Some(end)
            {
                return Err("Codex ancestor cutoff does not match its retained prefix".into());
            }
        } else if base.is_some()
            || ordinal_end.is_some()
            || rows.iter().any(|row| row.get("ordinal").is_some())
        {
            return Err(INVALID.into());
        }
        let next = if let Some(base) = base {
            if !paginated {
                return Err(INVALID.into());
            }
            let id = base["thread_id"]
                .as_str()
                .filter(|id| uuid(id))
                .ok_or(INVALID)?;
            let end = number(base, "end_ordinal_exclusive")?;
            if meta["forked_from_id"].as_str() != Some(id)
                || meta["forked_from_ordinal_exclusive"].as_u64() != Some(end)
            {
                return Err(INVALID.into());
            }
            Some((id.to_owned(), number(base, "end_byte_offset")?, end))
        } else {
            None
        };
        segments.push(rows);
        let Some((id, length, end)) = next else { break };
        current = id;
        prefix = Some(length);
        ordinal_end = Some(end);
    }
    let id = new_id()?;
    let mut meta = segments[0][0].clone();
    meta.as_object_mut().ok_or(INVALID)?.remove("ordinal");
    let payload = meta["payload"].as_object_mut().ok_or(INVALID)?;
    payload.insert("id".into(), Value::String(id.clone()));
    payload.insert("session_id".into(), Value::String(id.clone()));
    payload.insert("forked_from_id".into(), Value::String(sid.into()));
    payload.insert("history_mode".into(), Value::String("legacy".into()));
    payload.remove("history_base");
    payload.remove("forked_from_ordinal_exclusive");
    let mut bytes = Vec::new();
    let mut records = 0;
    for mut row in std::iter::once(meta).chain(
        segments
            .into_iter()
            .rev()
            .flat_map(|rows| rows.into_iter().skip(1)),
    ) {
        budget.entry()?;
        row.as_object_mut().ok_or(INVALID)?.remove("ordinal");
        let encoded = serde_json::to_vec(&row).map_err(|_| INVALID)?;
        budget.bytes(encoded.len() as u64 + 1)?;
        bytes.extend(encoded);
        bytes.push(b'\n');
        records += 1;
    }
    budget.remaining()?;
    Ok(Capture { id, bytes, records })
}

fn publish(home: &Path, captured: &Capture) -> Result<PathBuf> {
    let directory = home.join("sessions/1970/01/01");
    layouts::check_path(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(|_| "Cannot create private fork history directory")?;
        let meta = fs::metadata(&directory).map_err(|_| INVALID)?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err("Fork history directory must be private and owned".into());
        }
    }
    #[cfg(windows)]
    crate::platform::ensure_private_dir(&directory)
        .map_err(|_| "Cannot create private fork history directory")?;
    let path = directory.join(format!("rollout-1970-01-01T00-00-00-{}.jsonl", captured.id));
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| "Cannot create unique fork history file")?
    };
    #[cfg(windows)]
    let mut file = crate::platform::create_private_new(&path)
        .map_err(|_| "Cannot create unique fork history file")?;
    file.write_all(&captured.bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "Cannot persist fork history")?;
    crate::platform::sync_directory(&directory)
        .map_err(|_| "Cannot sync fork history directory")?;
    Ok(path)
}

/// This function runs only after confirmation. Both commands omit a prompt;
/// hooks are disabled for the internal import, then retained on the final UI.
pub(crate) fn create(
    source: &Path,
    sid: &str,
    plan: &crate::core::checkpoints::LaunchPlan,
    settings: &super::runtime_settings::Settings,
) -> Result<String> {
    use crate::adapters::{harnesses::ProcessPlan, process};
    let home = Path::new(
        plan.environment
            .get("CODEX_HOME")
            .ok_or("Fork license has no native home")?,
    );
    let native = ProcessPlan {
        binary: plan.binary.clone(),
        args: vec![],
        cwd: plan.cwd.clone(),
        environment: plan.environment.clone(),
        secret_stdin: false,
        instruction: "",
    };
    let mut budget = Budget::new(settings)?;
    let run = |args: &[&str], budget: &Budget| -> Result<process::Output> {
        let output=process::run(native.command().args(["--disable","hooks"]).args(args),budget.remaining()?,settings.harness_probe_limit_bytes as usize)
            .map_err(|_|"Native Codex history operation did not confirm completion; do not automatically retry")?;
        if !output.status.success() {
            return Err("Native Codex history operation failed; no terminal was launched".into());
        }
        Ok(output)
    };
    // Capability checks precede every write, and use the same binary/account.
    let help = run(&["migrate-rollouts", "--help"], &budget)?;
    let help =
        String::from_utf8(help.stdout).map_err(|_| "Invalid native history capability response")?;
    if !help.contains("--apply") || !help.contains("--thread") {
        return Err("Installed Codex cannot migrate a selected fork's history".into());
    }
    let help = run(&["exec", "fork", "--help"], &budget)?;
    let help =
        String::from_utf8(help.stdout).map_err(|_| "Invalid native fork capability response")?;
    if !help.contains("[PROMPT]") || !help.contains("--json") || !help.contains("SESSION_ID") {
        return Err("Installed Codex lacks the tested prompt-free fork contract".into());
    }
    let captured = capture(source, sid, &mut budget)?;
    if captured.records == 0 {
        return Err(INVALID.into());
    }
    let path = publish(home, &captured)?;
    let result = (|| {
        run(
            &["migrate-rollouts", "--apply", "--thread", &captured.id],
            &budget,
        )?;
        let output = run(
            &[
                "exec",
                "fork",
                "--skip-git-repo-check",
                "--json",
                &captured.id,
            ],
            &budget,
        )?;
        let mut child = None;
        for line in output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
        {
            let value = budget.json(line)?;
            if value["type"] != "thread.started" || child.is_some() {
                return Err(
                    "Native fork returned an unexpected event; completion is unconfirmed".into(),
                );
            }
            let id = value["thread_id"]
                .as_str()
                .filter(|id| uuid(id) && *id != captured.id && *id != sid)
                .ok_or("Native fork did not report a distinct conversation identity")?;
            child = Some(id.to_owned());
        }
        child.ok_or_else(|| "Native fork did not report its conversation identity".into())
    })();
    result.map_err(|error: String| {
        format!("{error}. Imported history retained at {}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const ROOT: &str = "12345678-abcd-4000-8000-123456789abc";
    const CHILD: &str = "22345678-abcd-4000-8000-123456789abc";
    const LEAF: &str = "32345678-abcd-4000-8000-123456789abc";
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("daedalus-fork-{}", new_id().unwrap()));
            Self(root)
        }
        fn write(&self, id: &str, rows: &[Value]) -> (PathBuf, Vec<u8>) {
            let path = self
                .0
                .join("sessions/2026/11/03")
                .join(format!("rollout-fixture-{id}.jsonl"));
            let bytes: Vec<u8> = rows
                .iter()
                .flat_map(|r| {
                    let mut v = serde_json::to_vec(r).unwrap();
                    v.push(b'\n');
                    v
                })
                .collect();
            layouts::write_private_limit(&path, &bytes, 1_000_000).unwrap();
            (path, bytes)
        }
        fn budget(&self) -> Budget {
            Budget::new(&super::super::runtime_settings::Settings::default()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn meta(id: &str, ordinal: u64) -> Value {
        json!({"ordinal":ordinal,"type":"session_meta","payload":{"id":id,"history_mode":"paginated"}})
    }
    fn message(ordinal: u64, text: &str) -> Value {
        json!({"ordinal":ordinal,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}})
    }
    fn branch(id: &str, parent: &str, ordinal: u64, bytes: u64) -> Value {
        let mut row = meta(id, ordinal);
        row["payload"]["forked_from_id"] = json!(parent);
        row["payload"]["forked_from_ordinal_exclusive"] = json!(ordinal);
        row["payload"]["history_base"] =
            json!({"thread_id":parent,"end_ordinal_exclusive":ordinal,"end_byte_offset":bytes});
        row
    }
    #[test]
    fn selected_lineage_keeps_exact_prefixes_and_preserves_source() {
        let f = Fixture::new();
        let (root, prefix) = f.write(ROOT, &[meta(ROOT, 0), message(1, "root-retained")]);
        let (_, child) = f.write(
            CHILD,
            &[
                branch(CHILD, ROOT, 2, prefix.len() as u64),
                message(3, "child-retained"),
            ],
        );
        f.write(
            LEAF,
            &[
                branch(LEAF, CHILD, 4, child.len() as u64),
                message(5, "leaf-retained"),
            ],
        );
        let (_, later) = f.write(
            ROOT,
            &[
                meta(ROOT, 0),
                message(1, "root-retained"),
                message(2, "later-excluded"),
            ],
        );
        let captured = capture(&f.0, LEAF, &mut f.budget()).unwrap();
        let text = String::from_utf8(captured.bytes).unwrap();
        assert!(
            text.contains("root-retained")
                && text.contains("child-retained")
                && text.contains("leaf-retained")
        );
        assert!(
            !text.contains("later-excluded")
                && !text.contains("history_base")
                && !text.contains("\"ordinal\"")
        );
        let header: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(header["payload"]["forked_from_id"], LEAF);
        assert_eq!(header["payload"]["id"], captured.id);
        assert_ne!(captured.id, LEAF);
        assert_eq!(captured.records, 4);
        assert_eq!(fs::read(root).unwrap(), later);
    }
    #[test]
    fn missing_ambiguous_partial_and_inconsistent_ancestors_are_refused() {
        let f = Fixture::new();
        f.write(CHILD, &[branch(CHILD, ROOT, 2, 10), message(3, "child")]);
        assert!(capture(&f.0, CHILD, &mut f.budget()).is_err());
        let (root, bytes) = f.write(ROOT, &[meta(ROOT, 0), message(1, "root")]);
        assert!(capture(&f.0, CHILD, &mut f.budget()).is_err());
        f.write(CHILD, &[branch(CHILD, ROOT, 3, bytes.len() as u64)]);
        assert!(capture(&f.0, CHILD, &mut f.budget()).is_err());
        let duplicate = f.0.join("sessions").join(format!("other-{ROOT}.jsonl"));
        layouts::write_private_limit(&duplicate, &bytes, 1_000_000).unwrap();
        assert!(
            capture(&f.0, ROOT, &mut f.budget())
                .err()
                .unwrap()
                .contains("Multiple")
        );
        fs::remove_file(duplicate).unwrap();
        fs::write(root, &bytes[..bytes.len() - 1]).unwrap();
        assert!(capture(&f.0, ROOT, &mut f.budget()).is_err());
    }
    #[test]
    fn history_json_and_cumulative_budgets_are_enforced_without_partial_results() {
        let f = Fixture::new();
        let (path, bytes) = f.write(ROOT, &[meta(ROOT, 0), message(1, "root")]);
        for which in 0..4 {
            let mut budget = f.budget();
            match which {
                0 => budget.bytes = bytes.len() as u64,
                1 => budget.entries = 1,
                2 => budget.values = 1,
                _ => budget.deadline = Instant::now(),
            }
            assert!(capture(&f.0, ROOT, &mut budget).is_err());
        }
        fs::write(path,format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{ROOT}\",\"id\":\"{CHILD}\"}}}}\n")).unwrap();
        assert!(capture(&f.0, ROOT, &mut f.budget()).is_err());
    }
    #[test]
    #[ignore = "Explicit private native-provider fixture; no real accounts or model requests"]
    fn owned_native_history_fixture() {
        use crate::core::checkpoints::{LaunchPlan, Operation};
        let root = PathBuf::from(
            std::env::var("DAEDALUS_FORK_FIXTURE_ROOT").expect("Owned fixture root required"),
        );
        layouts::check_path(&root).unwrap();
        let input: Value = serde_json::from_slice(
            &layouts::read_private(&root.join("native-fork-request.json"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let binary = PathBuf::from(input["binary"].as_str().unwrap());
        assert!(binary.is_absolute());
        let base = input["local_base_url"].as_str().unwrap();
        assert!(base.starts_with("http://127.0.0.1:"));
        let plan = LaunchPlan {
            binary,
            args: vec![],
            cwd: root.join("project"),
            operation: Operation::NativeFork,
            environment: [
                ("PATH".into(), std::env::var("PATH").unwrap()),
                ("HOME".into(), root.to_string_lossy().into_owned()),
                (
                    "CODEX_HOME".into(),
                    root.join("target").to_string_lossy().into_owned(),
                ),
                ("HTTP_PROXY".into(), base.into()),
                ("HTTPS_PROXY".into(), base.into()),
                ("ALL_PROXY".into(), base.into()),
                ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
            ]
            .into(),
        };
        let id = create(
            &root.join("source"),
            input["sid"].as_str().unwrap(),
            &plan,
            &super::super::runtime_settings::Settings::default(),
        )
        .unwrap();
        layouts::write_private(
            &root.join("native-fork-result.json"),
            &serde_json::to_vec(&json!({"id":id})).unwrap(),
        )
        .unwrap();
    }
}
