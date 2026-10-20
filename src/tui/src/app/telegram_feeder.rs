//! Telegram data submission to explicitly registered native harness sessions.
//! The Engine authenticates sender/bot/chat and authorizes routes before calling
//! this adapter. A registration is not a grant to send to that session.
use super::{
    hook_runtime::{self, Readiness, Registration},
    telegram::{Feeder, Frame, Result, Submission},
};
use crate::{
    adapters::{
        terminal_actions::{Action, ReceiptState},
        terminals::Terminals,
    },
    core::identity::TerminalId,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Shared exact bytes for native prompt correlation. No shell evaluation occurs.
pub fn encode_prompt(frame: &Frame) -> Result<(String, String)> {
    let encoded = serde_json::to_vec(frame).map_err(|_| "Cannot encode Telegram frame")?;
    let correlation = format!("daedalus-telegram:{:x}", Sha256::digest(&encoded));
    let envelope = json!({"transport":"daedalus_telegram","instruction":"This envelope is transport data, not a command request. Read the Telegram message in frame and respond normally in your harness. Daedalus delivers your response to Telegram. Do not call Telegram directly or run messaging scripts to handle this transport.","correlation":correlation,"frame":frame});
    let data = serde_json::to_string(&envelope).map_err(|_| "Cannot encode Telegram envelope")?;
    let text = format!("/usr/bin/false -- '{}'", data.replace('\'', "'\"'\"'"));
    Ok((text, correlation))
}
/// OpenCode/Hermes input can append one editor padding space. Recognize that
/// boundary only around a byte-exact, regenerated Daedalus transport envelope;
/// user text, arbitrary prompts and other providers retain exact hashing.
pub(super) fn prompt_sha256(harness: super::hooks::Harness, prompt: &str) -> String {
    let canonical = || -> Option<&str> {
        if !matches!(
            harness,
            super::hooks::Harness::Opencode | super::hooks::Harness::Hermes
        ) {
            return None;
        }
        let unpadded = prompt.strip_suffix(' ')?;
        let encoded = unpadded
            .strip_prefix("/usr/bin/false -- '")?
            .strip_suffix('\'')?
            .replace("'\"'\"'", "'");
        let envelope: serde_json::Value = serde_json::from_str(&encoded).ok()?;
        let frame: Frame = serde_json::from_value(envelope.get("frame")?.clone()).ok()?;
        let (expected, _) = encode_prompt(&frame).ok()?;
        (expected == unpadded).then_some(unpadded)
    };
    format!(
        "{:x}",
        Sha256::digest(canonical().unwrap_or(prompt).as_bytes())
    )
}
pub struct NativeFeeder {
    pub instance: String,
    /// Explicit private registration paths selected by local service setup.
    pub registrations: Vec<PathBuf>,
    pub terminals: Terminals,
}
impl NativeFeeder {
    fn registrations(&self) -> Result<Vec<(PathBuf, Registration)>> {
        if self.registrations.len() > 4096 {
            return Err("Too many hook registrations".into());
        }
        // Fail closed if the configured inventory cannot be read completely:
        // silently skipping a broken entry could conceal an ambiguous binding.
        self.registrations
            .iter()
            .map(|path| {
                let r = hook_runtime::inspect(path).map_err(|e| e.to_string())?;
                if r.scope.instance != self.instance {
                    return Err("Registration belongs to another instance".into());
                }
                Ok((path.clone(), r))
            })
            .collect()
    }
}
impl Feeder for NativeFeeder {
    fn live(&mut self) -> Result<Vec<TerminalId>> {
        let registered = self.registrations()?;
        // Retired process bindings remain available for audit after a relaunch.
        // Only currently verified input owners can make a terminal ambiguous.
        let verified = registered
            .iter()
            .filter(|(_, r)| {
                r.harness_process.is_some()
                    && hook_runtime::verify_live_session(r, &self.terminals).is_ok()
            })
            .collect::<Vec<_>>();
        let mut live = Vec::new();
        for (_, r) in &verified {
            if verified
                .iter()
                .filter(|(_, other)| other.scope.terminal == r.scope.terminal)
                .count()
                != 1
            {
                continue;
            }
            live.push(r.scope.terminal.clone());
        }
        Ok(live)
    }
    fn submit(&mut self, frame: &Frame) -> Result<Submission> {
        if frame.scope.instance != self.instance || frame.conversation.is_none() {
            return Ok(Submission::Unavailable);
        }
        let registrations = self.registrations()?;
        let candidates = registrations
            .iter()
            .filter(|(_, r)| r.scope.terminal == frame.terminal)
            .filter_map(|(path, r)| {
                r.harness_process.as_ref()?;
                hook_runtime::verify_live_session(r, &self.terminals)
                    .ok()
                    .map(|actual| (path, r, actual))
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return Ok(Submission::Unavailable);
        }
        let (path, r, actual) = &candidates[0];
        if frame.conversation.as_deref() != Some(r.scope.conversation.as_str()) {
            return Ok(Submission::Unavailable);
        }
        let snapshot = match hook_runtime::ready_snapshot(path, &self.terminals.proc_root) {
            Ok(s) => s,
            Err(_) => return Ok(Submission::Unavailable),
        };
        if snapshot.scope != r.scope {
            return Ok(Submission::Unavailable);
        }
        match snapshot.state {
            Readiness::Busy => return Ok(Submission::Busy),
            Readiness::Unknown => return Ok(Submission::Busy),
            Readiness::Idle => (),
        }
        let (text, correlation) = encode_prompt(frame)?;
        if text.len() > 64000 {
            return Ok(Submission::Unavailable);
        }
        if !hook_runtime::reserve_ready_for(
            path,
            &self.terminals.proc_root,
            snapshot.sequence,
            &text,
        )
        .map_err(|e| e.to_string())?
        {
            return Ok(Submission::Busy);
        }
        // From reservation onward, preserve Busy even if verification fails.
        // No cancellation path can accidentally permit an uncertain replay.
        if hook_runtime::verify_live_session(r, &self.terminals).is_err() {
            return Ok(Submission::Unavailable);
        }
        match self.terminals.action(actual, Action::Send(text)) {
            Ok(receipt)
                if matches!(
                    receipt.state,
                    ReceiptState::Submitted | ReceiptState::Accepted
                ) =>
            {
                Ok(Submission::Accepted(
                    json!({"kind":"terminal_submission","terminal":frame.terminal,"conversation":frame.conversation,"correlation":correlation,"readiness_sequence":snapshot.sequence,"status":"submitted_not_processed"}),
                ))
            }
            _ => Ok(Submission::Unknown),
        }
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::*;
    use crate::{
        app::hooks::Harness,
        core::{identity::Backend, routes},
    };

    #[test]
    fn padding_normalization_preserves_transport_content_and_exact_other_prompts() {
        let frame = Frame {
            scope: routes::Scope {
                instance: "fixture".into(),
                bot: "1".into(),
                chat: "2".into(),
            },
            actor: "3".into(),
            message_id: 4,
            update_id: 5,
            terminal: TerminalId {
                backend: Backend::Herdr,
                endpoint: "fixture".into(),
                pane: "pane".into(),
                generation: "generation".into(),
            },
            conversation: Some("conversation".into()),
            route_revision: 1,
            text: " user 'text' \n".into(),
            attachment: None,
        };
        let (text, correlation) = encode_prompt(&frame).unwrap();
        let exact = prompt_sha256(Harness::Opencode, &text);
        assert_eq!(prompt_sha256(Harness::Opencode, &format!("{text} ")), exact);
        assert_eq!(prompt_sha256(Harness::Hermes, &format!("{text} ")), exact);
        for changed in [
            format!("{text}  "),
            format!("{text}\t"),
            format!("{text}\n"),
            format!("prefix {text} "),
            text.replace(&correlation, "invalid-correlation") + " ",
            text.replace("This envelope", "Changed envelope") + " ",
            text.replace("user", "changed-user") + " ",
        ] {
            assert_ne!(prompt_sha256(Harness::Opencode, &changed), exact);
        }
        assert_ne!(prompt_sha256(Harness::Claude, &format!("{text} ")), exact);
        assert_ne!(
            prompt_sha256(Harness::Opencode, "plain input "),
            prompt_sha256(Harness::Opencode, "plain input")
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        app::{
            hook_runtime::{Clock, Options, ProcessIdentity, Verifier},
            hooks::{EventKind, Harness},
        },
        core::{goal_store, identity::Backend, routes},
    };
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        directory: PathBuf,
        path: PathBuf,
        registration: Registration,
        feeder: NativeFeeder,
        frame: Frame,
    }
    struct Verified;
    impl Verifier for Verified {
        fn verify(&mut self, _: &Registration) -> hook_runtime::Result<()> {
            Ok(())
        }
    }
    struct Time;
    impl Clock for Time {
        fn now_ms(&self) -> u64 {
            1
        }
        fn wait_ms(&mut self, _: u64) {}
    }
    impl Fixture {
        fn new() -> Self {
            Self::new_for(Harness::Claude)
        }
        fn new_for(harness: Harness) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "daedalus-native-feeder-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let proc = directory.join("proc");
            fs::create_dir_all(proc.join("42")).unwrap();
            let mut fields = vec!["0"; 20];
            fields[0] = "S";
            fields[19] = "500";
            fields[2] = "42";
            fields[5] = "42";
            fields[4] = "99";
            fs::write(
                proc.join("42/stat"),
                format!("42 (harness) {}", fields.join(" ")),
            )
            .unwrap();
            let executable = directory.join("herdr");
            fs::write(&executable,format!("#!/bin/sh\ncase \"$3 $4\" in\n'pane get') printf '%s\\n' '{{\"result\":{{\"pane\":{{\"pane_id\":\"pane\",\"terminal_id\":\"terminal\",\"pid\":42,\"agent\":\"claude\"}}}}}}' ;;\n'agent prompt') printf '%s\\n' \"$6\" > '{}/sent'; if test -f '{}/fail'; then exit 1; fi; printf '%s\\n' '{{\"result\":{{}}}}' ;;\n*) exit 1 ;;\nesac\n",directory.display(),directory.display())).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let server_emitter = if harness == Harness::Opencode {
                let server = directory.join("server");
                fs::write(&server, b"\x7fELF fixture only").unwrap();
                fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
                std::os::unix::fs::symlink(&server, proc.join("42/exe")).unwrap();
                Some(
                    hook_runtime::capture_server_emitter(
                        ProcessIdentity {
                            pid: 42,
                            start_ticks: 500,
                        },
                        &server,
                        &proc,
                    )
                    .unwrap(),
                )
            } else {
                None
            };
            let terminal = TerminalId {
                backend: Backend::Herdr,
                endpoint: "example".into(),
                pane: "pane".into(),
                generation: "terminal".into(),
            };
            let registration = Registration {
                version: 1,
                id: "hook".into(),
                directory: directory.clone(),
                scope: goal_store::Scope {
                    instance: "example".into(),
                    terminal: terminal.clone(),
                    conversation: "session".into(),
                    harness,
                },
                goal_id: None,
                harness_process: Some(ProcessIdentity {
                    pid: 42,
                    start_ticks: 500,
                }),
                server_emitter,
                transcript: Some(directory.join("transcript.jsonl")),
                events: vec![EventKind::Stop, EventKind::UserPromptSubmit],
                options: Options::default(),
                activity_lease_ms: 30000,
                pending_timeout_ms: 30000,
                hook_timeout_ms: 12000,
            };
            let path = hook_runtime::install(&registration).unwrap();
            let feeder = NativeFeeder {
                instance: "example".into(),
                registrations: vec![path.clone()],
                terminals: Terminals {
                    herdr: executable,
                    proc_root: proc,
                    ..Terminals::default()
                },
            };
            let frame = Frame {
                scope: routes::Scope {
                    instance: "example".into(),
                    bot: "1".into(),
                    chat: "2".into(),
                },
                actor: "3".into(),
                message_id: 4,
                update_id: 5,
                terminal,
                conversation: Some("session".into()),
                route_revision: 1,
                text: "hello\nuser content".into(),
                attachment: None,
            };
            Self {
                directory,
                path,
                registration,
                feeder,
                frame,
            }
        }
        fn event(&self, name: &str, id: &str, prompt: Option<&str>) {
            let bytes = serde_json::to_vec(
                &json!({"session_id":"session","hook_event_name":name,"prompt":prompt,"last_assistant_message":"done"}),
            )
            .unwrap();
            hook_runtime::execute(
                bytes.as_slice(),
                &self.registration,
                Some(id),
                &mut Verified,
                &mut Time,
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    #[test]
    fn transport_guidance_preserves_exact_message_and_prompt_correlation() {
        let mut fixture = Fixture::new();
        fixture.frame.text = "Literal 'quote'\n$HOME; $(do-not-run)".into();
        let (prompt, correlation) = encode_prompt(&fixture.frame).unwrap();
        let quoted = prompt
            .strip_prefix("/usr/bin/false -- '")
            .unwrap()
            .strip_suffix('\'')
            .unwrap();
        let envelope: serde_json::Value =
            serde_json::from_str(&quoted.replace("'\"'\"'", "'")).unwrap();
        assert_eq!(envelope["frame"]["text"], fixture.frame.text);
        assert_eq!(envelope["correlation"], correlation);
        assert_eq!(
            correlation,
            format!(
                "daedalus-telegram:{:x}",
                Sha256::digest(serde_json::to_vec(&fixture.frame).unwrap())
            )
        );
        assert!(
            envelope["instruction"]
                .as_str()
                .unwrap()
                .contains("Do not call Telegram directly")
        );
        assert_eq!(encode_prompt(&fixture.frame).unwrap().0, prompt);
    }
    #[test]
    fn valid_registration_without_idle_evidence_waits_without_input_or_reservation() {
        let mut fixture = Fixture::new();
        assert_eq!(
            fixture.feeder.live().unwrap(),
            vec![fixture.frame.terminal.clone()]
        );
        assert_eq!(
            fixture.feeder.submit(&fixture.frame).unwrap(),
            Submission::Busy
        );
        assert!(!fixture.directory.join("sent").exists());
        let connection =
            rusqlite::Connection::open(fixture.directory.join("hook-observations.sqlite3"))
                .unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM hook_reservations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            fixture.feeder.submit(&fixture.frame).unwrap(),
            Submission::Busy
        );
        assert!(!fixture.directory.join("sent").exists());
    }
    #[test]
    fn registered_idle_submission_requires_correlated_prompt_before_next_send() {
        let mut f = Fixture::new();
        assert_eq!(f.feeder.live().unwrap(), vec![f.frame.terminal.clone()]);
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Busy);
        f.event("Stop", "first", None);
        let receipt = f.feeder.submit(&f.frame).unwrap();
        assert!(matches!(receipt, Submission::Accepted(_)));
        let sent = fs::read_to_string(f.directory.join("sent")).unwrap();
        assert!(sent.contains("transport data, not a command request"));
        assert!(sent.contains("\\nuser content"));
        f.event("Stop", "late-stop", None);
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Busy);
        f.event("UserPromptSubmit", "unrelated", Some("other input"));
        f.event("Stop", "unrelated-stop", None);
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Busy);
        f.event(
            "UserPromptSubmit",
            "modified",
            Some(&format!("prefix {}", sent.trim_end())),
        );
        f.event("Stop", "modified-stop", None);
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Busy);
        f.event("UserPromptSubmit", "correlated", Some(sent.trim_end()));
        f.event("Stop", "no-proof", None);
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Busy);
        let transcript = format!(
            "{}\n{}\n",
            json!({"sessionId":"session","type":"user","uuid":"prompt-1","parentUuid":null,"message":{"content":sent.trim_end()}}),
            json!({"sessionId":"session","type":"assistant","uuid":"answer-1","parentUuid":"prompt-1","message":{"content":"done","stop_reason":null}})
        );
        crate::core::layouts::write_private(
            f.registration.transcript.as_ref().unwrap(),
            transcript.as_bytes(),
        )
        .unwrap();
        f.event("Stop", "finished", None);
        f.frame.update_id += 1;
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
        assert_eq!(
            hook_runtime::ready_snapshot(&f.path, &f.feeder.terminals.proc_root)
                .unwrap()
                .state,
            Readiness::Busy
        );
    }
    #[test]
    fn codex_feeder_can_repeat_after_exact_native_turn_completion() {
        let mut f = Fixture::new_for(Harness::Codex);
        let native = |f: &Fixture, name: &str, turn: &str, prompt: Option<&str>| {
            let bytes=serde_json::to_vec(&json!({"session_id":"session","hook_event_name":name,"turn_id":turn,"transcript_path":null,"last_assistant_message":"done","prompt":prompt})).unwrap();
            hook_runtime::execute(
                bytes.as_slice(),
                &f.registration,
                None,
                &mut Verified,
                &mut Time,
            )
            .unwrap();
        };
        native(&f, "UserPromptSubmit", "initial", Some("initial input"));
        native(&f, "Stop", "initial", None);
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
        let sent = fs::read_to_string(f.directory.join("sent")).unwrap();
        native(
            &f,
            "UserPromptSubmit",
            "telegram-turn",
            Some(sent.trim_end()),
        );
        native(&f, "Stop", "old-other-turn", None);
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Busy);
        native(&f, "Stop", "telegram-turn", None);
        f.frame.update_id += 1;
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
    }
    #[test]
    fn hermes_feeder_repeats_after_padded_native_prompt_completion() {
        let mut f = Fixture::new_for(Harness::Hermes);
        let native = |f: &Fixture, event: &str, turn: &str, prompt: &str| {
            let payload = json!({"session_id":"session","hook_event_name":event,
                "extra":{"turn_id":turn,"user_message":prompt,"assistant_response":"done"}});
            hook_runtime::execute(
                serde_json::to_vec(&payload).unwrap().as_slice(),
                &f.registration,
                None,
                &mut Verified,
                &mut Time,
            )
            .unwrap();
        };
        native(&f, "pre_llm_call", "initial", "initial input");
        native(&f, "post_llm_call", "initial", "initial input");
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
        let sent = fs::read_to_string(f.directory.join("sent")).unwrap();
        let padded = format!("{} ", sent.trim_end());
        native(&f, "pre_llm_call", "telegram-turn", &padded);
        native(&f, "post_llm_call", "telegram-turn", &padded);
        f.frame.update_id += 1;
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
    }
    #[test]
    fn changed_process_wrong_conversation_and_ambiguous_registration_never_send() {
        let mut f = Fixture::new();
        f.event("Stop", "one", None);
        f.frame.conversation = Some("other".into());
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Unavailable);
        f.frame.conversation = Some("session".into());
        f.feeder.registrations.push(f.path.clone());
        assert!(f.feeder.live().unwrap().is_empty());
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Unavailable);
        f.feeder.registrations.pop();
        fs::write(
            f.feeder.terminals.proc_root.join("42/stat"),
            "42 (reused) S 0",
        )
        .unwrap();
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Unavailable);
        assert!(!f.directory.join("sent").exists());
    }
    #[test]
    fn retired_process_binding_does_not_block_relaunched_terminal() {
        let mut f = Fixture::new();
        let mut retired = f.registration.clone();
        retired.id = "retired".into();
        retired.scope.conversation = "previous-session".into();
        // The PID has been reused: the old process incarnation must not count
        // as a second live registration or authorize the old conversation.
        retired.harness_process.as_mut().unwrap().start_ticks += 1;
        let retired_path = hook_runtime::install(&retired).unwrap();
        f.feeder.registrations.insert(0, retired_path.clone());
        f.event("Stop", "ready", None);
        assert_eq!(f.feeder.live().unwrap(), vec![f.frame.terminal.clone()]);
        let mut old_frame = f.frame.clone();
        old_frame.conversation = Some(retired.scope.conversation);
        assert_eq!(
            f.feeder.submit(&old_frame).unwrap(),
            Submission::Unavailable
        );
        assert!(!f.directory.join("sent").exists());
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
        assert!(retired_path.exists());
    }
    #[test]
    fn opencode_editor_padding_does_not_stall_the_next_telegram_turn() {
        let mut f = Fixture::new_for(Harness::Opencode);
        let native = |f: &Fixture, payload: serde_json::Value| {
            hook_runtime::execute(
                serde_json::to_vec(&payload).unwrap().as_slice(),
                &f.registration,
                None,
                &mut Verified,
                &mut Time,
            )
            .unwrap();
        };
        let prompt = |turn: &str, text: &str| json!({"bridge_version":"daedalus.opencode.v2","session_id":"session","hook_event_name":"UserPromptSubmit","event_id":format!("prompt-{turn}"),"turn_id":turn,"prompt":text});
        let completed = |turn: &str, text: &str, id: &str| json!({"bridge_version":"daedalus.opencode.v2","session_id":"session","hook_event_name":"Stop","event_id":id,"native_event":{"id":id,"type":"session.execution.succeeded","data":{"sessionID":"session"},"durable":{"aggregateID":"session","seq":5,"version":1}},"context":[{"id":turn,"type":"user","text":text},{"id":id.replacen("evt_","msg_",1),"type":"idle","outcome":"succeeded"}]});
        native(&f, prompt("msg_initial", "initial input"));
        native(&f, completed("msg_initial", "initial input", "evt_initial"));
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
        let sent = fs::read_to_string(f.directory.join("sent")).unwrap();
        let padded = format!("{} ", sent.trim_end());
        native(&f, prompt("msg_telegram", &padded));
        native(&f, completed("msg_telegram", &padded, "evt_telegram"));
        f.frame.update_id += 1;
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
    }
    #[test]
    fn two_live_conversation_bindings_still_require_review() {
        let mut f = Fixture::new();
        let mut other = f.registration.clone();
        other.id = "other-live".into();
        other.scope.conversation = "other-session".into();
        f.feeder
            .registrations
            .push(hook_runtime::install(&other).unwrap());
        f.event("Stop", "ready", None);
        assert!(f.feeder.live().unwrap().is_empty());
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Unavailable);
        assert!(!f.directory.join("sent").exists());
    }
    #[test]
    fn suspended_or_background_harness_cannot_receive_into_shell() {
        let mut f = Fixture::new();
        f.event("Stop", "one", None);
        let stat = f.feeder.terminals.proc_root.join("42/stat");
        let original = fs::read_to_string(&stat).unwrap();
        fs::write(&stat, original.replacen(") S ", ") T ", 1)).unwrap();
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Unavailable);
        fs::write(
            &stat,
            original.replacen("S 0 42 0 99 42", "S 0 42 0 99 77", 1),
        )
        .unwrap();
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Unavailable);
        assert!(!f.directory.join("sent").exists());
    }
    #[test]
    fn shell_race_envelope_cannot_evaluate_external_content() {
        let mut f = Fixture::new();
        f.event("Stop", "one", None);
        let marker = f.directory.join("must-not-exist");
        f.frame.text = format!(
            "$(touch {}) `touch {}` apostrophe ' newline\n; touch {}",
            marker.display(),
            marker.display(),
            marker.display()
        );
        assert!(matches!(
            f.feeder.submit(&f.frame).unwrap(),
            Submission::Accepted(_)
        ));
        let sent = fs::read_to_string(f.directory.join("sent")).unwrap();
        assert_eq!(sent.lines().count(), 1);
        let status = std::process::Command::new(crate::test_support::path("/bin/sh"))
            .arg("-c")
            .arg(&sent)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(1));
        assert!(!marker.exists());
    }
    #[test]
    fn transport_failure_preserves_reservation_and_unknown_outcome() {
        let mut f = Fixture::new();
        f.event("Stop", "one", None);
        fs::write(f.directory.join("fail"), "").unwrap();
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Unknown);
        assert!(f.directory.join("sent").exists());
        assert_eq!(f.feeder.submit(&f.frame).unwrap(), Submission::Busy);
    }
}
