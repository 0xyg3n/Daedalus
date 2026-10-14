# Commands

`daedalus` is the TUI, the CLI, the Telegram service and the hook runtime. Run
`daedalus --help`, or `daedalus COMMAND --help`, for exact options. `ABS` means an
absolute path. A `--target` is a JSON file holding one exact terminal from
`inventory` (backend, pane and generation); display names are never resolved.
Changes print a preview by default and run only with `--apply` and the preview's
digest where one is required.

## CLI

| Command | Operations |
| --- | --- |
| `daedalus` | Open the control panel; `setup` opens setup (`setup --cli` for prompts) |
| `config` | `init`, `import`, `import-instance`, `validate` |
| `inventory --json` | List tmux and Herdr terminals |
| `terminal` | `list`, `capture`, `send`, `interrupt`, `close`, `focus`, `attach`, `rename`, `launch`, `wait`, `command`, `keys`, `control` |
| `checkpoint` | `list`, `show`, `save`, `remove`, `spawn --operation resume\|fork\|handoff` |
| `route` | `list`, `active`, `candidates`, `select` |
| `hook` | `install`, `inspect`, `run`; `profile-*` (preview, apply, rollback, remove); `launch-*` (preview, apply, cleanup) |
| `goal` | `arm`, `status`, `pause`, `resume`, `complete` |
| `memory` | `list`, `status`, `refresh` for Hindsight retention receipts |
| `telegram` | `config init\|import\|check`, `serve`, `cutover preview\|apply\|rollback` |
| `agent-identity` | Optional Armonia rename: `preview`, `apply`, `status` |
| `diagnostics --json` | Queue, route, registration and configuration health |
| `update check [--json]` | Check for a newer GitHub release; installs nothing |
| `job` | Short aliases for terminal, checkpoint, route and diagnostics operations |
| `tma` | Open an independent view of an exact terminal |

Stateful commands take `--config-dir ABS`. Examples:

```bash
daedalus config init --config-dir /abs/instance --workspace /abs/project --apply
daedalus inventory --config-dir /abs/instance --json
daedalus terminal capture --config-dir /abs/instance --target /abs/target.json --lines 80
daedalus checkpoint save --config-dir /abs/instance --target /abs/target.json --name review --apply
daedalus checkpoint spawn --config-dir /abs/instance --key CHECKPOINT --target /abs/target.json --operation fork --harness claude
daedalus diagnostics --config-dir /abs/instance --json
```

- `terminal wait` needs `--registration ABS` and returns on a verified lifecycle
  event; silence is not completion.
- `terminal control` sends Claude Code `/model`, `/effort` or `/plan` with
  `--registration`, `--command` and optional `--arguments`. Apply with
  `--apply --expect-sha PREVIEW_SHA`.
- `job` verbs: `list`, `seats`, `snapshot`, `status`, `new`, `send`, `delegate`,
  `cmd`, `capture`, `wait`, `stop`, `kill`, `keys`, `attach`, `use`, `active`,
  `sessions`, `checkpoint`, `save`, `checkpoints`, `resume`, `fork`, `handoff`,
  `health`, `model`, `effort`, `plan`. They take the same flags as the commands
  they map to.

## Hooks and goals

```bash
daedalus hook inspect --registration /abs/registration.json
daedalus hook run --registration /abs/registration.json --event-id EVENT_ID
daedalus goal arm --registration /abs/registration.json < directive.txt
daedalus goal status --registration /abs/registration.json
```

`hook run` reads one hook payload on stdin. `goal arm` reads the directive on stdin
and does not start an agent. `profile-apply` needs the digest from `profile-preview`
and saves a rollback receipt. See [hooks](HOOKS.md) and [goals](GOALS.md).

## Telegram

`daedalus telegram config init` writes `telegram-service.json`: bot ID, token
file, sender grants (Read, Send, Manage), feeder registrations and reply
recipients. `daedalus telegram serve --config-dir ABS` runs the service in the
foreground; in the panel, **Telegram → Start the Telegram connection** starts it
after confirmation. Nothing starts it automatically.

Each chat routes to one exact terminal and conversation. In Herdr, run
`herdr integration install claude` so Herdr reports the Claude conversation ID that
routing needs. Only granted human senders can issue commands; forwarded messages
cannot. Commands that change a terminal ask for a one-time confirmation tied to the
sender, chat and route.

| Command | Purpose |
| --- | --- |
| `/menu`, `/panel`, `/help` | Command menu |
| `/status`, `/where` | Current route and service state |
| `/windows`, `/jobs` | Terminals this chat can route to |
| `/use [KEY]`, `/pin`, `/unpin` | Select or keep the chat's route |
| `/sessions`, `/busy`, `/pending` | Conversations, verified busy state, queue counts |
| `/say TEXT` | Send text to the routed terminal |
| `/capture` | Read recent terminal output; may contain private text |
| `/interrupt`, `/ctrlc` | Interrupt |
| `/stop`, `/esc`, `/enter`, `/keys KEYS` | Send keys |
| `/kill` | Close the terminal |
| `/new claude\|pi\|grok [split]` | Launch a hooked agent in a new split pane |
| `/quiet`, `/listen` | Group-chat listening |
| `/stream on\|off\|status` | Stream assistant text (private chats only) |
| `/reason`, `/verbose` `on\|off\|status` | Stream completed tool names |
| `/goal status\|arm TEXT\|pause\|resume\|complete` | Goal for the routed agent |
| `/checkpoint list\|save NAME\|remove KEY\|resume KEY\|fork KEY` | Checkpoints (`/checkpoints`, `/resume KEY` are aliases) |
| `/session_rename NAME`, `/session_rename_status OP` | Rename the tmux session |
| `/rename AGENT NEW_BASE`, `/rename_status ar:ID` | Optional Armonia rename |
| `/model`, `/effort`, `/plan`, `/clear`, `/compact`, `/context`, `/cost`, `/agents`, `/mcp`, `/review`, `/security-review` (`/secreview`), `/export`, `/config`, `/memory`, `/cc` | Claude Code slash commands; require Claude Code 2.1.296 |

`/mode`, `/license` (`/lic`) and `/mirror` (`/console`) reply that they are not
available.

Messages:

- Input: text, captions, photos (largest size, up to 20 MiB) and documents (up to
  20 MiB). Voice, audio, video and stickers are ignored; only their caption is used.
  Attachments are saved privately under `<config-dir>/attachments/`.
- Replies are plain text, split at 4096 UTF-16 units. Progress is one message
  edited in place.
- Streaming needs Read, Send and Manage. It sends verified assistant text or tool
  names, not console output, and never tool arguments, results or hidden reasoning.
  A new local prompt, route change, revocation or `/stream off` drops pending output.

## Diagnostics

`daedalus diagnostics --config-dir ABS --json` (or Activity in the panel) reports
queue counts, configuration and registration health, route validity and failures
without prompt text or credentials.

Input moves through admitted, queued, submitted, consumed or uncertain; output
through queued, leased, delivered or uncertain. A Telegram receipt shows delivery,
not that the agent's task succeeded. An uncertain send is never retried
automatically; review it before resending. `/busy` reports busy only with verified
lifecycle evidence; a quiet terminal or visible prompt does not count as idle.

For a stuck task, check the route and conversation, then the hook registration and
process, then the queue and failure receipts.
