# Harness compatibility

Daedalus runs shells and agent tools in its built-in PTY/ConPTY terminals or
through optional tmux and Herdr adapters. Seven native harness adapters provide
launch, profile selection and hook integration. Sign-in and credentials stay with
each tool.

| Tool | Hook profile | Guided first launch with hooks | Goal continuation |
| --- | --- | --- | --- |
| Claude Code | JSON matcher groups, 2.1.295+ | Yes | Stop-hook block |
| Codex | `hooks.json`; approve it in Codex's `/hooks` trust review | No; register a running process | Native hook response |
| Grok | Native lifecycle; goals 1.0.46 or 1.0.52+; idle readiness 1.0.52+ | Yes | End-turn gated |
| Hermes | YAML shell hooks | No; register a running process | Not available |
| Cursor | `hooks.json` version 1 | No; register a running process | Bounded follow-ups |
| Pi | Extension bridge, 0.87+ or 1.1.x | Yes, 1.1.x | Before-settle |
| OpenCode | Local server plugin, 2.0.28+ | No; register the server and its terminal process | Not available |

A plain launch does not register hooks or route Telegram. To hook a running
process, register it on the Hooks page; the registration must match its process
start, terminal generation and conversation. See [hooks](HOOKS.md) and
[goals](GOALS.md).

## Sessions

Claude Code, Codex, Grok and Pi support checkpoint resume, fork and cross-tool
handoff. Resume and fork are offered only when the installed tool's `--help` lists
the needed option. Pi uses its session file and header. A resumed or forked process
needs its own hook registration. Handoff passes reviewed context to another tool; it
does not copy the first tool's internal state.

Checkpoint capture and Codex hook binding distinguish launch selectors from
conversation IDs. Known Codex options may precede `resume` or `fork`; a canonical
UUID can identify the selected conversation. A resume name needs a reported native
ID. Literal arguments after `--` are not session flags. Forks need an explicit
parent UUID and a distinct reported child ID; uncertain parent selections remain
unresolved. Unrecognized options stop argument inference. An already identified
`resume` command can still use its reported native ID. See the
[Codex CLI reference](https://developers.openai.com/codex/cli/reference).

Claude Code and Grok resume names also need reported native IDs. Their canonical
UUID selectors, long `=value` forms and supported short selectors are recognized
separately from prompt, model, optional and list option values. OpenCode uses
`--session`/`-s` and `--fork`; a nonforking current native selection supersedes its
launch selection. A fork parent is never saved as the child. Unknown option arity,
conflicting selectors or an unproven parent cannot establish a conversation ID.
The selector grammar is checked against Claude Code2.1.317, Grok1.0.70 and
OpenCode1.19.5 help, plus their
[Claude](https://code.claude.com/docs/en/cli-reference),
[Grok](https://docs.x.ai/build/cli/reference) and
[OpenCode](https://dev.opencode.ai/docs/cli/) references. This does not certify
live-provider behavior or every CLI spelling.

Pi1.1.0, Hermes0.20.0 and Cursor2026.10.22 selectors are also checked against
installed help and their native references. Pi IDs retain their native case. Pi's `--resume`/`-r` opens a picker;
`--session` selects a path/ID and `--session-id` is an exact creation ID.
A Pi `--fork` parent is never reported as its child. A known distinct creation
ID can identify the child. Checkpoint capture verifies an explicitly selected
private fork file's first-line header and file identity before and after capture;
it does not derive an ID from its filename. Paths must be symlink-free, without
parent traversal. Partial IDs still require native evidence.
Hermes resume names and Cursor's latest/picker selectors also need native IDs.
Hermes timestamp IDs with six, eight or twelve hex characters and canonical UUID
selectors can identify explicit launches. Known option values never become session
flags, and helper subcommands do not acquire a live conversation ID. Native Pi,
Hermes and Cursor UI selections can change without restarting the process; current
native metadata takes precedence over nonforking launch arguments.
See [Pi](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/cli.md),
[Hermes](https://hermes-agent.nousresearch.com/docs/reference/cli-commands) and
[Cursor](https://cursor.com/docs/cli/reference/parameters).
Native Pi fork/creation/new-session semantics are exercised offline through metadata
RPC commands; this does not certify live model/provider compatibility. Unknown
extension flags, additional legacy ID formats and other native changes still need
explicit contracts.

## Readiness

Daedalus marks an agent idle only from verified events: Cursor and Hermes from
correlated turn events, OpenCode from its server bridge, Pi from `agent_settled`.
A Grok Stop event leaves readiness Unknown because Grok may continue the turn.
Grok 1.0.52+ in the 1.0 series can become ready through its native
`Notification` / `idle_prompt` event, typically about a minute after settlement.
This proves readiness for another prompt, not success or a final answer.

Telegram and `terminal control` slash commands support explicitly audited Claude
Code 2.1.296 and 2.1.317 contracts, the exact live process and an idle registration.
Confirmations bind the observed version, executable, scope, configuration and idle
observation. Model selection is potentially billable because native validation can
issue an inference request. Submission does not confirm a resulting setting.

## AGY

AGY (`agy`) shows up in the inventory and can be watched, attached, previewed and
controlled in a native Daedalus, tmux or Herdr terminal. Start it with its own CLI.
It has no Daedalus sign-in, hooks, readiness or resume/fork support.

## macOS boundaries

macOS uses POSIX PTYs and the native Daedalus terminals backend. Homebrew installs
system tools, npm installs Node harnesses into `~/.local`, and `npx` bootstraps npm
when only the runner is present. The TUI, CLI, configuration, routes, native PTYs,
agent identity, fresh instance import, tmux/Herdr process identity, and hook
invocation/ancestry work through the macOS process backend (`libproc`/`sysctl`).

The same backend provides:

- Read-only discovery of external terminals owned by the current user.
- Native terminal foreground command and working directory.
- Codex profile-wide hook binding and Codex/Grok open-file readiness, using the
  descriptor table from `libproc` in place of `/proc/PID/fd`. These are covered
  by fixture and current-process tests; live Codex and Grok sessions on macOS
  have not been verified yet.
- Telegram service start, status and stop. macOS has no `pidfd`, so stop never
  signals a PID: it writes a private request naming the service's exact process
  incarnation, which only that process consumes before shutting down. Like any
  stop, it is submitted, not complete, until ownership is released.
- OpenCode guided local-server enrollment: the server is the one direct child
  of the verified UI with the same executable and `serve --service`, found through
  `proc_listchildpids`. Tested with a live process tree, not a live OpenCode.
- Legacy cutover process checks. Some macOS releases hide the environment of
  Apple platform binaries; a matching legacy process whose `CONFIG_DIR` cannot be
  read counts as belonging to the instance and blocks cutover until its
  supervisor stops it. A process of this user that `libproc` cannot describe
  also blocks cutover; other users' processes are out of scope, as on Linux.
- In-app updates for command-line installs (`daedalus-<version>-<arch>-macos`).
  `Daedalus.app` updates through a newer DMG.

- Native-terminal Telegram feeding: `tests/native_feeder.rs` runs a real native
  terminal, hook lifecycle and feeder on macOS with a private shell fixture and no
  bot. Session metadata, dashboard self-terminal detection, guided launch and the
  Claude command adapter also read processes through `libproc`.

The following remain unverified on macOS:

- Live Codex, Grok, OpenCode and Claude sessions, and a live Telegram bot.
- `install.sh`; use `scripts/install-macos.sh` or the DMG instead.

## Native Windows boundaries

Windows x64 uses built-in ConPTY terminals by default. Herdr is optional. Configure
compatible native tmux backends with `DAEDALUS_TMUX_BIN` and endpoint selection.
Terminal controls and harness hooks have separate feature support:

| Windows operation | Support |
| --- | --- |
| Ordinary terminal launch/watch/capture/control | Native executable and exact backend identity required |
| Guided first launch with hooks | Unavailable; register an existing verified process |
| Existing Claude Code hooks | Native Herdr registration, audited 2.1.296+ contract |
| Existing Hermes, Pi and OpenCode hooks | Native Herdr registration with provider-specific proof; OpenCode and gateway-backed Hermes pin the server separately from the frontend |
| Codex, Grok and Cursor hooks | Not enabled on Windows |
| Hooks through native tmux-compatible backends | Not enabled on Windows |
| Herdr duplicate attachment view | Unavailable; use read-only Watch |
| Native session resume/fork | Available where the installed harness exposes the required session operation |

Linux, Windows and macOS have automated tests and native terminal checks.
Tests that read Linux `/proc` fixtures directly stay Linux-only; macOS tests use the
same fixtures through the process backend or the live current process. See
[installation](INSTALL.md#macos-install) for macOS setup,
[Windows setup](INSTALL.md#native-windows-install) and
[testing](ARCHITECTURE.md#testing) for the contributor workflow.

OpenCode local-server enrollment and saved-binding reconnect share the native identity metadata policy in **Terminals and discovery**. Linux reads child lists, command arguments and process stability metadata through bounded streams; exhausted process, scan, entry, byte or time budgets refuse enrollment. Reconnect and Telegram registration lookup count ignored files and parse registration JSON within the cumulative budget. A partial or ambiguous scan cannot choose a server or conversation. Windows subtree process admission is cumulative across repeated scans.

## Upstream references

- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
- [Codex](https://github.com/openai/codex)
- [Grok hooks](https://docs.x.ai/build/features/hooks)
- [Hermes hooks](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/hooks.md)
- [Cursor hooks](https://cursor.com/docs/hooks)
- [Pi extensions](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md)
- [OpenCode plugins](https://opencode.ai/v2/docs/build/plugins)
