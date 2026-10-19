//! One bounded discovery pass shared by local interfaces.
use crate::{
    adapters::terminals::{self, Terminals},
    app::config::ControlConfig,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf, thread};

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
    let mut roots = vec![PathBuf::from("/tmp")];
    if let Some(root) = std::env::var_os("TMUX_TMPDIR") {
        roots.push(root.into());
    }
    let result = collect(&config, &Terminals::default(), &roots);
    serde_json::to_writer(&mut *output, &result).map_err(|_| "Cannot write inventory")?;
    writeln!(output).map_err(|_| "Cannot write inventory".into())
}

#[derive(Clone)]
enum Endpoint {
    Tmux(String),
    Herdr(String),
}

pub fn collect(config: &ControlConfig, terminals: &Terminals, socket_roots: &[PathBuf]) -> Value {
    let (native, warnings) = if config.discovery.automatic {
        match super::native_terminal::discover() {
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
    let mut snapshot = collect_with_socket(
        config,
        terminals,
        socket_roots,
        std::env::var("TMUX").ok().as_deref(),
        &native,
    );
    for error in warnings {
        snapshot["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!({"backend":"native", "error":error}));
    }
    if config.discovery.automatic {
        match crate::adapters::standalone::collect(
            &terminals.proc_root,
            snapshot["panes"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ) {
            Ok(rows) => snapshot["external_terminals"] = rows.into(),
            Err(error) => snapshot["external_errors"] = json!([error]),
        }
    }
    snapshot
}

fn socket_candidates(config: &ControlConfig, current_tmux: Option<&str>) -> Vec<PathBuf> {
    let mut sockets: Vec<PathBuf> = config.tmux_sockets.iter().map(PathBuf::from).collect();
    if config.discovery.automatic
        && let Some(socket) = current_tmux.and_then(|value| value.split(',').next())
        && std::path::Path::new(socket).is_absolute()
        && !socket.chars().any(char::is_control)
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
        matches!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
        )
    }
}

fn collect_with_socket(
    config: &ControlConfig,
    terminals: &Terminals,
    socket_roots: &[PathBuf],
    current_tmux: Option<&str>,
    native_panes: &[Value],
) -> Value {
    let mut snapshot = json!({"panes":[], "clients":[], "workspaces":[], "tabs":[], "layouts":[], "herdr_sessions":[], "tmux_sessions":[], "herdr_focus":[], "errors":[], "unavailable":[], "stale_sockets":[]});
    if config.discovery.automatic {
        snapshot["panes"] = native_panes.to_vec().into();
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
    let sockets = terminals::discover_sockets(&sockets, roots, uid);
    let mut jobs: Vec<Endpoint> = sockets
        .into_iter()
        .filter_map(|p| p.to_str().map(|s| Endpoint::Tmux(s.into())))
        .collect();
    let mut sessions: BTreeSet<String> = config.discovery.herdr_sessions.iter().cloned().collect();
    if config.discovery.automatic && terminals::binary_available(&terminals.herdr) {
        match terminals.herdr_sessions() {
            Ok(discovered) => sessions.extend(discovered),
            Err(error) => snapshot["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!({"backend":"herdr", "error":error})),
        }
    }
    jobs.extend(sessions.into_iter().map(Endpoint::Herdr));
    // At most eight owned queries; joining also prevents detached discovery
    // workers from surviving the request that created them.
    for chunk in jobs.chunks(8) {
        thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|endpoint| {
                    (
                        endpoint,
                        scope.spawn(move || match endpoint {
                            Endpoint::Tmux(socket) => terminals.tmux_inventory(socket),
                            Endpoint::Herdr(server) => terminals.herdr_inventory(server),
                        }),
                    )
                })
                .collect();
            for (endpoint, handle) in handles {
                let result = handle
                    .join()
                    .unwrap_or_else(|_| Err("Discovery worker failed".into()));
                match result {
                    Ok(data) => {
                        for key in [
                            "panes",
                            "clients",
                            "workspaces",
                            "tabs",
                            "layouts",
                            "herdr_focus",
                        ] {
                            if let Some(rows) = data[key].as_array() {
                                snapshot[key]
                                    .as_array_mut()
                                    .unwrap()
                                    .extend(rows.iter().cloned());
                            }
                        }
                        match endpoint {
                            Endpoint::Herdr(server) => snapshot["herdr_sessions"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!(server)),
                            Endpoint::Tmux(socket) => {
                                let names: BTreeSet<_> = data["panes"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|p| p["session"].as_str())
                                    .collect();
                                for name in names {
                                    snapshot["tmux_sessions"]
                                        .as_array_mut()
                                        .unwrap()
                                        .push(json!({"socket":socket,"session":name}));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        #[cfg(unix)]
                        if let Endpoint::Tmux(socket) = endpoint
                            && stale_tmux_socket(socket)
                        {
                            snapshot["stale_sockets"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!({"backend":"tmux","endpoint":socket,"error":error}));
                            continue;
                        }
                        let (backend, endpoint) = match endpoint {
                            Endpoint::Tmux(s) => ("tmux", s),
                            Endpoint::Herdr(s) => ("herdr", s),
                        };
                        snapshot["errors"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!({"backend":backend,"endpoint":endpoint,"error":error}));
                    }
                }
            }
        });
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
    fn stale_socket_does_not_hide_live_query_failure_or_delete_endpoints() {
        use std::os::unix::net::UnixListener;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
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
        // /bin/false is a local deterministic fixture, never a terminal query.
        let root = std::env::temp_dir().join("daedalus-uninstalled-backend-fixture");
        let config = ControlConfig::fresh(&root).unwrap();
        let terminals = Terminals {
            herdr: "/bin/false".into(),
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
