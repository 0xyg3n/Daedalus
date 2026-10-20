//! Read Hermes native live-session leases, never conversation history or titles.
use crate::core::checkpoints;
use serde_json::Value;
use std::collections::BTreeMap;
type Result<T> = std::result::Result<T, String>;

fn selected(bytes: &[u8], processes: &BTreeMap<u32, f64>) -> Result<Option<String>> {
    if bytes.len() > 1_048_576 {
        return Err("Hermes active-session registry exceeds limit".into());
    }
    let registry: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid Hermes active-session registry")?;
    let entries = registry["entries"]
        .as_array()
        .ok_or("Missing Hermes active-session entries")?;
    if entries.len() > 4096 {
        return Err("Too many Hermes active-session leases".into());
    }
    let mut matches = Vec::new();
    for entry in entries {
        let Some(expected) = entry["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .and_then(|pid| processes.get(&pid))
        else {
            continue;
        };
        let Some(started) = entry["process_start_time"].as_f64() else {
            continue;
        };
        if !started.is_finite() || started <= 0.0 || (started - expected).abs() > 0.000001 {
            continue;
        }
        if !matches!(entry["surface"].as_str(), Some("cli" | "tui")) {
            continue;
        }
        let id = entry["session_id"]
            .as_str()
            .ok_or("Missing Hermes session ID")?;
        let id = checkpoints::resolve_sid(&[], "hermes", id)?.ok_or("Invalid Hermes session ID")?;
        matches.push(id);
    }
    if matches.len() > 1 {
        return Err("Hermes has multiple live conversations in this terminal; select one native session before connecting Telegram".into());
    }
    Ok(matches.pop())
}

#[cfg(windows)]
pub fn matches_registration(registration: &super::hook_runtime::Registration) -> Result<bool> {
    use crate::{adapters::harnesses, core::layouts};
    let process = registration
        .harness_process
        .as_ref()
        .ok_or("Missing Hermes process binding")?;
    let environment = crate::platform::environment();
    let profiles = layouts::read_private(&registration.directory.join("control.json"))?
        .map(|bytes| super::config::ControlConfig::parse(&bytes))
        .transpose()?
        .map(|config| config.profiles)
        .unwrap_or_default();
    let tool = harnesses::tool("hermes").ok_or("Missing Hermes adapter")?;
    let root = harnesses::profile_directory(&tool, &environment, &profiles)?;
    let fingerprint =
        serde_json::json!({"pid":process.pid.to_string(),"start":process.start_ticks.to_string()});
    Ok(live(&root, &fingerprint)?.is_some_and(|(id, _)| id == registration.scope.conversation))
}

#[cfg(windows)]
pub fn live(
    root: &std::path::Path,
    process: &Value,
) -> Result<Option<(String, std::path::PathBuf)>> {
    use crate::{app::hook_install, platform};
    use std::path::PathBuf;
    let pid = process["pid"]
        .as_str()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("Hermes foreground process is unavailable")?;
    let started = process["start"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or("Hermes foreground process generation is unavailable")?;
    let owner = platform::current_identity().map_err(|e| e.to_string())?;
    let verify = || -> Result<()> {
        let observed =
            platform::process_identity(pid).map_err(|_| "Hermes foreground process disappeared")?;
        if observed.creation_time != started || observed.owner != owner {
            return Err("Hermes foreground process changed".into());
        }
        Ok(())
    };
    verify()?;
    let mut processes = BTreeMap::new();
    let mut descendants =
        platform::descendant_processes(pid).map_err(|_| "Cannot inspect Hermes subprocesses")?;
    descendants.push(platform::process_identity(pid).map_err(|_| "Hermes process disappeared")?);
    if descendants.len() > 256 {
        return Err("Too many Hermes subprocesses".into());
    }
    for candidate in &descendants {
        let current =
            platform::process_identity(candidate.pid).map_err(|_| "Hermes subprocess changed")?;
        if current.creation_time != candidate.creation_time || current.owner != owner {
            return Err("Hermes subprocess identity changed".into());
        }
        let unix_ticks = current
            .creation_time
            .checked_sub(116_444_736_000_000_000)
            .ok_or("Invalid Hermes process creation time")?;
        processes.insert(current.pid, unix_ticks as f64 / 10_000_000.0);
    }
    let mut profiles = vec![root.to_path_buf()];
    let children = root.join("profiles");
    if children.exists() {
        for entry in
            std::fs::read_dir(&children).map_err(|_| "Cannot read Hermes profile directories")?
        {
            if profiles.len() > 128 {
                return Err("Too many Hermes profiles".into());
            }
            let entry = entry.map_err(|_| "Cannot inspect Hermes profile")?;
            if !entry.file_name().to_string_lossy().starts_with('.') && entry.path().is_dir() {
                profiles.push(entry.path());
            }
        }
    }
    let mut matches: Vec<(String, PathBuf)> = Vec::new();
    for profile in profiles {
        let path = profile.join("runtime/active_sessions.json");
        let Some(bytes) = hook_install::read_profile(&path)? else {
            continue;
        };
        if let Some(id) = selected(&bytes, &processes)? {
            matches.push((id, profile));
        }
    }
    verify()?;
    for candidate in descendants {
        let current = platform::process_identity(candidate.pid)
            .map_err(|_| "Hermes subprocess disappeared")?;
        if current.creation_time != candidate.creation_time || current.owner != owner {
            return Err("Hermes subprocess changed during discovery".into());
        }
    }
    if matches.len() > 1 {
        return Err("Hermes profile identity is ambiguous; no conversation was selected".into());
    }
    Ok(matches.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn leases_require_exact_live_process_and_reject_ambiguity() {
        let processes = BTreeMap::from([(42, 1792000000.123456)]);
        let lease = json!({"pid":42,"process_start_time":1792000000.123456,"session_id":"native-session","surface":"cli"});
        let read = |entries: Vec<Value>| {
            selected(
                &serde_json::to_vec(&json!({"entries":entries})).unwrap(),
                &processes,
            )
        };
        assert_eq!(
            read(vec![lease.clone()]).unwrap().as_deref(),
            Some("native-session")
        );
        for changed in [
            json!({"pid":43}),
            json!({"process_start_time":1791999999.0}),
            json!({"process_start_time":null}),
            json!({"surface":"gateway"}),
        ] {
            let mut stale = lease.clone();
            for (key, value) in changed.as_object().unwrap() {
                stale[key] = value.clone();
            }
            assert_eq!(read(vec![stale]).unwrap(), None);
        }
        assert!(read(vec![lease.clone(), lease]).is_err());
        assert!(selected(b"{}", &processes).is_err());
    }
}
