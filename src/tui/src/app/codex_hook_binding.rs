//! Codex hook emitters (Linux `/proc`, macOS `libproc`) and their independent Herdr terminal clients.
//! Only an already registered conversation may refresh its proved binding.
use super::hook_runtime::{self, Error, ProcessIdentity, Registration, Result, ServerEmitter};
use crate::{adapters::terminals::Terminals, core::identity::Backend};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    identity: ProcessIdentity,
    parent: u32,
    tty: i64,
    uid: u32,
    argv: Vec<String>,
}
impl Process {
    fn codex(&self) -> bool {
        self.argv.first().is_some_and(|arg| {
            Path::new(arg)
                .file_name()
                .is_some_and(|name| name == "codex")
        })
    }
    fn owned(&self) -> bool {
        self.uid == unsafe { libc::geteuid() }
    }
    fn managed(&self) -> bool {
        self.tty == 0
            && self.argv.iter().any(|arg| arg == "app-server")
            && self.argv.iter().any(|arg| arg == "--managed-daemon")
    }
}
fn process(root: &Path, pid: u32) -> Result<Process> {
    let info = crate::platform::process_info(root, pid).ok_or(Error::Identity)?;
    if matches!(info.state, 'Z' | 'X' | 'T' | 't') {
        return Err(Error::Identity);
    }
    let argv = crate::platform::process_argv(root, pid).ok_or(Error::Identity)?;
    Ok(Process {
        identity: ProcessIdentity {
            pid,
            start_ticks: info.start,
        },
        parent: info.parent,
        tty: info.tty,
        uid: info.uid,
        argv,
    })
}
fn ancestor(root: &Path, mut pid: u32) -> Result<Option<Process>> {
    let mut seen = BTreeSet::new();
    for _ in 0..128 {
        if pid <= 1 {
            return Ok(None);
        }
        if !seen.insert(pid) {
            return Err(Error::Identity);
        }
        let row = process(root, pid)?;
        if row.codex() {
            if !row.owned() {
                return Err(Error::Identity);
            }
            return Ok(Some(row));
        }
        pid = row.parent;
    }
    Err(Error::Identity)
}
fn pin(root: &Path, emitter: &Process) -> Result<ServerEmitter> {
    if !emitter.owned() || !emitter.codex() || !emitter.managed() {
        return Err(Error::Identity);
    }
    let executable =
        crate::platform::process_executable(root, emitter.identity.pid).ok_or(Error::Identity)?;
    if executable.file_name().is_none_or(|name| name != "codex") {
        return Err(Error::Identity);
    }
    hook_runtime::capture_server_emitter(emitter.identity.clone(), &executable, root)
}
fn selected(pane: &Value) -> Option<&str> {
    let session = pane.get("agent_session")?;
    (session["kind"] == "id")
        .then(|| session["value"].as_str())
        .flatten()
}
fn client(
    root: &Path,
    registration: &Registration,
    pane: &Value,
    info: &Value,
    emitter: &Process,
) -> Result<Process> {
    let conversation = registration.scope.conversation.as_str();
    if selected(pane).is_some_and(|session| session != conversation) {
        return Err(Error::Identity);
    }
    let rows = info["foreground_processes"]
        .as_array()
        .ok_or(Error::Identity)?;
    let mut found = None;
    let mut pids = BTreeSet::new();
    for row in rows {
        let pid = row["pid"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or(Error::Identity)?;
        if !pids.insert(pid) {
            return Err(Error::Identity);
        }
        let observed = process(root, pid)?;
        if !observed.codex() || !observed.owned() || observed.tty == 0 {
            continue;
        }
        let matches = if emitter.tty != 0 {
            observed == *emitter
        } else {
            crate::core::checkpoints::resolve_sid(
                &observed.argv,
                "codex",
                selected(pane).unwrap_or_default(),
            )
            .map_err(|_| Error::Identity)?
            .as_deref()
                == Some(conversation)
        };
        if !matches {
            continue;
        }
        if found.replace(observed).is_some() {
            return Err(Error::Identity);
        }
    }
    found.ok_or(Error::Identity)
}

pub(super) fn prepare(
    registration: &Registration,
    terminals: &Terminals,
    caller: u32,
    pane_hint: Option<&(String, String)>,
) -> Result<Option<Registration>> {
    registration.validate()?;
    if registration.scope.harness != super::hooks::Harness::Codex {
        return Err(Error::Invalid(
            "Codex binding requires a Codex registration",
        ));
    }
    let root = &terminals.proc_root;
    let Some(emitter) = ancestor(root, caller)? else {
        // Explicit bindings still pass through the normal ancestry verifier.
        return Ok(Some(registration.clone()));
    };
    let terminal = &registration.scope.terminal;
    if terminal.backend != Backend::Herdr {
        return if emitter.tty == 0 && registration.server_emitter.is_none() {
            Err(Error::Invalid(
                "Codex managed-daemon discovery requires a verified Herdr terminal client",
            ))
        } else {
            Ok(Some(registration.clone()))
        };
    }
    if emitter.tty != 0
        && pane_hint
            .is_none_or(|(pane, endpoint)| pane != &terminal.pane || endpoint != &terminal.endpoint)
    {
        return Ok(None);
    }
    let server = if emitter.tty == 0 {
        Some(pin(root, &emitter)?)
    } else {
        None
    };
    // The daemon's inherited pane hint is deliberately ignored. Query only the
    // endpoint and pane from this exact conversation's private registration.
    let started = Instant::now();
    let budget = Duration::from_millis(registration.hook_timeout_ms);
    let mut bounded = terminals.clone();
    let mut query = |args: &[&str]| {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(Error::Deadline);
        }
        bounded.timeout = terminals.timeout.min(remaining);
        bounded
            .herdr_result(&terminal.endpoint, args)
            .map_err(|_| Error::Identity)
    };
    let pane = query(&["pane", "get", &terminal.pane])?["pane"].clone();
    let info = query(&["pane", "process-info", "--pane", &terminal.pane])?["process_info"].clone();
    if pane["pane_id"] != terminal.pane || info["pane_id"] != terminal.pane {
        return Err(Error::Identity);
    }
    let generation = pane["terminal_id"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(Error::Identity)?;
    let foreground = client(root, registration, &pane, &info, &emitter)?;
    let shell_pid = info["shell_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or(Error::Identity)?;
    let shell = process(root, shell_pid)?;
    if !shell.owned() || shell.tty == 0 || shell.tty != foreground.tty {
        return Err(Error::Identity);
    }
    // An exit or PID reuse while querying the terminal cannot update a binding.
    for row in [&emitter, &foreground, &shell] {
        if process(root, row.identity.pid)? != *row {
            return Err(Error::Identity);
        }
    }
    let mut updated = registration.clone();
    updated.scope.terminal.generation = generation.into();
    updated.harness_process = Some(foreground.identity);
    updated.server_emitter = server;
    updated.validate()?;
    hook_runtime::persist_codex_binding(registration, &updated, root)?;
    Ok(Some(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        registration: Registration,
        terminals: Terminals,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "daedalus-codex-binding-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let directory = root.join("instance");
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let registration: Registration = serde_json::from_value(json!({
                "version":1,"id":"codex","directory":directory,
                "scope":{"instance":"fixture","harness":"codex","conversation":"019a043e-1234-7000-8000-000000000001",
                    "terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"old"}},
                "harness_process":{"pid":99,"start_ticks":1},"goal_id":null,"server_emitter":null,"transcript":null,
                "events":["PostToolUse"],"options":{"telegram_delivery":true,"telegram_progress":true,"notifications":false,"hindsight":false},
                "activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000
            })).unwrap();
            hook_runtime::install(&registration).unwrap();
            let proc_root = root.join("proc");
            fs::create_dir(&proc_root).unwrap();
            let script = root.join("herdr");
            let quote =
                |path: PathBuf| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
            fs::write(&script,format!("#!/bin/sh\n[ \"$1\" = --session ] && [ \"$2\" = fixture ] && [ \"$3\" = pane ] || exit 1\ncase \"$4\" in\nget) /bin/cat {};;\nprocess-info) /bin/cat {};;\n*) exit 1;;\nesac\n",quote(root.join("pane.json")),quote(root.join("info.json")))).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
            let mut f = Self {
                root,
                registration,
                terminals: Terminals {
                    herdr: script,
                    proc_root,
                    ..Terminals::default()
                },
            };
            f.write_process(501, 500, 0, &["daedalus"]);
            f.write_process(500, 1, 0, &["codex", "app-server", "--managed-daemon"]);
            f.write_process(
                400,
                300,
                70,
                &["codex", "resume", "019a043e-1234-7000-8000-000000000001"],
            );
            f.write_process(300, 1, 70, &["bash"]);
            let executable = f.root.join("codex");
            fs::write(&executable, crate::test_support::native_image(b"")).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            symlink(executable, f.terminals.proc_root.join("500/exe")).unwrap();
            f.pane(json!({"pane_id":"pane","terminal_id":"new","agent_session":{"kind":"id","value":"019a043e-1234-7000-8000-000000000001"}}));
            f.info(json!({"pane_id":"pane","shell_pid":300,"foreground_processes":[{"pid":400}]}));
            f
        }
        fn write_process(&self, pid: u32, parent: u32, tty: i64, argv: &[&str]) {
            let dir = self.terminals.proc_root.join(pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            let mut fields = vec!["0".to_string(); 20];
            fields[0] = "S".into();
            fields[1] = parent.to_string();
            fields[2] = pid.to_string();
            fields[4] = tty.to_string();
            fields[5] = pid.to_string();
            fields[19] = "42".into();
            fs::write(
                dir.join("stat"),
                format!("{pid} (fixture) {}", fields.join(" ")),
            )
            .unwrap();
            fs::write(dir.join("cmdline"), format!("{}\0", argv.join("\0"))).unwrap();
        }
        fn pane(&mut self, value: Value) {
            fs::write(
                self.root.join("pane.json"),
                json!({"pane":value}).to_string(),
            )
            .unwrap();
        }
        fn info(&mut self, value: Value) {
            fs::write(
                self.root.join("info.json"),
                json!({"process_info":value}).to_string(),
            )
            .unwrap();
        }
        fn prepare(&self) -> Result<Option<Registration>> {
            prepare(
                &self.registration,
                &self.terminals,
                501,
                Some(&("stale-pane".into(), "stale-endpoint".into())),
            )
        }
        fn bytes(&self) -> Vec<u8> {
            fs::read(self.registration.path()).unwrap()
        }
    }
    #[test]
    fn daemon_refresh_ignores_inherited_pane_and_preserves_policy_and_independent_client() {
        let f = Fixture::new();
        let updated = f.prepare().unwrap().unwrap();
        assert_eq!(updated.harness_process.as_ref().unwrap().pid, 400);
        assert_eq!(updated.server_emitter.as_ref().unwrap().process.pid, 500);
        assert_eq!(updated.scope.terminal.generation, "new");
        let mut expected = serde_json::to_value(&f.registration).unwrap();
        expected["harness_process"] = json!(updated.harness_process);
        expected["server_emitter"] = json!(updated.server_emitter);
        expected["scope"]["terminal"]["generation"] = json!("new");
        assert_eq!(serde_json::to_value(&updated).unwrap(), expected);
        assert_eq!(
            serde_json::from_slice::<Value>(&f.bytes()).unwrap(),
            expected
        );
        let db =
            rusqlite::Connection::open(f.registration.directory.join("hook-observations.sqlite3"))
                .unwrap();
        let states: Vec<String> = db
            .prepare("SELECT state FROM hook_observations")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(states, ["\"unknown\"", "\"unknown\""]);
        // A concurrent identical proof is idempotent, without repeating fences.
        let again = f.prepare().unwrap().unwrap();
        assert_eq!(serde_json::to_value(again).unwrap(), expected);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM hook_observations", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
    #[test]
    fn terminal_client_needs_unique_foreground_and_consistent_conversation() {
        for case in [
            "missing",
            "duplicate",
            "ambiguous",
            "other-session",
            "other-tty",
            "wrong-pane",
            "wrong-info-pane",
            "missing-generation",
        ] {
            let mut f = Fixture::new();
            let before = f.bytes();
            match case {
                "missing"=>f.info(json!({"pane_id":"pane","shell_pid":300,"foreground_processes":[]})),
                "duplicate"=>f.info(json!({"pane_id":"pane","shell_pid":300,"foreground_processes":[{"pid":400},{"pid":400}]})),
                "ambiguous"=>{f.write_process(401,300,70,&["codex","resume","019a043e-1234-7000-8000-000000000001"]); f.info(json!({"pane_id":"pane","shell_pid":300,"foreground_processes":[{"pid":400},{"pid":401}]}));},
                "other-session"=>f.pane(json!({"pane_id":"pane","terminal_id":"new","agent_session":{"kind":"id","value":"other"}})),
                "other-tty"=>f.write_process(300,1,71,&["bash"]),
                "wrong-pane"=>f.pane(json!({"pane_id":"other","terminal_id":"new"})),
                "wrong-info-pane"=>f.info(json!({"pane_id":"other","shell_pid":300,"foreground_processes":[{"pid":400}]})),
                "missing-generation"=>f.pane(json!({"pane_id":"pane"})), _=>unreachable!(),
            }
            assert!(f.prepare().is_err(), "{case}");
            assert_eq!(f.bytes(), before, "{case}");
            assert!(
                !f.registration
                    .directory
                    .join("hook-observations.sqlite3")
                    .exists(),
                "{case}"
            );
        }
    }
    #[test]
    fn unselected_client_needs_a_verified_resume_session() {
        let mut f = Fixture::new();
        f.pane(json!({"pane_id":"pane","terminal_id":"new"}));
        f.write_process(
            400,
            300,
            70,
            &[
                "codex",
                "resume",
                "other",
                "019a043e-1234-7000-8000-000000000001",
            ],
        );
        assert!(f.prepare().is_err());
        f.write_process(
            400,
            300,
            70,
            &["codex", "resume", "019a043e-1234-7000-8000-000000000001"],
        );
        assert!(f.prepare().unwrap().is_some());
    }
    #[test]
    fn selected_conversation_cannot_override_a_conflicting_explicit_resume() {
        let f = Fixture::new();
        let before = f.bytes();
        f.write_process(
            400,
            300,
            70,
            &["codex", "resume", "019a043e-1234-7000-8000-000000000002"],
        );
        assert!(f.prepare().is_err());
        assert_eq!(f.bytes(), before);
    }
    #[test]
    fn option_prefixed_fork_never_binds_the_parent_and_names_need_native_selection() {
        let mut f = Fixture::new();
        let before = f.bytes();
        f.write_process(
            400,
            300,
            70,
            &[
                "codex",
                "--config=model=example",
                "fork",
                "019a043e-1234-7000-8000-000000000001",
            ],
        );
        assert!(f.prepare().is_err());
        assert_eq!(f.bytes(), before);
        f.pane(json!({"pane_id":"pane","terminal_id":"new"}));
        f.write_process(400, 300, 70, &["codex", "resume", "work"]);
        assert!(f.prepare().is_err());
        assert_eq!(f.bytes(), before);
        f.write_process(
            400,
            300,
            70,
            &[
                "codex",
                "--config=model=example",
                "resume",
                "--model=example",
                "019a043e-1234-7000-8000-000000000001",
            ],
        );
        assert!(f.prepare().unwrap().is_some());
    }
    #[test]
    fn emitter_incarnation_change_during_terminal_queries_is_rejected_before_writing() {
        let f = Fixture::new();
        let before = f.bytes();
        let stat = f.terminals.proc_root.join("500/stat");
        let changed = f.root.join("changed-stat");
        fs::write(
            &changed,
            fs::read_to_string(&stat).unwrap().replacen("42", "43", 1),
        )
        .unwrap();
        let quote = |p: &Path| format!("'{}'", p.to_str().unwrap().replace('\'', "'\\''"));
        let script = fs::read_to_string(&f.terminals.herdr).unwrap();
        let line = format!(
            "process-info) /bin/cat {};;",
            quote(&f.root.join("info.json"))
        );
        assert!(script.contains(&line));
        fs::write(
            &f.terminals.herdr,
            script.replace(
                &line,
                &format!(
                    "process-info) /bin/cat {}; /bin/cp {} {};;",
                    quote(&f.root.join("info.json")),
                    quote(&changed),
                    quote(&stat)
                ),
            ),
        )
        .unwrap();
        assert!(f.prepare().is_err());
        assert_eq!(f.bytes(), before);
        assert!(
            !f.registration
                .directory
                .join("hook-observations.sqlite3")
                .exists()
        );
    }
    #[test]
    fn detached_emitter_requires_owned_native_managed_codex() {
        for case in [
            "ordinary-server",
            "missing-managed",
            "changed-image",
            "writable-image",
            "text-image",
        ] {
            let f = Fixture::new();
            let before = f.bytes();
            match case {
                "ordinary-server" => f.write_process(500, 1, 0, &["codex", "--managed-daemon"]),
                "missing-managed" => f.write_process(500, 1, 0, &["codex", "app-server"]),
                "changed-image" => {
                    fs::remove_file(f.terminals.proc_root.join("500/exe")).unwrap();
                    symlink("/bin/true", f.terminals.proc_root.join("500/exe")).unwrap();
                }
                "writable-image" => {
                    fs::set_permissions(f.root.join("codex"), fs::Permissions::from_mode(0o777))
                        .unwrap()
                }
                "text-image" => fs::write(f.root.join("codex"), b"#!/bin/sh\nexit 0\n").unwrap(),
                _ => unreachable!(),
            }
            assert!(f.prepare().is_err(), "{case}");
            assert_eq!(f.bytes(), before, "{case}");
        }
        let f = Fixture::new();
        let mut emitter = process(&f.terminals.proc_root, 500).unwrap();
        emitter.uid = emitter.uid.wrapping_add(1);
        assert!(pin(&f.terminals.proc_root, &emitter).is_err());
    }
    #[test]
    fn direct_client_filters_other_panes_and_clears_stale_server_binding() {
        let mut f = Fixture::new();
        f.registration.server_emitter = Some(
            pin(
                &f.terminals.proc_root,
                &process(&f.terminals.proc_root, 500).unwrap(),
            )
            .unwrap(),
        );
        crate::core::layouts::write_private(
            &f.registration.path(),
            &serde_json::to_vec(&f.registration).unwrap(),
        )
        .unwrap();
        let before = f.bytes();
        f.write_process(501, 400, 0, &["daedalus"]);
        assert!(f.prepare().unwrap().is_none());
        assert_eq!(f.bytes(), before);
        let updated = prepare(
            &f.registration,
            &f.terminals,
            501,
            Some(&("pane".into(), "fixture".into())),
        )
        .unwrap()
        .unwrap();
        assert_eq!(updated.harness_process.unwrap().pid, 400);
        assert!(updated.server_emitter.is_none());
        assert!(
            hook_runtime::inspect(&f.registration.path())
                .unwrap()
                .server_emitter
                .is_none()
        );
    }
    #[test]
    fn concurrent_policy_change_is_never_overwritten_by_binding_refresh() {
        let f = Fixture::new();
        let mut changed = f.registration.clone();
        changed.options.telegram_delivery = false;
        crate::core::layouts::write_private(
            &changed.path(),
            &serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        let before = f.bytes();
        assert!(f.prepare().is_err());
        assert_eq!(f.bytes(), before);
        assert!(
            !f.registration
                .directory
                .join("hook-observations.sqlite3")
                .exists()
        );
    }
    #[test]
    fn malformed_or_cyclic_process_ancestry_is_bounded() {
        let f = Fixture::new();
        f.write_process(501, 501, 0, &["not-codex"]);
        assert!(f.prepare().is_err());
        fs::write(f.terminals.proc_root.join("501/stat"), b"bad").unwrap();
        assert!(f.prepare().is_err());
        fs::write(f.terminals.proc_root.join("501/stat"), vec![b'x'; 8193]).unwrap();
        assert!(f.prepare().is_err());
    }
}
