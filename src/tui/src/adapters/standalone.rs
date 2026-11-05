//! Display-only discovery of terminals not owned by an integrated backend.
//! Process evidence cannot authorize terminal input, capture, adoption or killing.
use serde_json::Value;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use serde_json::json;
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[cfg(test)]
pub fn collect(proc_root: &Path, managed: &[Value]) -> Result<Vec<Value>, String> {
    let settings = crate::app::runtime_settings::Settings::default();
    let budget = crate::app::discovery_budget::Budget::new(&settings)?;
    collect_with(proc_root, managed, &settings, &budget)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug)]
struct Process {
    pid: u32,
    parent: u32,
    start: u64,
    tty: i64,
    tty_path: Option<String>,
    group: i64,
    foreground_group: i64,
    name: String,
    executable: Option<String>,
    cwd: Option<String>,
    state: char,
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn pid(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|p| *p > 0)
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn managed_pids(rows: &[Value]) -> BTreeSet<u32> {
    rows.iter()
        .flat_map(|row| {
            [
                pid(&row["pid"]),
                pid(&row["shell_pid"]),
                pid(&row["pane_pid"]),
                pid(&row["process_info"]["shell_pid"]),
            ]
        })
        .flatten()
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn excluded(mut current: u32, parents: &BTreeMap<u32, u32>, managed: &BTreeSet<u32>) -> bool {
    let mut visited = BTreeSet::new();
    for _ in 0..256 {
        if managed.contains(&current) {
            return true;
        }
        if !visited.insert(current) {
            return true;
        }
        let Some(&parent) = parents.get(&current) else {
            return false;
        };
        if parent == 0 {
            return false;
        }
        current = parent;
    }
    // An incomplete or cyclic ancestry is not evidence of a standalone terminal.
    true
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn is_shell(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().trim_end_matches(".exe"),
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "csh"
            | "tcsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
            | "elvish"
            | "xonsh"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn project_with(
    processes: Vec<Process>,
    managed: &[Value],
    deadline: std::time::Instant,
    budget: &crate::app::discovery_budget::Budget,
) -> Result<Vec<Value>, String> {
    let parents = processes.iter().map(|p| (p.pid, p.parent)).collect();
    let managed_pids = managed_pids(managed);
    let explicit_ttys: BTreeSet<_> = managed.iter().filter_map(|p| p["tty"].as_str()).collect();
    let managed_ttys: BTreeSet<_> = processes
        .iter()
        .filter(|p| {
            p.tty != 0
                && (excluded(p.pid, &parents, &managed_pids)
                    || p.tty_path
                        .as_deref()
                        .is_some_and(|tty| explicit_ttys.contains(tty)))
        })
        .map(|p| p.tty)
        .collect();
    if std::time::Instant::now() >= deadline {
        return Err("External process discovery deadline reached".into());
    }
    let mut groups: BTreeMap<i64, Vec<&Process>> = BTreeMap::new();
    for process in &processes {
        if std::time::Instant::now() >= deadline {
            return Err("External process discovery deadline reached".into());
        }
        if process.tty != 0
            && !managed_ttys.contains(&process.tty)
            && !matches!(process.state, 'Z' | 'X' | 'x')
        {
            groups.entry(process.tty).or_default().push(process);
        }
    }
    let mut result = Vec::new();
    for mut group in groups.into_values() {
        if std::time::Instant::now() >= deadline {
            return Err("External process discovery deadline reached".into());
        }
        group.sort_by_key(|p| (p.start, p.pid));
        let root = group[0];
        let foreground = group
            .iter()
            .filter(|p| p.group == root.foreground_group)
            .min_by_key(|p| (p.start, p.pid));
        let names: BTreeSet<_> = group.iter().map(|p| p.name.as_str()).collect();
        let row = json!({
            "id": format!("external:{}:{}", root.pid, root.start),
            "backend": "external", "kind": if is_shell(&root.name) { "shell" } else { "terminal" }, "pid": root.pid,
            "parent_pid": root.parent, "start_time": root.start,
            "name": root.name, "executable": root.executable, "cwd": root.cwd,
            "tty": root.tty_path.clone().unwrap_or_else(|| root.tty.to_string()), "tty_number": root.tty, "foreground": foreground.map(|p| &p.name),
            "processes": names, "process_count": group.len(),
            "state": match root.state { 'R' => "running", 'T' | 't' => "stopped", _ => "waiting" },
            "controllable": false, "capture_available": false,
            "detail": "External terminal · process information only"
        });
        if budget.admit_value(&row).is_err() {
            break;
        }
        result.push(row);
    }
    Ok(result)
}
#[cfg(all(test, target_os = "linux"))]
fn project(processes: Vec<Process>, managed: &[Value]) -> Vec<Value> {
    let settings = crate::app::runtime_settings::Settings::default();
    let budget = crate::app::discovery_budget::Budget::new(&settings).unwrap();
    project_with(processes, managed, budget.deadline, &budget).unwrap()
}

#[cfg(target_os = "linux")]
fn parse_stat(text: &str) -> Option<Process> {
    let (pid_text, rest) = text.split_once(" (")?;
    let end = rest.rfind(") ")?;
    let name = clean(&rest[..end]);
    let fields: Vec<_> = rest[end + 2..].split_whitespace().collect();
    Some(Process {
        pid: pid_text.parse().ok()?,
        name,
        state: fields.first()?.chars().next()?,
        parent: fields.get(1)?.parse().ok()?,
        group: fields.get(2)?.parse().ok()?,
        tty: fields.get(4)?.parse().ok()?,
        foreground_group: fields.get(5)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
        tty_path: None,
        executable: None,
        cwd: None,
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn collect_with(
    proc_root: &Path,
    managed: &[Value],
    settings: &crate::app::runtime_settings::Settings,
    budget: &crate::app::discovery_budget::Budget,
) -> Result<Vec<Value>, String> {
    use std::{
        fs,
        io::Read,
        os::unix::fs::MetadataExt,
        time::{Duration, Instant},
    };
    let deadline = budget
        .deadline
        .min(Instant::now() + Duration::from_millis(settings.external_discovery_timeout_ms));
    let entries =
        fs::read_dir(proc_root).map_err(|_| "Cannot enumerate local terminal processes")?;
    let uid = unsafe { libc::geteuid() };
    let mut processes = Vec::new();
    for (count, entry) in entries.enumerate() {
        if count >= settings.external_discovery_entry_limit as usize || Instant::now() >= deadline {
            return Err("External process discovery entry limit or deadline reached".into());
        }
        let entry = entry.map_err(|_| "Process discovery changed during enumeration")?;
        let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let directory = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&directory) else {
            continue;
        };
        if !metadata.is_dir() || metadata.uid() != uid {
            continue;
        }
        let mut text = String::new();
        let Ok(file) = fs::File::open(directory.join("stat")) else {
            continue;
        };
        if file.take(8192).read_to_string(&mut text).is_err() {
            continue;
        }
        let Some(mut process) = parse_stat(&text).filter(|p| p.pid == number) else {
            continue;
        };
        if process.tty != 0 {
            process.tty_path = ["fd/0", "fd/1", "fd/2"].into_iter().find_map(|fd| {
                let path = fs::read_link(directory.join(fd)).ok()?;
                let text = path.to_str()?;
                (text.starts_with("/dev/pts/") || text.starts_with("/dev/tty")).then(|| clean(text))
            });
            process.executable = fs::read_link(directory.join("exe"))
                .ok()
                .map(|p| clean(&p.to_string_lossy()));
            process.cwd = fs::read_link(directory.join("cwd"))
                .ok()
                .map(|p| clean(&p.to_string_lossy()));
            // Reject PID reuse between the identity and display-field observations.
            let Ok(file) = fs::File::open(directory.join("stat")) else {
                continue;
            };
            let mut current = String::new();
            if file.take(8192).read_to_string(&mut current).is_err() {
                continue;
            }
            if fs::symlink_metadata(&directory)
                .ok()
                .is_none_or(|meta| meta.uid() != uid)
            {
                continue;
            }
            if parse_stat(&current).is_none_or(|p| p.start != process.start || p.tty != process.tty)
            {
                continue;
            }
        }
        processes.push(process);
    }
    if Instant::now() >= deadline {
        return Err("External process discovery deadline reached".into());
    }
    project_with(processes, managed, deadline, budget)
}

/// macOS has no procfs: `libproc` supplies the same identity fields. Only
/// processes owned by this user are observed; PID reuse between the identity
/// and display observations is rejected exactly as on Linux.
#[cfg(target_os = "macos")]
pub(crate) fn collect_with(
    proc_root: &Path,
    managed: &[Value],
    settings: &crate::app::runtime_settings::Settings,
    budget: &crate::app::discovery_budget::Budget,
) -> Result<Vec<Value>, String> {
    use crate::platform::{process_cwd, process_info, process_list, process_name};
    use std::time::{Duration, Instant};
    let deadline = budget
        .deadline
        .min(Instant::now() + Duration::from_millis(settings.external_discovery_timeout_ms));
    let pids = process_list(proc_root);
    if pids.is_empty() {
        return Err("Cannot enumerate local terminal processes".into());
    }
    let ttys = macos_tty_paths(settings.external_discovery_entry_limit as usize);
    let uid = unsafe { libc::geteuid() };
    let mut processes = Vec::new();
    for (count, number) in pids.into_iter().enumerate() {
        if count >= settings.external_discovery_entry_limit as usize || Instant::now() >= deadline {
            return Err("External process discovery entry limit or deadline reached".into());
        }
        let Some(info) = process_info(proc_root, number).filter(|i| i.uid == uid) else {
            continue;
        };
        let tty = info.tty;
        let name = info
            .executable
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| process_name(number))
            .unwrap_or_default();
        let mut process = Process {
            pid: number,
            parent: info.parent,
            start: info.start,
            tty,
            tty_path: None,
            group: i64::from(info.pgid),
            foreground_group: info.foreground,
            name: clean(&name),
            executable: None,
            cwd: None,
            state: info.state,
        };
        if tty != 0 {
            process.tty_path = ttys.get(&tty).cloned();
            process.executable = info.executable.map(|p| clean(&p.to_string_lossy()));
            process.cwd = process_cwd(number).map(|p| clean(&p.to_string_lossy()));
            let current = process_info(proc_root, number);
            if current.is_none_or(|c| c.uid != uid || c.start != info.start || c.tty != info.tty) {
                continue;
            }
        }
        processes.push(process);
    }
    if Instant::now() >= deadline {
        return Err("External process discovery deadline reached".into());
    }
    project_with(processes, managed, deadline, budget)
}

/// Map character-device numbers of `/dev/tty*` nodes to their paths.
#[cfg(target_os = "macos")]
fn macos_tty_paths(limit: usize) -> BTreeMap<i64, String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let mut map = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return map;
    };
    for entry in entries.flatten().take(limit) {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|n| n.starts_with("tty")) else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.file_type().is_char_device() {
            map.insert(meta.rdev() as i64, format!("/dev/{}", clean(name)));
        }
    }
    map
}

#[cfg(windows)]
pub(crate) fn collect_with(
    _proc_root: &Path,
    managed: &[Value],
    settings: &crate::app::runtime_settings::Settings,
    budget: &crate::app::discovery_budget::Budget,
) -> Result<Vec<Value>, String> {
    let deadline = budget.deadline.min(
        std::time::Instant::now()
            + std::time::Duration::from_millis(settings.external_discovery_timeout_ms),
    );
    let snapshot = crate::platform::terminal_processes(
        settings.external_discovery_entry_limit as usize,
        settings.external_discovery_query_limit as usize,
        deadline,
    )
    .map_err(|_| "Cannot enumerate owned Windows shell processes")?;
    let managed = managed_pids(managed);
    let mut rows = Vec::new();
    for p in snapshot
        .processes
        .into_iter()
        .filter(|p| !excluded(p.pid, &snapshot.parents, &managed))
    {
        if std::time::Instant::now() >= deadline {
            return Err("External process discovery deadline reached".into());
        }
        let name = clean(
            &p.executable
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        );
        let row = json!({"id": format!("external:{}:{}", p.pid, p.creation_time),
            "backend": "external", "kind": if is_shell(&name) { "shell" } else { "terminal_host" },
            "pid": p.pid, "parent_pid": p.parent_pid, "start_time": p.creation_time,
            "name": name, "executable": clean(&p.executable.to_string_lossy()),
            "cwd": null, "tty": null, "state": "running", "controllable": false,
            "capture_available": false, "detail": "External process · window association unavailable"});
        if budget.admit_value(&row).is_err() {
            break;
        }
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn collect_with(
    _proc_root: &Path,
    _managed: &[Value],
    _settings: &crate::app::runtime_settings::Settings,
    _budget: &crate::app::discovery_budget::Budget,
) -> Result<Vec<Value>, String> {
    Err("External terminal process discovery is available on Linux, macOS and Windows".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    fn stat(pid: u32, parent: u32, tty: i64, start: u64, name: &str) -> String {
        format!(
            "{pid} ({name}) S {parent} {pid} {pid} {tty} {pid} 0 0 0 0 0 0 0 0 0 0 0 0 0 {start} 0"
        )
    }
    fn process(pid: u32, parent: u32, tty: i64, start: u64, name: &str) -> Process {
        parse_stat(&stat(pid, parent, tty, start, name)).unwrap()
    }
    #[test]
    fn parses_names_with_parentheses_and_preserves_identity() {
        let p = process(10, 1, 34819, 1234, "odd ) shell\u{1b}");
        assert_eq!((p.pid, p.parent, p.tty, p.start), (10, 1, 34819, 1234));
        assert_eq!(p.name, "odd ) shell");
        assert!(parse_stat("broken").is_none());
    }
    #[test]
    fn one_row_per_terminal_excludes_managed_descendants_and_background_processes() {
        let rows = project(
            vec![
                process(10, 1, 10, 100, "bash"),
                process(11, 10, 10, 101, "vim"),
                process(20, 1, 20, 200, "bash"),
                process(21, 20, 21, 201, "zsh"),
                process(30, 1, 0, 300, "bash"),
                process(40, 1, 40, 400, "fish"),
            ],
            &[json!({"pid":"20"})],
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "external:10:100");
        assert_eq!(rows[0]["process_count"], 2);
        assert_eq!(rows[0]["controllable"], false);
        assert_eq!(rows[1]["name"], "fish");
    }
    #[test]
    fn terminal_identity_does_not_change_when_program_starts_a_child_shell() {
        let original = project(vec![process(10, 1, 20, 40, "editor")], &[]);
        let child = project(
            vec![
                process(10, 1, 20, 40, "editor"),
                process(11, 10, 20, 50, "bash"),
            ],
            &[],
        );
        assert_eq!(original[0]["id"], child[0]["id"]);
        assert_eq!(child[0]["process_count"], 2);
    }
    #[test]
    fn managed_tty_without_pid_is_not_duplicated_and_dead_processes_are_ignored() {
        let mut p = process(10, 1, 20, 40, "bash");
        p.tty_path = Some("/dev/pts/4".into());
        let mut dead = process(11, 1, 21, 41, "bash");
        dead.state = 'Z';
        assert!(project(vec![p, dead], &[json!({"tty":"/dev/pts/4"})]).is_empty());
    }
    #[test]
    fn unavailable_proc_is_an_error_not_an_empty_success() {
        let path = std::env::temp_dir().join(format!("dd-no-proc-{}", std::process::id()));
        assert!(collect(&path, &[]).is_err());
    }
    #[test]
    fn fake_proc_is_global_without_reading_arguments_or_environment() {
        use std::{fs, os::unix::fs::symlink};
        let root = std::env::temp_dir().join(format!(
            "dd-proc-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        fs::create_dir(&root).unwrap();
        for (pid, tty) in [(400, 10), (500, 20)] {
            let directory = root.join(pid.to_string());
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("stat"), stat(pid, 1, tty, 1000, "bash")).unwrap();
            symlink("/different/workspace", directory.join("cwd")).unwrap();
            fs::write(directory.join("cmdline"), "secret argument").unwrap();
            fs::write(directory.join("environ"), "SECRET=private").unwrap();
        }
        let rows = collect(&root, &[]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["cwd"], "/different/workspace");
        assert!(!serde_json::to_string(&rows).unwrap().contains("secret"));
        let mut settings = crate::app::runtime_settings::Settings {
            external_discovery_entry_limit: 1,
            ..Default::default()
        };
        let budget = crate::app::discovery_budget::Budget::new(&settings).unwrap();
        assert!(
            collect_with(&root, &[], &settings, &budget)
                .unwrap_err()
                .contains("entry limit")
        );
        settings.external_discovery_entry_limit = 2;
        let mut budget = crate::app::discovery_budget::Budget::new(&settings).unwrap();
        assert_eq!(
            collect_with(&root, &[], &settings, &budget).unwrap().len(),
            2
        );
        budget.deadline = std::time::Instant::now() - std::time::Duration::from_millis(1);
        assert!(
            collect_with(&root, &[], &settings, &budget)
                .unwrap_err()
                .contains("deadline")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn changed_incarnation_changes_id_and_cycles_are_not_standalone() {
        let old = project(vec![process(10, 1, 20, 40, "sh")], &[]);
        let new = project(vec![process(10, 1, 20, 41, "sh")], &[]);
        assert_ne!(old[0]["id"], new[0]["id"]);
        assert!(
            project(
                vec![process(10, 11, 20, 40, "sh"), process(11, 10, 20, 41, "sh")],
                &[]
            )
            .is_empty()
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    #[test]
    fn enumerates_owned_processes_and_excludes_managed_ancestry() {
        let me = std::process::id();
        let rows = collect(Path::new("/proc"), &[json!({"pid": me})]).unwrap();
        for row in &rows {
            assert_eq!(row["backend"], "external");
            assert_ne!(row["pid"], json!(me));
            assert_eq!(row["controllable"], false);
            assert!(
                row["tty"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("/dev/tty"))
            );
        }
    }
    #[test]
    fn tty_paths_map_character_devices() {
        let map = macos_tty_paths(4096);
        assert!(map.values().all(|p| p.starts_with("/dev/tty")));
        assert!(!map.is_empty());
    }
}
