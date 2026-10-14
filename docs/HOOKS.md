# Hooks

Hooks are how an agent tells Daedalus what it is doing. Each tool's native hook
system calls `daedalus hook run`, which records the event in a durable journal.
Daedalus uses that journal to know when a turn ends, what to send to Telegram and
whether a goal should continue. Hooks are not a sandbox and do not make tool output
trusted.

## Features

Each registration pins one tool, process start, terminal generation and
conversation, and enables its features explicitly.

| Feature | Triggered by | Effect |
| --- | --- | --- |
| Lifecycle | Session, prompt, turn and completion events | Busy/idle state; Unknown when evidence is missing |
| Telegram reply | A verified assistant reply or completed turn | Queued reply to the bound recipient |
| Progress | Tool and activity events | One Telegram message edited in place |
| Text stream (optional) | Accepted Telegram input plus the matching turn | Assistant text only |
| Work stream (optional) | Completed tool events | Tool names only; no arguments, results or console output |
| Notifications (optional) | Attention events | Sent to the bound recipient |
| Hindsight memory (optional) | Session start; end of a turn | Recall and retention ([below](#hindsight-memory)) |
| Goal continuation | The tool's stop or before-settle boundary | Continues an armed goal ([goals](GOALS.md)) |

Installing a hook profile never grants a Telegram recipient or starts a service.

## Install and remove

The Hooks page (or `daedalus hook profile-preview` / `profile-apply`) installs a
profile for any of the seven tools. It merges into the tool's existing hook file,
shows the change first and saves a private backup and receipt. Each tool keeps its
own trust and consent prompts. Run `daedalus hook --help` for every option.

- **Guided first launch** (Claude Code, Pi, Grok) starts the tool in a new terminal
  already registered. For the other tools, register an existing process; the
  registration must match its process start, terminal and conversation.
- **Removal** uses the saved receipt. If the hook file changed since, Daedalus asks
  for review instead of overwriting it. Cleaning up a Grok launch hook also needs
  the original process gone.
- **Herdr integration**: Herdr ships its own integration installer for Claude,
  Codex and Grok. Daedalus can run it after a review. `herdr integration install
  claude` makes Herdr report the Claude conversation ID that Telegram routing needs.

## Rules

| Area | Behavior | Code |
| --- | --- | --- |
| Registration | Private file; stale terminal or process identities are rejected | `app/hooks.rs`, `app/hook_install.rs` |
| Lifecycle | Bounded JSON input; uncertain readiness stays Unknown | `app/hook_runtime.rs` |
| Telegram reply | Recipient, route and grants rechecked before sending; an uncertain send is not replayed | `app/hook_effects.rs`, `app/hook_dispatch.rs` |
| Progress | Only a confirmed message is edited; losing permission drops pending edits | `app/hook_dispatch.rs` |
| Streams | Tied to the accepted Telegram input and its turn; a local prompt or `/stream off` revokes queued output | `app/telegram_stream.rs` |
| Profile changes | Preview digest, backup, receipt; changed files refuse rollback or removal | `app/hook_install.rs`, `app/hook_uninstall.rs` |
| First launch | One-use, expiring review; exact new terminal and process | `app/hook_launch.rs`, `app/pane_launch.rs` |

## Hindsight memory

[Hindsight](https://docs.hindsight.vectorize.io/) memory is optional and
independent of Telegram. Configure it on the Hooks page; settings live in the
private `hindsight.json`: endpoint, credential file, recall query and one bank per
registration. A bank is never guessed from a project name or pane title.

- Recall runs at session start and adds bounded context. Recalled text is
  reference data, not instructions.
- Retention sends the assistant's reply text at the end of a turn. Prompts, tool
  arguments, tool results and hidden reasoning are not sent.
- An accepted retention request is not reported as indexed.
  `daedalus memory status|refresh --key KEY` checks the receipt and the provider's
  operation status.
- Missing configuration disables memory. A failed request is reported as failed;
  no memory is invented.
