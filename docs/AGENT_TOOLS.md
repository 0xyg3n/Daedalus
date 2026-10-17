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

The request has only `tool`, `args` and optional `input`. Unknown fields and
multiple JSON documents are rejected. `input` is accepted only for `goal arm` and
contains that goal directive. Limits: 1 MiB request, 256 arguments, 16 KiB per
argument, 128 KiB total argument text, 4 MiB native operation output.

Success is `{"version":1,"ok":true,"result":...}`. Native JSON output is returned
as JSON; other output is returned as a string. Failure is
`{"version":1,"ok":false,"error":{"code":"...","message":"..."}}`.
Operation failures discard partial output and redact underlying error details.
Use native help/diagnostics for investigation. Check `ok` even when the process
exits normally. A failed or truncated response does not prove a mutation had no
effect; do not automatically retry it.

## Available operations

- Inventory and aggregate diagnostics.
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
rejected before dispatch. Capability metadata describes accessible entrypoints,
not universal feature parity or proof that an installed provider works.
