# Agent JSON interface

Included in the current Rust executable.

`daedalus agent capabilities` lists noninteractive native command families and
operation names. `daedalus agent call` reads one JSON request from stdin and emits
one versioned JSON response. This is a local CLI adapter, not an MCP server or a
public network API.

```json
{"tool":"config","args":["init","--config-dir","/absolute/private/instance","--workspace","/absolute/project"]}
```

On Windows, use absolute Windows paths with JSON backslashes escaped. Pass the
request through a file or stdin instead of interpolating it into a shell command:

```sh
daedalus agent capabilities
daedalus agent call < request.json
```

The request has `tool`, `args`, optional `input` and optional `request_id`. Unknown fields and
multiple JSON documents are rejected. `input` is accepted only for `goal arm` and
contains that goal directive. Limits: 1 MiB request, 256 arguments, 16 KiB per
argument, 128 KiB total argument text, 4 MiB native operation output.

`request_id` is a caller-chosen correlation label of 1–128 ASCII letters, digits,
`-`, `_`, `.` or `:`. Valid labels are echoed on success and operation failures.
Malformed JSON and invalid labels are not echoed. A label is neither an approval
nor an idempotency key: repeating it executes another request.

Success is `{"version":1,"ok":true,"result":...}`. Native JSON output is returned
as JSON; other output is returned as a string. Failure is
`{"version":1,"ok":false,"error":{"code":"...","message":"..."}}`.
Operation failures discard partial output and redact underlying error details.
Use native help/diagnostics for investigation. Check `ok` even when the process
exits normally. A failed or truncated response does not prove a mutation had no
effect; do not automatically retry it.

## Available operations

- Inventory and aggregate diagnostics.
- `workspace_usage`: read existing Herdr selection aggregates joined to current
  terminal identities; no terminal capture or new usage recording.
- Terminal list/capture/send/keys, interrupt/close/focus/rename, ordinary launch,
  bounded lifecycle wait and audited harness controls.
- Checkpoint list/show/save/remove and capability-checked spawn.
- Exact route candidates/selection/status.
- Hook registration/profile review, installation/removal/rollback and launch-plan
  preparation/cleanup. Hook event execution and provider activation are excluded.
- Goal arm/status/pause/resume/complete.
- Memory receipt list/status and explicit provider-status refresh.
- Configuration init/import/whole-instance import/validate.
- Optional Armonia identity preview/apply/status.
- Explicit update check; it contacts the release provider but installs nothing.
- Telegram status/start/stop/bind/bindings, configuration and coordinated cutover.
  Start/stop request a transition; status observes the result.

Underlying native parsers, permissions, exact targets, previews, approval digests,
one-use confirmations and platform/provider restrictions apply unchanged. The
adapter adds no `--apply`, grants or defaults. It dispatches shared application
functions directly; it does not invoke a shell or another Daedalus process to
interpret requests. Authorized underlying operations may call installed tools or
transports as documented by those operations.

## Interactive handoff

Use the ordinary TUI/CLI for setup prompts, provider sign-in, terminal attachment,
hook event execution, guided provider activation and the continuous Telegram
service. `terminal attach`, `hook launch`, `hook run` and `telegram serve` are
rejected before dispatch. Capability metadata lists accessible entrypoints;
provider requirements are covered in the [harness guide](HARNESSES.md).

## Human and agent coordination

Use the same private configuration directory for the TUI and agent requests.
Both interfaces dispatch the same operations and terminal identity checks. The
JSON adapter is a local process interface with the permissions of its caller;
keep its stdin/stdout inside the trusted local workflow. It adds no remote
listener, account system, task reservation or shared-agent scheduler.

1. Call `inventory` with `--config-dir ABS --json`. Select an exact current
   terminal identity; display names and pane positions alone cannot identify it.
2. Prepare the intended native operation. Read its help and inspect its target,
   preview and any approval digest. Human operators can inspect the same target
   in the TUI. Merely displaying a preview does not authorize an effect.
3. Apply using the operation's documented flags. The adapter adds no `--apply`
   or permission grant; the shared operation checks identity and approvals again.
4. Inspect receipts or status to distinguish submission from completion. Use
   `request_id` to associate the response with your own work item. If a pane
   changes, discover it again; if an effect is uncertain, review before retrying.

`agent capabilities` includes these coordination steps and the operation roster.
Different operations have different native confirmation requirements; this
interface does not impose a separate approval workflow or imply every mutation
has the same `--apply` contract.

## Workspace selection aggregates

```json
{"request_id":"worker-2:usage.1","tool":"workspace_usage","args":["--config-dir","/absolute/private/instance","--json"]}
```

The same read is available as:

```sh
daedalus workspace-usage --config-dir /absolute/private/instance --json
```

The result contains only currently discovered Herdr terminal identities and
existing `visits`, `seconds` and `last_selected` aggregates, keyed by the same
hashed `TerminalId` used by the TUI recorder. A new terminal generation does not
inherit its predecessor's counts. There are no pane contents, provider prompts,
transcripts or saved labels in this response. The identities include local
endpoint and pane identifiers and should remain private.

`status: "missing"` means no usage database exists; the read creates nothing.
`recorded: false` and `usage: null` mean no matching record, rather than zero
measured use. `discovery_complete` and `discovery_errors` report whether the
inventory pass encountered endpoint errors; undiscovered terminals are omitted.
Unreadable, unsafe or incompatible databases fail explicitly. Reads use the
bounded private read-only diagnostics connection and do not create schema,
recover journals or call the usage sampler. SQLite may coordinate existing WAL
readers through their existing shared-memory index.

These figures describe Herdr server selection observed while the local TUI
recorder runs. They do not establish human attention, focused desktop windows,
agent execution time or activity between samples. The API does not start a
recorder or supply historical terminal listings. Tmux usage is not measured by
this Herdr-specific feature.
