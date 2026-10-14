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
    collect_with_socket(
        config,
        terminals,
        socket_roots,
        std::env::var("TMUX").ok().as_deref(),
    )
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

fn collect_with_socket(
    config: &ControlConfig,
    terminals: &Terminals,
    socket_roots: &[PathBuf],
    current_tmux: Option<&str>,
) -> Value {
    let mut snapshot = json!({"panes":[], "clients":[], "workspaces":[], "tabs":[], "layouts":[], "herdr_sessions":[], "tmux_sessions":[], "herdr_focus":[], "errors":[], "unavailable":[], "stale_sockets":[]});
    let sockets = socket_candidates(config, current_tmux);
    let roots = if config.discovery.automatic {
        socket_roots
    } else {
        &[]
    };
    let sockets = terminals::discover_sockets(&sockets, roots, unsafe { libc::geteuid() });
    let mut jobs: Vec<Endpoint> = sockets
        .into_iter()
        .filter_map(|p| p.to_str().map(|s| Endpoint::Tmux(s.into())))
        .collect();
    let mut sessions: BTreeSet<String> = config.discovery.herdr_sessions.iter().cloned().collect();
    if config.discovery.automatic {
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
    #[test]
    fn active_custom_socket_is_included_only_with_automatic_discovery() {
        let mut config = ControlConfig::fresh(std::path::Path::new("/tmp/project")).unwrap();
        assert_eq!(
            socket_candidates(&config, Some("/custom/server.sock,123,0")),
            [PathBuf::from("/custom/server.sock")]
        );
        config.discovery.automatic = false;
        assert!(socket_candidates(&config, Some("/custom/server.sock,123,0")).is_empty());
        config.discovery.automatic = true;
        assert!(socket_candidates(&config, Some("relative,123,0")).is_empty());
    }
    #[test]
    fn disabled_discovery_does_not_probe_unconfigured_endpoints() {
        let mut config = ControlConfig::fresh(std::path::Path::new("/tmp/project")).unwrap();
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
        let mut config = ControlConfig::fresh(std::path::Path::new("/tmp/project")).unwrap();
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
