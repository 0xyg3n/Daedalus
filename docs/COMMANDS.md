# Commands

`daedalus` is the TUI, the CLI, the Telegram service and the hook runtime. Run
`daedalus --help`, or `daedalus COMMAND --help`, for exact options. `ABS` means an
absolute path. A `--target` is a JSON file holding one exact terminal from
`inventory` (backend, pane and generation); display names are never resolved.
Changes print a preview by default and run only with `--apply` and the preview's
digest where one is required.

## TUI shortcuts

**F1** opens a searchable shortcut reference, including the current terminal actions.
Search by key, action or category; arrows scroll and Esc closes it. **Ctrl+P** finds
commands, **Ctrl+K** finds terminals, and **,** opens options and configuration.
In Workspaces, **n** creates a shell or workspace and **N** creates a shell pane in
the selected project. While attached, use **Ctrl+] then N**. Setup and **Detected
harnesses** discover installed tools, reuse configured profiles and let you review
or deselect automatic selections before saving. See [the TUI guide](TUI.md).

## CLI

| Command | Operations |
| --- | --- |
| `daedalus` | Open the control panel; `setup` opens setup (`setup --cli` for prompts) |
| `shell [--config-dir ABS] [--workspace NAME] [--name NAME] [--cwd ABS] [-- COMMAND ARGS…]` | Start and attach a persistent native terminal; Ctrl+] then D detaches |
| `terminal-native` | Native terminal `start`, `list`, `capture`, `input`, `resize`, `rename`, `close`, `attach`, `stream` |
| `config` | `init`, `import`, `import-instance`, `validate`, `show`, `set` |
| `agent` | `capabilities`, `call` with one bounded JSON request on stdin; see [agent tools](AGENT_TOOLS.md) |
| `inventory --json` | List native, tmux and Herdr terminals, plus read-only external shell observations |
| `workspace-usage --config-dir ABS --json` | Read existing Herdr selection aggregates for current terminal identities |
| `terminal` | `list`, `capture`, `send`, `interrupt`, `close`, `focus`, `attach`, `rename`, `launch`, `wait`, `command`, `keys`, `control` |
| `checkpoint` | `list`, `show`, `save`, `remove`, `spawn --operation resume\|fork\|handoff` |
| `route` | `list`, `active`, `candidates`, `select` |
| `hook` | `install`, `inspect`, `run`; `profile-*` (preview, apply, rollback, remove); `launch-*` (preview, apply, cleanup) |
| `goal` | `arm`, `status`, `pause`, `resume`, `complete` |
| `memory` | `config show/set/bind/unbind` for offline Hindsight settings; `list`, `status`, `refresh` for retention receipts |
| `gateway` | `config show/init/set` for reviewed provider override settings ([gateway](GATEWAY.md)) |
| `dashboard` | `config show/set`, search-root and saved-instance operations for global preferences ([dashboard](DASHBOARD.md)) |
| `history` | `config show/set/recover`, paged `list` with `--unresolved` or `--archive`, bounded `archives`, reviewed `archive` ([control history](HISTORY.md)) |
| `telegram` | `status`, `start`, `stop`, `bindings`, `bind`, `config init\|import\|check\|show\|set`, `serve`, `cutover preview\|apply\|rollback` |
| `agent-identity` | Optional Armonia rename: `preview`, `apply`, `status` |
| `diagnostics --json` | Queue, route, registration and configuration health |
| `update check [--json]` | Review a compatible GitHub release; installs nothing |
| `update install --version VERSION --approve SHA [--json]` | Verify and install the exact reviewed release; Windows CLI returns the verified installer path, while TUI launches it after closing |
| `job` | Short aliases for terminal, checkpoint, route and diagnostics operations |
| `tma` | Open an independent view of an exact terminal |

Stateful commands take `--config-dir ABS`. Examples:

```bash
daedalus config init --config-dir /abs/instance --workspace /abs/project --apply
daedalus config show --config-dir /abs/instance
daedalus config set --config-dir /abs/instance --key ui.preview_interval_ms --value 700
daedalus config set --config-dir /abs/instance --key ui.preview_interval_ms --value 700 --apply
daedalus inventory --config-dir /abs/instance --json
daedalus terminal capture --config-dir /abs/instance --target /abs/target.json --lines 80
daedalus checkpoint save --config-dir /abs/instance --target /abs/target.json --name review --apply
daedalus checkpoint spawn --config-dir /abs/instance --key CHECKPOINT --target /abs/target.json --operation fork --harness claude
daedalus checkpoint licenses --config-dir /abs/instance
daedalus checkpoint spawn --config-dir /abs/instance --key CHECKPOINT --target /abs/target.json --operation fork --harness codex --license work
daedalus diagnostics --config-dir /abs/instance --json
```

`checkpoint licenses` lists Codex license names and native directories without
reading authentication files. `checkpoint spawn --operation fork --harness codex
--license NAME` previews the selected profile; add `--apply` to prepare native
history and launch the fork. Named licenses use native profile settings and
bypass the optional Daedalus gateway. The original saved checkpoint is retained.
See [fork licenses and history limits](TUI.md#workspaces) for catalog precedence,
native capabilities, and failure behavior.

- `config show` lists saved portable configuration, eleven instance fields, and every interface and local
  runtime setting, including its type, bounds and description, plus native profile
  overrides for all seven supported tools. `config set` edits
  one listed instance, `ui.`, `runtime.` or `profiles.TOOL` field;
  it previews the exact before/after value by default. `--apply` validates and
  writes privately, keeps a backup and refuses a concurrent configuration change.
  CLI edits take effect on the next launch or instance reopen. Runtime settings
  are described below. See
  [interface settings](TUI.md#display) for defaults and environment precedence.
- `config set --key profiles.codex --value /abs/existing/profile` previews a native
  profile selection; add `--apply` to save it with a private backup. Use
  `--value inherit` to clear that tool's saved override and use its environment
  variable or default profile. Paths must be existing, absolute and symlink-free.
  The selected directory's native account files are retained. Other tools keep
  their saved overrides, and future launches use the selected root. Configuration
  writes refuse retired instances and coordinate with cutover through the shared
  configuration lock.
- **Configuration settings → Instance settings** edits `agent_name`,
  `agent_home`, `subscriptions`, `tmux_sockets`, `discovery.automatic`, `discovery.native`,
  `discovery.herdr_sessions`, `discovery.herdr_sockets`, and the three portable
  `telegram.owner_id`, `telegram.bot_id`, `telegram.username` hints. The schema
  `version` remains fixed. Each save uses a frozen instance/file review and
  a private backup. Existing account profiles, service grants and credentials
  have separate controls.
- Instance list setters take JSON arrays of exact strings; metadata connections
  take a JSON object with unique session keys and absolute socket paths.
  For example, `config set --key tmux_sockets --value '["/abs/a, socket"]'`
  preserves the comma. Add `--config-dir ABS` and `--apply` to save a reviewed
  value. `--value null` clears an optional Telegram hint. The TUI offers per-entry
  add, edit and remove actions, pages lists, and keeps drafts unsaved until review.
  Saved instance values over the interactive input/review limits can be managed
  with validated configuration import.
- The default instance name is an identity, not just a label: changing it can
  stop a running service until its identity is reviewed and restarted. Saved
  registrations, routes and goals keep their existing scopes. A workspace edit
  affects future launches; existing terminals keep their working directories.
  Preferred tools do not install software, sign in, change a paid plan or enable
  a gateway. Reopening setup preserves saved selections, including a gateway
  preference; detected tools are suggestions only for a fresh setup.
- Automatic discovery can be disabled while exact tmux sockets and Herdr session
  names remain configured. `discovery.native` accepts `true`, `false` or `inherit`:
  native terminals can be discovered independently, disabled independently, or
  follow the automatic policy as older configurations do. In JSON, omit `native`
  or use `null` to inherit. Herdr metadata socket bindings are explicit and do
  not automatically add a session to terminal discovery. Duplicate profile or
  metadata-map keys are refused instead of selecting the last JSON value.
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

## Native terminals

```sh
daedalus shell --workspace project --name dev
daedalus terminal-native start --workspace project --name worker --cwd /abs/project -- /bin/bash
daedalus terminal-native list
daedalus terminal-native capture ID GENERATION
daedalus terminal-native capture ID GENERATION --scrollback-rows 100
daedalus terminal-native attach ID GENERATION
daedalus terminal-native resize ID GENERATION --cols 120 --rows 36
daedalus terminal-native rename ID GENERATION --name review --workspace project
daedalus terminal-native close ID GENERATION
```

`start` returns the terminal ID and generation as JSON. Both are required for
later operations; stale identities are rejected. `shell` uses the platform shell
unless a command follows `--`. On Windows, use an installed Windows executable
and a native absolute working directory. `input` accepts `--text` or `--base64`;
it sends exactly those bytes without adding Enter. Mutating `terminal-native`
commands act immediately; unlike the higher-level `terminal` operations, they
do not produce a preview. `close` terminates the owned terminal process.

Saved dimensions, discovery and snapshot budgets are available in **Configuration settings →
Terminals and discovery**, or `config set --key runtime.SETTING --value VALUE`.
All 56 fields in `control.json.runtime` have defaults when omitted:

| Setting | Default | Range |
| --- | --- | --- |
| `native_launch_cols` / `native_launch_rows` | 120 / 32 | 2–512 / 2–256 |
| `shell_cols` / `shell_rows` | 120 / 36 | 2–512 / 2–256 |
| `terminal_query_timeout_ms` | 5000 | 100–60000 |
| `terminal_query_limit_bytes` | 8000000 | 1024–8000000 |
| `discovery_workers` | 8 | 1–32 |
| `discovery_endpoint_limit` | 1024 | 1–8192 |
| `discovery_socket_scan_limit` | 4096 | 1–65536 |
| `discovery_timeout_ms` | 15000 | 100–300000 |
| `discovery_metadata_limit_bytes` | 2097152 | 4096–4194304 |
| `discovery_metadata_value_limit` | 100000 | 256–1000000 |
| `discovery_frame_limit_bytes` | 6291456 | 1048576–8388608 |
| `discovery_relationship_limit` | 16384 | 1–262144 |
| `discovery_relationship_limit_bytes` | 2097152 | 1024–8388608 |
| `discovery_projection_timeout_ms` | 2000 | 1–60000 |
| `discovery_layout_pane_limit` | 4096 | 1–65536 |
| `discovery_layout_limit_bytes` | 2097152 | 1024–8388608 |
| `discovery_layout_timeout_ms` | 2000 | 1–60000 |
| `terminal_resize_pane_limit` | 128 | 2–65536 |
| `terminal_resize_max_cells` | 4096 | 1–65535 |
| `terminal_resize_min_cols` | 4 | 1–32767 |
| `terminal_resize_min_rows` | 3 | 1–32767 |
| `terminal_view_history_limit` | 200 | 1–65536 |
| `terminal_view_history_limit_bytes` | 8388608 | 4096–67108864 |
| `terminal_view_history_value_limit` | 100000 | 128–8388608 |
| `terminal_view_history_timeout_ms` | 2000 | 1–60000 |
| `native_discovery_entry_limit` | 1024 | 1–65536 |
| `native_discovery_timeout_ms` | 1000 | 1–60000 |
| `native_discovery_rpc_timeout_ms` | 250 | 1–60000 |
| `external_discovery_entry_limit` | 65536 | 1–1048576 |
| `external_discovery_timeout_ms` | 2000 | 1–60000 |
| `external_discovery_query_limit` | 512 | 1–65536 |
| `workspace_usage_max_gap_ms` | 30000 | 1–86400000; maximum interval that accrues selected time |
| `workspace_usage_lease_ms` | 30000 | 1–86400000; recorder ownership lease |
| `workspace_usage_retention_days` | 0 | 0–36500; zero keeps history, positive expires old aggregate rows during recording |
| `workspace_usage_entry_limit` | 16384 | 1–1048576; cumulative observations, leases and aggregate lookups |
| `workspace_usage_work_limit_bytes` | 16777216 | 4096–1073741824; cumulative projected metadata and encoded bytes |
| `workspace_usage_timeout_ms` | 2000 | 1–60000; cooperative aggregate-processing/SQL deadline |
| `workspace_usage_sql_step_limit` | 1000000 | 1–100000000; cumulative SQLite VM instructions |
| `workspace_usage_database_limit_bytes` | 67108864 | 4096–536870912; logical database and per-file limit |
| `workspace_usage_output_limit_bytes` | 4194304 | 1024–67108864; captured inventory and complete response byte limits |
| `workspace_usage_busy_wait_ms` | 25 | 0–5000; SQLite lock wait, also bounded by the deadline |
| `native_metadata_file_limit_bytes` | 1048576 | 1024–67108864; maximum input bytes per native identity file |
| `native_metadata_work_limit_bytes` | 16777216 | 1024–1073741824; cumulative metadata reads and stability checks |
| `native_metadata_entry_limit` | 16384 | 1–1048576; cumulative directories, leases and process checks |
| `native_metadata_value_limit` | 262144 | 16–16777216; cumulative strict JSON values and keys |
| `native_metadata_header_limit_bytes` | 16384 | 128–1048576; Pi header only, through its newline |
| `native_metadata_timeout_ms` | 2000 | 1–60000; cooperative native metadata deadline |
| `native_metadata_process_limit` | 128 | 1–65536; cumulative foreground, ancestry, subtree and stability process checks including roots |
| `native_metadata_process_scan_limit` | 65536 | 1–1048576; cumulative Windows snapshot rows or Linux process-directory and child PID entries |
| `native_metadata_depth_limit` | 32 | 1–256; process ancestry depth on Linux and Windows |
| `native_command_argument_limit_bytes` | 2048 | 1–65536; reviewed native command argument bytes, including `/cc` syntax |
| `native_command_model_limit_bytes` | 128 | 1–4096; model ID bytes, also bounded by the argument limit |
| `harness_probe_timeout_ms` | 2000 | 100–60000; local account-status and native command-version checks |
| `harness_probe_limit_bytes` | 16384 | 1024–1048576; output from local account-status and native command-version checks |
| `native_login_timeout_ms` | 120000 | 100–900000 |
| `native_login_output_limit_bytes` | 262144 | 1024–1048576 |
| `database_snapshot_timeout_ms` | 10000 | 1–900000 |
| `database_snapshot_limit_bytes` | 268435456 | 4096–268435456 |
| `database_snapshot_pages_per_step` | 128 | 1–4096 |
| `database_snapshot_busy_wait_ms` | 5 | 1–1000 |
| `database_snapshot_progress_ops` | 10000 | 1–1000000 |
| `instance_import_limit_bytes` | 536870912 | 4096–4294967296 |
| `instance_import_entry_limit` | 4096 | 1–65536 |
| `instance_import_json_value_limit` | 1000000 | 128–16777216 |
| `instance_import_work_limit_bytes` | 2147483648 | 4096–34359738368 |
| `instance_import_timeout_ms` | 120000 | 1–900000 |
| `instance_import_attachment_limit` | 256 | 1–65536 |
| `instance_import_attachment_limit_bytes` | 134217728 | 1–4294967296 |
| `instance_import_frame_limit_bytes` | 262144 | 1024–8388608 |
| `native_scrollback_rows` | 1000 | 0–65536; history retained by newly launched native terminals |
| `checkpoint_file_limit_bytes` | 8388608 | 4096–67108864; maximum bytes per checkpoint registry/removal file |
| `checkpoint_work_limit_bytes` | 67108864 | 4096–1073741824; cumulative admitted checkpoint read/normalization/output bytes |
| `checkpoint_entry_limit` | 65536 | 1–1048576; combined raw checkpoint/removal entries before duplicate collapse, including new entries |
| `checkpoint_value_limit` | 1000000 | 128–16777216; cumulative JSON values and object keys |
| `checkpoint_timeout_ms` | 2000 | 1–60000; cooperative checkpoint processing/write-admission deadline |
| `session_fork_limit_bytes` | 1073741824 | 4096–4294967296; cumulative selected history reads and materialized bytes |
| `session_fork_entry_limit` | 262144 | 1–4194304; directory entries and history records processed |
| `session_fork_value_limit` | 16777216 | 128–134217728; cumulative history JSON values and object keys |
| `session_fork_timeout_ms` | 30000 | 100–300000; shared capture/native migration/fork preparation deadline |
| `agent_activity_max_age_ms` | 60000 | 1000–86400000; maximum verified lifecycle/snapshot age before feedback is stale |
| `agent_activity_query_timeout_ms` | 250 | 1–5000; shared cooperative read-only activity query budget per discovery pass |

Checkpoint policies cover canonical and both supported legacy registries, the
removal index, local list/show/save/remove/launch checks, discovery observations
and import. Duplicate JSON keys, conflicting profile aliases and conflicting
legacy seat identities are refused. Reads preserve the source; failed admission
does not prune existing references or automatically retry effects. Missing
`control.json` uses policy defaults for legacy checkpoint-only list/show/remove;
an invalid existing configuration is refused.

Checkpoint observations share the remaining discovery metadata budget and its
earlier deadline. Failure marks checkpoint observation unavailable while retaining
known terminal identities and a detailed discovery warning. Import also charges
the exact checkpoint input JSON to its shared work/value budget. Removal entries
are preserved; relocating captured context adds the corresponding relocated
removal key so that removed references stay hidden. Unmatched historical removal
entries remain evidence. Byte-work counts admitted serialized/loaded JSON, not
physical I/O or process RSS; deadlines are cooperative and cannot interrupt a
blocking OS call. Saved policy changes apply when the instance is reopened.

Terminal discovery queries sorted tmux paths, then sorted Herdr sessions, up to
the saved endpoint limit, including explicitly configured endpoints. Workers
take the next endpoint as soon as one finishes; completed results retain that
stable order. All workers are joined before the refresh returns. The query byte
budget includes both stdout and stderr for each subprocess. Its per-query timeout
is capped by the remaining whole-refresh deadline. Native discovery runs first
using the same worker limit, followed by endpoint queries and the external scan.
Native registry entries and external process entries have separate saved limits.
Linux counts every directory entry during its process scan; Windows counts
snapshot rows and caps the number of candidate process identity queries.

Native foreground discovery, session-ID recovery, checkpoint preparation and
stability checks share one saved native identity metadata budget per operation.
Linux argument, environment and process-stat reads and process-directory scans
use the saved file, work, entry and process limits. Hook process verification and
dashboard self-ancestry checks also use the saved policy. Resource exhaustion
reports the setting to review; it cannot become a successful partial match or a
context-only checkpoint fallback. Windows executable/owner/birth-time queries
avoid process snapshots; callers needing parentage use bounded snapshots and
retain incarnation and ownership checks. Owned Windows query startup and thread
scans use the query's absolute timeout before resuming a child.

The whole-refresh deadline and stage deadlines are cooperative. Owned subprocess
queries are stopped when their remaining time expires. Native metadata RPCs use
one absolute deadline across connection, partial reads/writes and response checks.
Filesystem and operating-system calls can finish later; results completing after
the relevant deadline are refused and discovery reports incompleteness.

The shared metadata byte/value budgets count cumulative JSON working volume,
including wire replies, normalized records and diagnostics. Repeated
representations count separately; nested values and object keys count toward the
value limit before JSON is materialized. Plain tmux/session text is also charged.
The fixed snapshot root and final incompleteness warnings reserve 1024 bytes and
128 values. These budgets bound admitted discovery metadata; query buffers and
derived presentation data have separate bounds, so this is not a precise
whole-application memory limit. A rejected large endpoint does not discard earlier
verified rows, and a later smaller endpoint can still fit.

The terminal display limit bounds each serialized TUI state frame within the
8 MiB protocol ceiling. Large displays retain a parent-before-child prefix of
exact terminal rows, preserve compiled settings controls and report **PARTIAL**.
Optional saved presentation items can also be omitted when they exceed the limit.
The original verified terminal/view totals remain visible; omitted rows do not
prove a terminal absent. **Discovery warnings** explains display truncation.
**Workspace hierarchy** uses a bounded text outline without building all detail
rows. Increase the relevant limit or narrow discovery scope to view more.

Optional view-place details and nested/declared/checkpoint/route links have
separate count, serialized JSON byte and cooperative projection-time limits.
Every admitted base terminal and its exact identity remain in the tree when
optional details are omitted. Verified terminal/view totals still describe the
admitted snapshot; omitted relationships do not prove absence. The UI and
**Discovery warnings** explain the relationship limit. Increasing the saved limits
restores details on the next projection. The time limit covers optional indexes
and details; base topology construction still completes.

Client activity uses indexed counts instead of retaining every client label for
every terminal. A timed-out client index reports activity unavailable and never
claims nobody else is typing. Native saved routes require full terminal identity;
legacy display-label bindings do not project routes. Grouped aliases preserve
different terminal generations, and linked badges use full identity. These
relationships remain observations and do not grant input or routing authority.
A failed refresh remains **STALE** even when its display is also limited.

Layout metadata has a separate per-group pane limit, cumulative serialized JSON
byte budget and cooperative time limit. Complete valid groups fit together;
unsupported, overlapping, duplicate, ambiguous or over-budget geometry is omitted
and reported in **Discovery warnings**, while base terminal identities remain.
Coordinates and positive dimensions must fit the interface's 16-bit cell format
(0–65535 for positions, 1–65535 for sizes), and panes must fit their reported area.
Each tmux view keeps its own geometry. Herdr layout rows must identify one captured
pane after any reported workspace, tab and terminal-generation selectors are
applied. An unresolved Herdr row conservatively omits that session's geometry.
Unzoomed layouts must include every captured pane in their group; zoomed layouts
may show a subset. A failed refresh remains **STALE** when geometry is omitted.

The saved resize settings apply to both divider previews and backend admission.
Defaults allow up to 128 panes, 4096 cells per movement and minimum dimensions of
4 columns by 3 rows. Dividers use indexed shared borders; an intervening one-cell
terminal cannot be skipped. Panels too small to draw have no interactive divider.
Before submitting once to the native adapter, the backend checks the current
complete geometry, saved limits and both exact terminal identities again. A
missing acknowledgement remains unconfirmed; refresh before trying again. These
checks include Herdr's complete native split tree: ratio amounts use the affected
ancestor area, and rounded cell movement and every changed descendant's minimum
size are checked before submission. Missing or inconsistent split metadata,
ambiguous native borders and requests outside native ratio limits are refused;
the requested movement is never silently shortened. Every changed terminal is
revalidated. A correlated reply must report the expected full layout before the
resize is confirmed. An empty, mismatched or missing reply remains unconfirmed
and is not retried. These paths, both axes, nested moves and zoom refusal were
exercised with a private Linux Herdr 0.9.0 server. Other versions and native
Windows runtime still require their own evidence.

Repeated identical observations of the same terminal view count once. Conflicting
observations retain the known terminal identity and show **Discovery warnings**;
actions, previews, saved-selection references and layout geometry cannot use that
terminal until a clean refresh. Other verified terminals remain usable. Discovery
is partial while conflicts remain, and an already stale refresh stays stale.
Interface preferences and graceful quit remain available when a selection becomes
unverified.

Saved viewer registrations use version 2 structured source and viewer identities:
backend, endpoint, pane and generation remain separate fields. The reader accepts
legacy records only when their encoding is unambiguous; extra slash separators,
conflicting sources for one viewer and unsupported identities are omitted with a
**Discovery warnings** notice. Duplicate records for the same relationship count
once. An incomplete identity pass cannot establish uniqueness and publishes no
viewer relationships from that pass. Physical linked views and registered viewers
use the same full terminal identity in the tree and attach review.

The saved viewer history limit replaces the fixed 200-record retention policy.
Its default remains 200. A confirmed new viewer registration retains the newest
records up to the saved limit; reading history never prunes or rewrites it.
Unsupported registry versions are preserved and refused. An unreadable registry
leaves viewer relationship details explicitly incomplete. A registration failure
after native submission is reported without launching the viewer again. This
setting does not close terminals or change input control.

Viewer-history file size, cumulative JSON values/keys and cooperative processing
time are saved separately. Defaults are 8 MiB, 100000 values and 2000 ms. Strict
preflight rejects duplicate keys and excessive values before allocating parsed
records. Read and projected/saved representations count toward the same history
value budget. Over-budget, unsupported or ambiguous history remains unchanged;
raise the relevant **Viewer history** limit in **Terminals and discovery** to
inspect supported data. Retention counts do not guarantee arbitrary records fit.

Live viewer metadata shares the remaining inventory discovery byte/value/time
budget. Only known relationship/display fields are projected; unknown saved
fields remain on disk. Omitted relationships leave known terminals visible and
their viewer details incomplete. A supported later refresh restores details.
Registration checks serialized size and values before admitting its atomic write;
failures do not launch the already submitted viewer again. Deadlines are
cooperative: filesystem calls can finish later and cleanup does not imply a hard
wall-clock or total-memory bound.

Instance import and final cutover use these source viewer-history limits to
validate a single snapshot and preserve its exact bytes. Unresolved rows remain
historical evidence; fresh native identities still determine live relationships.
Import does not prune history or launch a viewer. Unsupported or over-budget
registries refuse the import. See [instance import](INSTALL.md#configuration-directory)
for preserved state and rollback behavior.

Saved pins, layouts, workflows and checkpoint source references use canonical
terminal endpoints. Historical encoded tmux server labels and the native `local`
display label remain readable through their exact legacy translation. All saved
identity fields, including the supplemental generation when present, must match.
Pin badges and pin-removal selectors use that same identity; labels remain display
text. Missing or changed references are never rebound to a convenient live pane.

Checkpoint tree links disclose the saved profile, project and source. A native
conversation ID match is a candidate association; it does not verify the current
terminal's profile. An exact saved source, including supplemental generation,
identifies the historical origin and still leaves the current profile unverified.
`ui.checkpoint_matches` controls these links: `all` (default) shows all native-ID
candidates, `source` shows only exact saved origins, and `off` hides tree links.
Edit **Interface settings → Checkpoint tree links**, or preview
`config set --key ui.checkpoint_matches --value source` and add `--apply` to save.
The saved checkpoint list and reviewed resume/fork actions remain available;
explicit saved-profile launch selection keeps its existing behavior.

Automatic socket enumeration shares its entry budget across all roots and counts
unrelated files too. Missing optional directories are normal; unreadable or
invalid directories, unrepresentable paths and remaining entries/endpoints over
the saved limits appear in discovery errors. Such a snapshot is incomplete.
Explicit paths remain candidates when automatic enumeration stops. Increase the
relevant saved limit or narrow discovery scope to inspect the remaining endpoints.
The active `TMUX` socket path retains commas before its final PID/index fields.

Instance import and cutover use the **source instance's** captured snapshot
settings for each database, including final native route reconciliation. These
settings are preserved in the imported configuration. Size checks include the
physical source file, committed WAL pages, growth during copying and the final
standalone copy; 256 MiB remains the fixed maximum. Existing destination databases
or `-wal`, `-shm` and `-journal` sidecars are refused and preserved for inspection.
During final native route reconciliation, an earlier inert staged route database
and its sidecars are retained in the private `imported-route-review` folder before
creating the replacement registry. The retained files do not authorize routes.
Failed new copies are removed without changing uncertain source receipts.

The deadline is cooperative. Copy steps and SQLite VM progress callbacks check
elapsed time; a busy/locked pause is capped by the remaining budget. Filesystem
calls can finish later, but a copy that completes after its deadline is refused.
These per-database settings are also capped by the remaining shared import byte
and time budgets.

The shared **Instance import and cutover** settings use the captured source
configuration. Retained bytes and entries bound each staged or compared snapshot,
including directories, receipts, cutover proof and retained route review. JSON
work counts object keys and values in structured staged JSON cumulatively across
capture, writes and repeated verification; opaque JSONL and database contents use
byte work instead. Byte work counts logical captured payloads, staged writes,
completed database copies and bytes read by snapshot hashing. These counters do
not reset between preparation, cutover changes, sealing and publication checks.

The shared deadline starts after the existing bounded source-settings bootstrap
and includes review time until final publication admission. Refusal discards the
owned stage and leaves source and existing destination data unchanged. Raise the
named source setting and prepare a fresh review. Rollback retains its separate
offline proof protocol. These limits do not guarantee total memory usage, total
filesystem I/O, transient SQLite/atomic-write disk usage or hard elapsed time;
individual parser/filesystem/SQLite operations and cleanup can finish later.

Queued inbound frames in `pending` or `retryable` state are processed in ID
order, one borrowed SQLite text payload at a time. The queued-frame byte limit
and shared JSON/work/deadline admission precede owned JSON and Frame decoding.
Attachment count and combined reference bytes are separate source policies;
each verified reference counts, including repeated content. Exhaustion refuses
the cutover before publication; raise the named source setting and review again.

Each file also obeys the effective final Telegram service's `file_limit_bytes`.
Missing, invalid or disallowed files become blocked drafts in the staged copy.
Verified private digest filenames retain their safe storage extension, including
legacy `.bin`; display names do not determine paths. Relocation changes only the
attachment path and payload digest, preserving other JSON fields. Existing
uncertain rows and receipts remain evidence, and no input is replayed.

Workspace and checkpoint actions use the native launch dimensions. `shell` and
`terminal-native start` use saved shell dimensions only with `--config-dir ABS`;
without it they retain 120×36. Explicit `--cols` / `--rows` override saved values.
Attaching a viewer can resize the terminal. Saving new defaults leaves existing
terminals at their current size.

Native terminals retain `runtime.native_scrollback_rows` when launched with the
instance configuration. Without it, the default is 1000 rows. An explicit
`--scrollback-rows N` launch flag takes precedence; zero disables retention.
Existing sessions keep their launch capacity. `capture --scrollback-rows N`
reads a separate history projection up to 65536 rows back, clamped to retained
history; it leaves the shared live screen and other viewers unchanged.
Native watch/control streams accept `terminal.scroll` with `direction` `up` or
`down` and 1–1000 `lines`. Each stream owns its position. Controller input returns
that stream to live. Older native daemons report history unavailable while their
live stream stays connected; they are not restarted automatically.

The terminal query budget applies to tmux/Herdr subprocess queries and actions
using the instance. A mutation that times out remains unconfirmed. Account
probes use the same profile selection and filtered environment as native launches;
timeout, oversized output or an unrecognized response leaves sign-in unverified.
Private-stdin sign-in uses the saved `native_login_timeout_ms` deadline and
`native_login_output_limit_bytes` combined output budget. Raw native output and
private input are not displayed. Failure, timeout or excess output leaves the
sign-in outcome unconfirmed; inspect the tool's account before retrying. Daedalus
does not retry a started sign-in command. Interactive sign-in and opened tools
keep their own lifetime. Capability, hook and native RPC deadlines are separate.

For programmatic views, `stream ID GENERATION --mode observe|control` emits
newline-delimited JSON screen frames with base64 ANSI data. Control streams
accept `terminal.input` (`bytes` in base64), `terminal.resize` (`cols`, `rows`),
and `terminal.release`. Observers cannot input or resize. Detaching releases
control without stopping the session. Keep captures and input out of public logs.

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

### Refreshing installed hooks

**Hooks → Profiles → Refresh / reload managed hooks** reviews all recorded
installations, backs up the existing profiles and updates only managed handlers
to the current Daedalus launcher. Removed handlers are not reinstalled. Changed
profiles or registrations invalidate the review instead of overwriting newer edits.
The backend action is `hooks.reload`.

Each harness has its own activation step: Claude and Cursor watch profile changes;
Codex requires `/hooks` review when a command changes; Pi uses `/reload`; OpenCode
plugins belong to its server. Grok and Hermes may require a native restart.
Daedalus reports these requirements and does not call a profile write a verified
runtime reload. It never kills sessions or approves native trust through this action.
Private per-event results in `hook-status/` contain only status, time and a fixed
error message, never hook payloads, tokens, prompts or tool output.

## Telegram

Each registered DM or group has its own saved default terminal. Open a terminal's
actions and choose **Connect Telegram chat**, or click **Telegram** while attached
(`Ctrl+]`, then `G`). Choose the chat, review its current destination and confirm.
The selected pane stays selected; other chats keep their defaults. After saving,
connect its installed reply hook when prompted to enable responses. If no matching
hook exists, **Set up standard hooks** opens the installer for that exact terminal
and continues to the selected chat's recipient review. Codex and Claude profiles
are detected from the live process. OpenCode defaults to a separate plugin file
for the selected terminal and conversation under its configuration directory;
its account/session data directory is not a plugin destination. Other adapters
request an explicit profile.
Native trust/reload requirements still apply. A saved route alone does not confirm
delivery: the Telegram service must be running, and ordinary group messages require
listening to be enabled.

OpenCode enrollment requires its selected conversation ID to be reported by the
terminal integration. A running executable alone does not identify a conversation.
The guided installer verifies a single native local server directly owned by that
UI and includes both process identities in its review. Shared or remote servers
require an explicit local server binding through `hook profile-preview`; they are
never inferred from a pane name. Installing a plugin and observing its first
accepted event are separate steps. Load the plugin in the owning OpenCode server
before testing delivery; reconnecting a UI alone does not load server plugins.

On Linux and macOS, an existing Codex session with no recorded hook observations can
establish initial readiness from the native rollout still held open by its registered
process (macOS reads the descriptor table through `libproc`; it has fixture and
current-process tests but no live Codex verification yet).
The session ID must match and its latest relevant record must be a completed turn.
New prompts, incomplete writes, ambiguous rollouts or a replaced process block input.
This evidence is rechecked before reserving the queued message; normal hooks track
subsequent turns. This bootstrap is not available on Windows yet.


Chat names and IDs are kept in the private configuration directory. Bots can
discover chats they receive updates from; they cannot list every group in a
person's Telegram account. Add the bot to another group and send
`/start@YourBot`, then use **Find DM or group chats** to register it.

Open **Options → Configuration settings → Telegram configuration**. The default
**Connect my bot and DM** flow asks for the bot token in a masked field and your
numeric Telegram owner user ID. It derives the bot ID from the token and uses your
owner ID as the private DM chat ID. Send `/start` to the bot before using it.
Configuration and token storage are reviewed before saving; starting the service
and assigning a pane remain explicit steps. If local setup has not been saved,
**Save and continue** creates it first. Existing settings are preserved.

An existing connection opens its settings menu instead of repeating onboarding:
- **Add a DM or group** authorizes a user and a chat. Group IDs include their minus sign.
- **Manage chat names** assigns private display names to registered chat IDs. These
  names appear when selecting a chat for a pane; renaming does not change access or routing.
- **Assign chats to panes** chooses each chat's default terminal and reply hook.
- **Change bot token** replaces the private credential for the configured bot.
- **Start connection / Stop connection** controls this instance's service.

**Find chats automatically** discovers recent bot updates for local approval.
Keep the managed service stopped while discovering. Send `/start` to the bot in a
DM or `/start@YourBot` in a group, then select the exact sender and chat.
`daedalus telegram discover --config-dir ABS` provides the same candidate list.
Discovery does not grant access or consume the update cursor, and preserves saved
chat display names. Manual chat registration works without discovery.

`daedalus telegram config show --file ABS` lists runtime values and editable fields.
`daedalus telegram config set --file ABS --key runtime.SETTING --value VALUE`
previews one change; add `--apply` to save it with a backup. Stop the service first;
runtime edits are serialized against startup ownership. No token is read and no
network request is made. See [transport settings](TELEGRAM.md#transport-and-service-settings).

`daedalus telegram config init` writes `telegram-service.json`: bot ID, token
file, sender grants (Read, Send, Manage), feeder registrations and reply
recipients. `daedalus telegram serve --config-dir ABS` runs the service in the
foreground; in the panel, **Telegram → Start the Telegram connection** starts it
after confirmation. Nothing starts it automatically.

Local agents can inspect `telegram status --config-dir ABS`, preview `telegram
start` or `telegram stop`, then apply with `--apply --approve DIGEST` using the
unchanged preview digest. Start/stop request a transition; check status afterward.
`telegram bindings --config-dir ABS --registration ABS` lists eligible reply
recipients. `telegram bind --config-dir ABS --registration ABS --chat ID --actor
ID` previews an exact hook-to-chat binding; applying also requires `--apply
--approve DIGEST`. Existing grants and live registration/conversation identity
are checked. These commands never accept a token argument.

Each chat routes to one exact terminal and conversation. In Herdr, run
`herdr integration install claude` so Herdr reports the Claude conversation ID that
routing needs. Only granted human senders can issue commands; forwarded messages
cannot. Commands that change a terminal ask for a one-time confirmation tied to the
sender, chat and route.

| Command | Purpose |
| --- | --- |
| `/menu`, `/panel`, `/help` | Command menu |
| `/status`, `/where` | Current route and service state |
| `/windows`, `/jobs`, `/fleet` | Terminals this chat can route to |
| `/use [KEY]`, `/pin`, `/unpin` | Select or keep the chat's route |
| `/sessions`, `/busy`, `/pending` | Conversations, verified busy state, queue counts |
| `/say TEXT`, `/send TEXT` | Send text to the routed terminal |
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
| `/model`, `/effort`, `/plan`, `/clear`, `/compact`, `/context`, `/cost`, `/agents`, `/mcp`, `/review`, `/security-review` (`/secreview`), `/export`, `/config`, `/memory`, `/cc` | Claude Code slash commands; explicitly audited versions 2.1.296 and 2.1.317 |

Command aliases remain accepted without duplicate menu entries: `/wins`, `/tmux`
(list terminals), `/queue`, `/mute`, `/hush`, `/ping` (quiet), `/watch`, `/unmute`
(listen), `/escape`, `/ret`, `/return` and `/cancel` (Ctrl+C). They use the same
route checks, grants and confirmations as their canonical commands.

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

### Cube layout

Select **Cube** in the Workspaces view bar, or cycle layouts with `[` / `]`. Each workspace has its own cube, identified by workspace name, number, colour and terminal count. `Alt+Up/Down` switches workspace cubes, wrapping at the ends. Left/right cycle one terminal within the current cube; up/down cycle two. `Alt+Left/Right` changes the front terminal without changing the cube orientation. Click a face once to select and attach its terminal; click empty space outside the faces to detach while keeping Cube open. `Enter` opens terminal actions; `z` opens Focus and `Esc` returns to Cube. Small windows show only the front terminal. Rotation changes the selected preview without sending keys to an agent.

Workspace colours are derived from workspace identity, rather than terminal order. Unknown workspace membership is labelled Unassigned. Rotation briefly adds chromatic scan and frame effects; reduced motion disables those effects. Terminal text and the underlying terminal are unchanged by the effects.

Cube remembers each workspace’s front terminal and orientation during the current TUI session. Removed terminals fall back to a current member of that workspace. `PageUp/PageDown` also switch cubes; Alt+mouse-wheel switches cubes and plain wheel rotates terminals. Transitions follow the movement direction and settle within 180 ms. Focus uses the same coloured tree rows as Tree, with terminal metadata underneath.

The Motion option controls selection sweeps, switch feedback, shortcut hover highlights and cube glitch fragments. Reduced motion disables these effects; no-colour terminals keep plain styling. Effects do not change terminal text or input.

**a** or a click inside the selected preview attaches
without leaving Cube, Tiles or Wall. **w** watches in place. **Ctrl+] then D**
detaches, **Ctrl+] then O** observes, and **Ctrl+] then [ / ]** changes layout.
**Ctrl+] then arrows** detaches and navigates within a preview layout. Selecting
another terminal releases the old input connection. A hidden or undersized screen
never receives keyboard or paste input. **z** remains the explicit Focus action.

Cube resizing: drag the front face's right/bottom edges, or use `<` / `>` for
width and `+` / `-` for height. While attached, prefix the resize key with
`Ctrl+]`. Double-click an edge to reset it; proportions persist in local layouts.

Native identity metadata uses one cumulative policy per Pi/Hermes operation, including stability rechecks. OpenCode direct-child discovery and saved-binding reconnect share one policy; Linux process streams obey the saved file/work limits and all child candidates count toward the process/scan limits. Hook-binding lookup for reconnect and Telegram uses the saved cumulative entry, file, work, JSON-value and deadline limits, counting ignored directory entries. It never accepts a match from an incomplete scan. Hook registrations also retain their 64 KiB protocol ceiling and private ownership/location checks. Duplicate JSON keys, ambiguous matches, exhausted budgets and expired observations are refused. Pi reads only its first header line. Windows subtree discovery and display labels use the saved process, snapshot and depth limits. These cooperative deadlines do not preempt blocking operating-system calls. Native slash-command verification uses the saved probe timeout and output limit; audited command-version support remains explicit.

Native command confirmations show the observed audited version, verified conversation, impact flags and review fingerprint before the command preview. Telegram stores this exact review and rechecks native executable identity, version, configuration, scope and idle observation when the one-use button is pressed. Older native confirmations without that review are refused; request a new one. Submission remains distinct from completion.

For Claude Code 2.1.317, `/cost` and `/stats` resolve to `/usage`, `/review` resolves to `/code-review`, and `/cc /list-agents` or `/cc /peers` request the native peer inventory where enabled. `/agents` prints native configuration guidance. The audited effort forms add `xhigh`, `max`, `status`, and `ultracode [on|off]`; native model/account policy may still reject a form. Review flags and task-starting plan arguments remain outside this adapter. Unknown versions remain unsupported. References: [native commands](https://code.claude.com/docs/en/commands).

The saved native argument and model ID limits appear under Terminals and discovery → Native commands. A request over those limits is refused before launching a version probe. Changing a limit does not relax native command syntax or authorize another provider version. Clear starts a new native conversation; external route bindings retain their current scope and need review after a conversation change.

Native model selection is shown as potentially billable: the audited validation path can issue a one-token model request when a model is not already validated. Preparing a Daedalus confirmation only queries the native version; it does not dispatch the model-selection command. Model IDs admit ASCII letters/digits and `._:-/@`, with a leading letter or digit; saved byte limits also apply.

Native controls require the registration instance to match the selected configuration identity. A saved registration is not silently adopted after changing that identity. The verified instance is included in the review.

Workspace selection recording is controlled by `ui.workspace_usage_recording` (default `true`). Turning it off stops sampling without deleting existing aggregates; re-enabling starts a fresh interval. The read-only `workspace-usage` API can still read retained history and reports whether recording is enabled in the saved configuration. Native inventory uses the saved discovery policy; aggregate projection/SQL uses the Workspace usage policy. Retention defaults to zero (keep all); a positive number expires old aggregate rows during a successful sample and does not compact the database file or delete native session history. Budget exhaustion refuses the complete sample/projection; unfinished sampling writes are rolled back.
