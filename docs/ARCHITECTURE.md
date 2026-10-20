# Architecture

<p align="center"><img src="../assets/architecture.svg" alt="Daedalus architecture diagram" width="900"></p>

Daedalus is one Rust executable built from `src/tui`. It contains the Ratatui
interface, the CLI and its backend, the Telegram service and the hook runtime.
Shared operations live in `src/tui/src/app`, durable stores in `core`, and terminal
and tool boundaries in `adapters`.

## Interface and backend

The interface starts the same executable as `daedalus backend --stdio` and talks to
it over a local, versioned JSON protocol (version 2). Requests carry state queries,
previews, exact-target operations and dialog answers; each confirmation can be used
once. The protocol is not a network API.

## Native terminal backend

`app/native_terminal.rs` owns a detached PTY/ConPTY daemon for each native
terminal. `adapters/native.rs` projects its observations and launches into the
shared terminal operations. Detaching a client leaves the shell alive; closing
the terminal ends the owned process. User-wide discovery uses an owner-private
registry, independent of the UI's working directory or configuration instance.

Clients authenticate a fresh server challenge before sending input over loopback.
Requests and replies are authenticated and bound to the terminal generation;
captured requests cannot be replayed on a new connection. One renewable control
lease owns input and resize. Observers receive rendered screens but cannot mutate
terminal size or write. A lost write acknowledgement remains uncertain.

The backend parses ANSI output into a bounded screen; it does not forward raw
terminal escape sequences to viewers. External process discovery is a separate,
read-only adapter in `adapters/standalone.rs`. It cannot adopt an arbitrary existing
shell's PTY, read all Windows application tabs, or control another user's processes.

## Identity and effects

A display name is never an identity. Every operation pins the backend, endpoint,
pane generation, process start and, where needed, the conversation, and rechecks
them just before acting. The TUI, CLI and Telegram all call the same operations.

Telegram keeps a durable inbox and outbox, with one lock per instance and bot.
Admitting input, submitting it to a terminal, observing it through hooks and
delivering a reply are separate states. Sender, route, registration and recipient
are rechecked at dispatch. An unknown outcome is kept for review and never re-sent.
A permission revoked after a request was sent cannot recall that request.

Private JSON and SQLite stores hold configuration, routes, checkpoints, goals,
layouts and receipts. Armonia and Hindsight are optional, each with its own
configuration; terminal routing needs neither. Agent tools keep their own
authentication and runtime. The Pi and OpenCode hook bridges run inside those tools.

On Windows, SQLite uses `TRUNCATE` journals with private sidecar files and in-memory
temporary storage. Existing WAL/SHM files must pass owner and ACL checks; data is
not deleted to bypass a journaling or permissions failure.

## Code map

| Area | Files | Notes |
| --- | --- | --- |
| Shared operations | `app/backend.rs`, CLI and menu modules | Unknown commands are rejected |
| Terminals | `adapters/terminals.rs`, `adapters/terminal_actions.rs`, `app/tree.rs` | Native PTY/ConPTY, tmux and Herdr; external process discovery is read-only |
| Agent tools | `adapters/harnesses.rs`, `app/hook_install.rs`, `app/hook_runtime.rs` | Launch, sign-in, profiles, hooks |
| Sessions | `app/session_actions.rs`, `app/session_menu.rs`, `app/session_cli.rs` | Resume, fork, handoff |
| Telegram | `app/telegram*.rs`, `adapters/telegram.rs` | Admission, outbox, commands, streams |
| Goals | `core/goal_store.rs`, `app/hook_runtime.rs` | Durable transitions and recovery |
| Setup and import | `app/wizard*.rs`, `app/setup.rs`, `app/config.rs`, `app/migration.rs`, `app/cutover.rs`, `app/dependencies.rs` | Dependencies reviewed before install |
| Gateway | `app/gateway.rs` | Optional Claude/Codex endpoint override |
| Armonia | `app/agent_identity.rs`, `app/armonia_status.rs` | Reads an existing registry |
| Hindsight | `app/memory.rs`, `app/memory_status.rs`, `adapters/hindsight.rs` | Exact bank per registration |
| Diagnostics | `app/diagnostics.rs` | Aggregate status only |
| Interface | `src/tui/src/*.rs`, `src/tui/assets/commands.json` | Action catalog, navigation, tree fixtures |

Daedalus does not mirror raw console output, administer gateway accounts, run
unattended watchdogs or fail over between providers.

## Adding an integration

A new frontend or app connector goes through five stages: authorize the incoming
event, resolve an exact current route, dispatch through an existing typed
operation, correlate the tool's result, and record delivery evidence. Keep app,
sender, tool, account, terminal and conversation identities separate. Never derive
permissions from observed senders or use pane titles as identities. Recheck grants
and route revision before a queued delivery, and keep uncertain external effects
for review instead of retrying them.

## Testing

```bash
./scripts/check.sh
git diff --check
```

On native Windows, run `./scripts/check-windows.ps1` in PowerShell. It adds
isolated PE/checksum installer fixtures and performs the same Cargo checks.
The native Windows suite includes a real isolated ConPTY shell test for input,
capture, resize, identity refusal and cleanup. Installer fixtures verify individual
ownership and BOM-free receipts. Run these checks on Windows for native runtime
coverage. No hosted workflow is required.

`scripts/check.sh` runs `cargo fmt --check`, `cargo test --locked --all-targets`,
`cargo clippy -D warnings` and a release build for `src/tui`.

Tests use isolated state, synthetic hook events, local HTTP fixtures for Telegram
and Hindsight, and private tmux and shell fixtures. They never start a real bot,
send messages, touch user sessions or make paid model requests.
`src/tui/tests/ui_startup.rs` drives the interface in a PTY with no agent tools
installed; `src/tui/tests/native_cli.rs` runs the packaged CLI against private
state. `src/tui/tests/native_terminal.rs` exercises actual isolated native shell
processes, stream control, resize, stale identity refusal and detach survival.

Record the upstream harness version when reporting integration regressions. Run
a credential scan on the current tree before publishing.

## Supported operations

The TUI, CLI, Telegram service and local agent interface share the native
operations described in the [command reference](COMMANDS.md). See
[agent tools](HARNESSES.md) for provider and platform support,
[hooks](HOOKS.md) for lifecycle integration, [goals](GOALS.md) for continuation,
and [installation](INSTALL.md) for setup and configuration import.
