//! One bounded discovery pass shared by local interfaces.
use super::discovery_budget::Budget;
use crate::{
    adapters::terminals::{self, Terminals},
    app::config::ControlConfig,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, sync::Arc, thread};

pub fn run(args: &[String], output: &mut impl std::io::Write) -> Result<(), String> {
    if args == ["--help"] || args == ["-h"] {
        return writeln!(output, "Usage: daedalus inventory --config-dir ABS --json\nReads configured terminal endpoints; does not capture terminal contents.")
            .map_err(|_| "Cannot write inventory help".into());
    }
    let mut directory = None;
    let mut json_output = false;
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config-dir" if directory.is_none() => {
                directory = Some(PathBuf::from(
                    arguments.next().ok_or("Missing configuration directory")?,
                ));
            }
            "--json" if !json_output => json_output = true,
            _ => return Err("Invalid inventory option; use inventory --help".into()),
        }
    }
    if !json_output {
        return Err("Use --json for native inventory output".into());
    }
    let directory = directory.ok_or("Missing --config-dir")?;
    let bytes = crate::core::layouts::read_private(&directory.join("control.json"))?
        .ok_or("Configuration missing; run config init or config import")?;
    let config = ControlConfig::parse(&bytes)?;
    let mut adapter = Terminals::default().configured(&config.runtime)?;
    let budget = Arc::new(Budget::new(&config.runtime)?);
    adapter.discovery_budget = Some(budget.clone());
    let mut result = collect_with_budget(&config, &adapter, &socket_roots(), &budget);
    if super::session_actions::enrich_sessions(&directory, &mut result, &adapter).is_err() {
        result["errors"].as_array_mut().expect("inventory errors").push(json!({"backend":"sessions", "error":"Some harness session metadata could not be verified"}));
    }
    budget.finish(&mut result);
    serde_json::to_writer(&mut *output, &result).map_err(|_| "Cannot write inventory")?;
    writeln!(output).map_err(|_| "Cannot write inventory".into())
}

pub fn socket_roots() -> Vec<PathBuf> {
    let candidates = std::iter::once(PathBuf::from("/tmp")).chain(
        ["TMUX_TMPDIR", "XDG_RUNTIME_DIR"]
            .into_iter()
            .filter_map(|key| std::env::var_os(key).map(PathBuf::from)),
    );
    canonical_roots(candidates)
}

/// tmux reports its socket through the resolved directory (macOS `/tmp` is
/// `/private/tmp`). Scanning the same resolved path keeps one server one
/// endpoint, so the dashboard's own tmux terminal is still recognized.
fn canonical_roots(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in candidates.into_iter().filter(|root| root.is_absolute()) {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

#[derive(Clone)]
enum Endpoint {
    Tmux(String),
    Herdr(String),
}

pub fn collect(config: &ControlConfig, terminals: &Terminals, socket_roots: &[PathBuf]) -> Value {
    let budget = match Budget::new(&config.runtime) {
        Ok(budget) => Arc::new(budget),
        Err(_) => {
            return json!({"panes":[],"clients":[],"errors":["Invalid local runtime settings"]});
        }
    };
    collect_with_budget(config, terminals, socket_roots, &budget)
}
pub(crate) fn collect_with_budget(
    config: &ControlConfig,
    terminals: &Terminals,
    socket_roots: &[PathBuf],
    budget: &Arc<Budget>,
) -> Value {
    let mut terminals = terminals.clone();
    if terminals.apply_settings(&config.runtime).is_err() {
        return json!({"panes":[],"clients":[],"errors":["Invalid local runtime settings"]});
    }
    terminals.discovery_budget = Some(budget.clone());
    let terminals = &terminals;
    let (native, warnings) = if config.discovery.native_enabled() {
        match super::native_terminal::discover_with(&config.runtime, budget) {
            Ok((sessions, warnings)) => (
                sessions
                    .iter()
                    .map(crate::adapters::native::pane)
                    .collect::<Vec<_>>(),
                warnings,
            ),
            Err(error) => (Vec::new(), vec![error]),
        }
    } else {
        (Vec::new(), Vec::new())
    };
    let mut snapshot = collect_scoped(
        config,
        terminals,
        socket_roots,
        std::env::var("TMUX").ok().as_deref(),
        &native,
        budget,
    );
    for error in warnings {
        budget.error(&mut snapshot, json!({"backend":"native", "error":error}));
    }
    if config.discovery.automatic {
        match crate::adapters::standalone::collect_with(
            &terminals.proc_root,
            snapshot["panes"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            &config.runtime,
            budget,
        ) {
            Ok(rows) => snapshot["external_terminals"] = rows.into(),
            Err(error) => {
                budget.error(&mut snapshot, json!({"backend":"external", "error":error}));
                if budget.admit_value(&json!(error)).is_ok() {
                    snapshot["external_errors"] = json!([error]);
                }
            }
        }
    }
    budget.finish(&mut snapshot);
    snapshot
}

/// Workers take the next available endpoint, while results keep discovery order.
/// Every worker is joined; a failed endpoint never abandons the remaining queue.
fn query_endpoints(
    jobs: &[Endpoint],
    workers: usize,
    query: impl Fn(&Endpoint) -> Result<Value, String> + Sync,
) -> Vec<Result<Value, String>> {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let next = AtomicUsize::new(0);
    let results = Mutex::new(vec![None; jobs.len()]);
    thread::scope(|scope| {
        for _ in 0..jobs.len().min(workers.max(1)) {
            let next = &next;
            let results = &results;
            let query = &query;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(endpoint) = jobs.get(index) else {
                        break;
                    };
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| query(endpoint)))
                            .unwrap_or_else(|_| Err("Discovery worker failed".into()));
                    results.lock().expect("discovery results lock")[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .expect("discovery results lock")
        .into_iter()
        .map(|row| row.unwrap_or_else(|| Err("Discovery worker failed".into())))
        .collect()
}

fn socket_candidates(config: &ControlConfig, current_tmux: Option<&str>) -> Vec<PathBuf> {
    let mut sockets: Vec<PathBuf> = config.tmux_sockets.iter().map(PathBuf::from).collect();
    if config.discovery.automatic
        && let Some(socket) = current_tmux.and_then(terminals::active_tmux_socket)
    {
        sockets.push(socket.into());
    }
    sockets
}

/// Only a definite dead Unix endpoint may be removed from completeness errors.
/// A nonblocking connect keeps a full listener backlog from hanging discovery.
#[cfg(unix)]
fn stale_tmux_socket(path: &str) -> bool {
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    };
    let bytes = std::path::Path::new(path).as_os_str().as_bytes();
    unsafe {
        let mut address: libc::sockaddr_un = std::mem::zeroed();
        if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
            return false;
        }
        let descriptor = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if descriptor < 0 {
            return false;
        }
        let socket = OwnedFd::from_raw_fd(descriptor);
        if libc::fcntl(socket.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) < 0
            || libc::fcntl(socket.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) < 0
        {
            return false;
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (target, &byte) in address.sun_path.iter_mut().zip(bytes) {
            *target = byte as libc::c_char;
        }
        let length = std::mem::size_of_val(&address) as libc::socklen_t;
        #[cfg(target_os = "macos")]
        {
            address.sun_len = length as u8;
        }
        if libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length,
        ) == 0
        {
            return false;
        }
        let error = std::io::Error::last_os_error();
        // macOS reports ECONNREFUSED both for a dropped listener and for a live
        // listener whose backlog is full. Only a missing file is unambiguous.
        #[cfg(target_os = "linux")]
        {
            matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            error.kind() == std::io::ErrorKind::NotFound
        }
    }
}

#[cfg(test)]
fn collect_with_socket(
    config: &ControlConfig,
    terminals: &Terminals,
    roots: &[PathBuf],
    current: Option<&str>,
    native: &[Value],
) -> Value {
    let budget = match Budget::new(&config.runtime) {
        Ok(budget) => Arc::new(budget),
        Err(error) => {
            return json!({"panes":[],"clients":[],"errors":[{"backend":"discovery","error":error}]});
        }
    };
    let mut snapshot = collect_scoped(config, terminals, roots, current, native, &budget);
    budget.finish(&mut snapshot);
    snapshot
}
fn collect_scoped(
    config: &ControlConfig,
    terminals: &Terminals,
    socket_roots: &[PathBuf],
    current_tmux: Option<&str>,
    native_panes: &[Value],
    budget: &Arc<Budget>,
) -> Value {
    let mut snapshot = json!({"panes":[], "clients":[], "workspaces":[], "tabs":[], "layouts":[], "herdr_sessions":[], "tmux_sessions":[], "herdr_focus":[], "errors":[], "unavailable":[], "stale_sockets":[]});
    let mut terminals = match terminals.configured(&config.runtime) {
        Ok(terminals) => terminals,
        Err(error) => {
            snapshot["errors"] = json!([{"backend":"discovery", "error":error}]);
            return snapshot;
        }
    };
    terminals.discovery_budget = Some(budget.clone());
    if config.discovery.native_enabled() {
        for pane in native_panes {
            if budget.admit_value(pane).is_ok() {
                snapshot["panes"].as_array_mut().unwrap().push(pane.clone());
            }
        }
    }
    let sockets = socket_candidates(config, current_tmux);
    let roots = if config.discovery.automatic {
        socket_roots
    } else {
        &[]
    };
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() };
    #[cfg(windows)]
    let uid = 0; // tmux socket discovery is Unix-only.
    let sockets = terminals::discover_sockets_with(
        &sockets,
        roots,
        uid,
        config.runtime.discovery_socket_scan_limit as usize,
        Some(budget.deadline),
    );
    for error in sockets.errors {
        budget.error(&mut snapshot, json!({"backend":"discovery", "error":error}));
    }
    let mut jobs = Vec::new();
    for path in sockets.paths {
        if let Some(path) = path.to_str() {
            jobs.push(Endpoint::Tmux(path.into()));
        } else {
            budget.error(
                &mut snapshot,
                json!({"backend":"discovery", "error":"Socket path is not UTF-8"}),
            );
        }
    }
    let mut sessions: BTreeSet<String> = config.discovery.herdr_sessions.iter().cloned().collect();
    if config.discovery.automatic && terminals::binary_available(&terminals.herdr) {
        match terminals.herdr_sessions(config.runtime.discovery_endpoint_limit as usize) {
            Ok(discovered) => sessions.extend(discovered),
            Err(error) => budget.error(&mut snapshot, json!({"backend":"herdr", "error":error})),
        }
    }
    jobs.extend(sessions.into_iter().map(Endpoint::Herdr));
    if jobs.len() > config.runtime.discovery_endpoint_limit as usize {
        jobs.truncate(config.runtime.discovery_endpoint_limit as usize);
        budget.error(&mut snapshot, json!({"backend":"discovery", "error":"Terminal endpoint limit reached; raise runtime.discovery_endpoint_limit or reduce discovery scope"}));
    }
    let results = query_endpoints(
        &jobs,
        config.runtime.discovery_workers as usize,
        |endpoint| {
            budget.time_left()?;
            let data = match endpoint {
                Endpoint::Tmux(socket) => terminals.tmux_inventory(socket),
                Endpoint::Herdr(server) => terminals.herdr_inventory(server),
            }?;
            budget.admit_value(&data)?;
            Ok(data)
        },
    );
    for (endpoint, result) in jobs.iter().zip(results) {
        match result {
            Ok(mut data) => {
                let tmux_names: BTreeSet<String> = data["panes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["session"].as_str().map(str::to_owned))
                    .collect();
                for key in [
                    "panes",
                    "clients",
                    "workspaces",
                    "tabs",
                    "layouts",
                    "herdr_focus",
                ] {
                    if let Some(Value::Array(rows)) = data.get_mut(key) {
                        snapshot[key].as_array_mut().unwrap().append(rows);
                    }
                }
                match endpoint {
                    Endpoint::Herdr(server) => {
                        let row = json!(server);
                        if budget.admit_value(&row).is_ok() {
                            snapshot["herdr_sessions"].as_array_mut().unwrap().push(row);
                        }
                    }
                    Endpoint::Tmux(socket) => {
                        for name in tmux_names {
                            let row = json!({"socket":socket,"session":name});
                            if budget.admit_value(&row).is_ok() {
                                snapshot["tmux_sessions"].as_array_mut().unwrap().push(row);
                            }
                        }
                    }
                }
            }
            Err(error) => {
                #[cfg(unix)]
                if let Endpoint::Tmux(socket) = endpoint
                    && stale_tmux_socket(socket)
                {
                    let row = json!({"backend":"tmux","endpoint":socket,"error":error});
                    if budget.admit_value(&row).is_ok() {
                        snapshot["stale_sockets"].as_array_mut().unwrap().push(row);
                    }
                    continue;
                }
                let (backend, endpoint) = match endpoint {
                    Endpoint::Tmux(s) => ("tmux", s),
                    Endpoint::Herdr(s) => ("herdr", s),
                };
                budget.error(
                    &mut snapshot,
                    json!({"backend":backend,"endpoint":endpoint,"error":error}),
                );
            }
        }
    }
    if crate::adapters::pane_observations::normalize(&mut snapshot) > 0 {
        budget.error(
            &mut snapshot,
            json!({"backend":"discovery","error":crate::adapters::pane_observations::WARNING}),
        );
    }
    snapshot["external_terminals"] = json!([]);
    snapshot["external_errors"] = json!([]);
    snapshot["observed_at"] = json!(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    );
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn socket_roots_resolve_symlinks_and_deduplicate() {
        let base = std::env::temp_dir().join(format!("dd-socket-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, base.join("link")).unwrap();
        let real = std::fs::canonicalize(&real).unwrap();
        assert_eq!(
            canonical_roots([base.join("link"), real.clone(), "relative".into()]),
            vec![real]
        );
        let missing = base.join("missing");
        assert_eq!(canonical_roots([missing.clone()]), vec![missing]);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn workers_advance_past_a_slow_endpoint_join_every_job_and_keep_result_order() {
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        use std::time::Duration;
        let jobs: Vec<_> = (0..6).map(|n| Endpoint::Herdr(n.to_string())).collect();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let calls: Vec<_> = jobs.iter().map(|_| AtomicUsize::new(0)).collect();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let started_rx = Mutex::new(started_rx);
        let release_rx = Mutex::new(release_rx);
        struct Active<'a>(&'a AtomicUsize);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let rows = query_endpoints(&jobs, 2, |endpoint| {
            let Endpoint::Herdr(n) = endpoint else {
                unreachable!()
            };
            let n: usize = n.parse().unwrap();
            calls[n].fetch_add(1, Ordering::SeqCst);
            peak.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            let _guard = Active(&active);
            match n {
                0 => {
                    started_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|_| "later endpoint was blocked behind the first batch")?;
                }
                2 => {
                    started_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|_| "first endpoint never started")?;
                    release_tx.send(()).unwrap();
                }
                3 => return Err("endpoint refused".into()),
                4 => panic!("isolated worker fixture"),
                _ => {}
            }
            Ok(json!(n))
        });
        assert_eq!(
            rows,
            vec![
                Ok(json!(0)),
                Ok(json!(1)),
                Ok(json!(2)),
                Err("endpoint refused".into()),
                Err("Discovery worker failed".into()),
                Ok(json!(5))
            ]
        );
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert!(calls.iter().all(|n| n.load(Ordering::SeqCst) == 1));
        assert!(query_endpoints(&[], 2, |_| unreachable!()).is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn unreadable_socket_scan_is_reported_as_incomplete() {
        let root = std::env::temp_dir().join(format!(
            "dd-inventory-scan-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let uid = unsafe { libc::geteuid() };
        // A regular file is deterministic even when the tests run as root.
        crate::core::layouts::write_private(&root.join(format!("tmux-{uid}")), b"not a directory")
            .unwrap();
        let config = ControlConfig::fresh(&root).unwrap();
        let terminals = Terminals {
            herdr: root.join("missing-herdr"),
            ..Terminals::default()
        };
        let snapshot = collect_with_socket(&config, &terminals, &[root], None, &[]);
        assert!(
            snapshot["errors"].as_array().unwrap().iter().any(|error| {
                error["error"]
                    .as_str()
                    .is_some_and(|text| text.contains("socket directory"))
            }),
            "a failed scan must not claim complete discovery: {snapshot}"
        );
    }
    // Linux connect(2) keeps a saturated listener live; macOS reports
    // ECONNREFUSED once the backlog fills, which is ambiguous with a stale
    // socket file. macOS therefore treats both as an error, not stale.
    #[cfg(target_os = "linux")]
    #[test]
    fn stale_socket_does_not_hide_live_query_failure_or_delete_endpoints() {
        use std::os::unix::net::UnixListener;
        let unique = crate::test_support::unique_nanos();
        let root =
            std::env::temp_dir().join(format!("dd-inventory-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let stale = root.join("stale");
        let live = root.join("live");
        drop(UnixListener::bind(&stale).unwrap());
        let listener = UnixListener::bind(&live).unwrap();
        use std::os::fd::AsRawFd;
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        let mut config = ControlConfig::fresh(&root).unwrap();
        config.discovery.automatic = false;
        config.tmux_sockets = vec![
            stale.to_string_lossy().into_owned(),
            live.to_string_lossy().into_owned(),
        ];
        let terminals = Terminals {
            tmux: root.join("missing-tmux-query"),
            ..Terminals::default()
        };
        let snapshot = collect_with_socket(&config, &terminals, &[], None, &[]);
        assert_eq!(snapshot["stale_sockets"].as_array().unwrap().len(), 1);
        assert_eq!(
            snapshot["stale_sockets"][0]["endpoint"],
            stale.to_str().unwrap()
        );
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["errors"][0]["endpoint"], live.to_str().unwrap());
        assert_eq!(snapshot["panes"], json!([]));
        assert!(stale.exists() && live.exists());
        // Saturated live listeners must remain failures, not stale endpoints.
        for _ in 0..8 {
            assert!(!stale_tmux_socket(live.to_str().unwrap()));
        }
        assert!(stale_tmux_socket(root.join("missing").to_str().unwrap()));
        assert!(!stale_tmux_socket(&"x".repeat(1024)));
    }
    #[test]
    fn absent_optional_backend_does_not_make_native_inventory_fail() {
        let root = std::env::temp_dir().join("daedalus-uninstalled-backend-fixture");
        let mut config = ControlConfig::fresh(&root).unwrap();
        let terminals = Terminals {
            herdr: root.join("missing-herdr"),
            ..Terminals::default()
        };
        let native = json!({"backend":"native", "server":"local", "pane_id":"session", "terminal_id":"generation", "shell_pid":42});
        let snapshot = collect_with_socket(
            &config,
            &terminals,
            &[],
            None,
            std::slice::from_ref(&native),
        );
        assert_eq!(snapshot["panes"], json!([native]));
        assert_eq!(snapshot["errors"], json!([]));
        assert_eq!(snapshot["external_terminals"], json!([]));
        config.discovery.automatic = false;
        assert_eq!(
            collect_with_socket(&config, &terminals, &[], None, &[native])["panes"],
            json!([])
        );
    }
    #[cfg(unix)]
    #[test]
    fn installed_optional_backend_rejection_is_still_an_error() {
        // A local deterministic fixture, never a terminal query. macOS has no
        // /bin/false; /usr/bin/false exists on both platforms.
        let unavailable = if cfg!(target_os = "macos") {
            "/usr/bin/false"
        } else {
            "/bin/false"
        };
        let root = std::env::temp_dir().join("daedalus-uninstalled-backend-fixture");
        let config = ControlConfig::fresh(&root).unwrap();
        let terminals = Terminals {
            herdr: unavailable.into(),
            ..Terminals::default()
        };
        let snapshot = collect_with_socket(&config, &terminals, &[], None, &[]);
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["errors"][0]["backend"], "herdr");
    }
    #[test]
    fn active_custom_socket_is_included_only_with_automatic_discovery() {
        let mut config = ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
            "/tmp/project",
        )))
        .unwrap();
        assert_eq!(
            socket_candidates(
                &config,
                Some(&format!(
                    "{},123,0",
                    crate::test_support::path("/custom/server.sock")
                ))
            ),
            [PathBuf::from(crate::test_support::path(
                "/custom/server.sock"
            ))]
        );
        config.discovery.automatic = false;
        assert!(socket_candidates(&config, Some("/custom/server.sock,123,0")).is_empty());
        config.discovery.automatic = true;
        assert!(socket_candidates(&config, Some("relative,123,0")).is_empty());
    }
    #[test]
    fn active_tmux_socket_keeps_commas_before_pid_and_index() {
        let config = ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
            "/tmp/project",
        )))
        .unwrap();
        let socket = crate::test_support::path("/custom/server, work.sock");
        assert_eq!(
            socket_candidates(&config, Some(&format!("{socket},123,0"))),
            [PathBuf::from(socket)]
        );
    }
    #[test]
    fn disabled_discovery_does_not_probe_unconfigured_endpoints() {
        let mut config = ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
            "/tmp/project",
        )))
        .unwrap();
        config.discovery.automatic = false;
        let terminals = Terminals {
            tmux: "/nonexistent/tmux".into(),
            herdr: "/nonexistent/herdr".into(),
            ..Terminals::default()
        };
        let snapshot = collect(&config, &terminals, &[]);
        assert_eq!(snapshot["errors"], json!([]));
        assert_eq!(snapshot["panes"], json!([]));
    }
    #[test]
    fn configured_missing_endpoint_is_an_error_not_a_phantom_terminal() {
        let mut config = ControlConfig::fresh(std::path::Path::new(crate::test_support::path(
            "/tmp/project",
        )))
        .unwrap();
        config.discovery.automatic = false;
        config.discovery.herdr_sessions.push("missing".into());
        let terminals = Terminals {
            herdr: "/nonexistent/herdr".into(),
            ..Terminals::default()
        };
        let snapshot = collect(&config, &terminals, &[]);
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["panes"], json!([]));
        assert_eq!(snapshot["herdr_sessions"], json!([]));
    }
}
