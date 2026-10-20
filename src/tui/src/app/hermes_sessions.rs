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
    let Some((id, profile)) = live(&root, &fingerprint)? else {
        return Ok(false);
    };
    if id != registration.scope.conversation {
        return Ok(false);
    }
    let current = gateway(&profile, std::path::Path::new("unused"))?;
    Ok(current == registration.server_emitter)
}

fn gateway_pid(bytes: &[u8], creation_time: u64) -> Result<u32> {
    if bytes.len() > 1_048_576 {
        return Err("Hermes gateway state exceeds limit".into());
    }
    let state: Value = serde_json::from_slice(bytes).map_err(|_| "Invalid Hermes gateway state")?;
    let pid = state["pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or("Missing Hermes gateway PID")?;
    let unix = creation_time
        .checked_sub(116_444_736_000_000_000)
        .ok_or("Invalid Hermes gateway birth time")?;
    let centiseconds = (unix as f64 / 100_000.0).round() as u64;
    if state["kind"] != "hermes-gateway"
        || state["gateway_state"] != "running"
        || state["start_time"].as_u64() != Some(centiseconds)
    {
        return Err("Hermes gateway process changed or is not running".into());
    }
    Ok(pid)
}

/// The profile's native daemon record binds the hook emitter separately from
/// the frontend. Never infer a shared Python process from its name alone.
#[cfg(windows)]
pub fn gateway(
    profile: &std::path::Path,
    proc_root: &std::path::Path,
) -> Result<Option<super::hook_runtime::ServerEmitter>> {
    use crate::{
        app::{hook_install, hook_runtime},
        platform,
    };
    let path = profile.join("gateway_state.json");
    let Some(bytes) = hook_install::read_profile(&path)? else {
        return Ok(None);
    };
    let state: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid Hermes gateway state")?;
    let pid = state["pid"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("Missing Hermes gateway PID")?;
    let process =
        platform::process_identity(pid).map_err(|_| "Hermes gateway process disappeared")?;
    if process.owner != platform::current_identity().map_err(|e| e.to_string())?
        || gateway_pid(&bytes, process.creation_time)? != pid
    {
        return Err("Hermes gateway identity changed".into());
    }
    let pin = hook_runtime::capture_server_emitter(
        hook_runtime::ProcessIdentity {
            pid,
            start_ticks: process.creation_time,
        },
        &process.executable,
        proc_root,
    )
    .map_err(|e| e.to_string())?;
    let again = hook_install::read_profile(&path)?.ok_or("Hermes gateway state disappeared")?;
    if gateway_pid(&again, process.creation_time)? != pid {
        return Err("Hermes gateway changed during discovery".into());
    }
    Ok(Some(pin))
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
    fn gateway_requires_native_kind_running_state_and_exact_process_birth() {
        let ticks = 116_444_736_000_000_000 + 1_792_000_000 * 10_000_000;
        let state = json!({"pid":42,"kind":"hermes-gateway","gateway_state":"running","start_time":179200000000_u64});
        assert_eq!(
            gateway_pid(&serde_json::to_vec(&state).unwrap(), ticks).unwrap(),
            42
        );
        for changed in [
            json!({"pid":0}),
            json!({"kind":"other"}),
            json!({"gateway_state":"stopped"}),
            json!({"start_time":179199999999_u64}),
        ] {
            let mut invalid = state.clone();
            for (k, v) in changed.as_object().unwrap() {
                invalid[k] = v.clone();
            }
            assert!(gateway_pid(&serde_json::to_vec(&invalid).unwrap(), ticks).is_err());
        }
        assert!(gateway_pid(&serde_json::to_vec(&state).unwrap(), ticks + 100_000).is_err());
    }
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
