# Telegram

Connect a bot to an agent conversation so you can send work from a DM or group and
receive assistant replies. Each registered chat has its own destination pane and
reply binding. Credentials, chat IDs and attachments stay in your private
[configuration directory](INSTALL.md#configuration-directory).

## Connect a bot

1. Open **Options → Configuration settings → Telegram configuration** and choose
   **Connect my bot and DM**.
2. Paste the bot token into the masked field and enter your numeric Telegram owner
   user ID. Daedalus derives the bot ID and uses your owner ID as the default DM
   chat ID. Review and save the settings.
3. Send `/start` to your bot in Telegram.
4. Open the destination harness conversation, then assign the chat to its pane.
   Install or review its standard hooks and connect the reply recipient.
5. Choose **Start connection** and check its status before sending a message.

Saving settings does not start the service. Installing hooks does not grant chat
access. Each harness keeps its own hook trust prompts and activation requirements.
See [hook installation](HOOKS.md#install-and-remove).

Token fields show a character count without revealing the token. On Windows,
**Ctrl+V** and **Shift+Insert** support clipboard paste in forms. Hook setup suggests
a detected absolute profile path: press Enter to accept it, enter another path or
choose `skip` / Esc to finish later. Skipping hooks leaves replies unconfigured.

## Register groups and change destinations

An existing connection opens its settings menu:

| Setting | Purpose |
| --- | --- |
| Transport and service | Configure the API origin, deadlines, polling, backoff, batches, feedback, progress and streaming. Stop the service before saving these changes. |
| Add a DM or group | Authorize a sender and chat; preserve the minus sign in group IDs. |
| Manage chat names | Give registered chats private display names. |
| Assign chats to panes | Set a chat's default destination and reply hook. |
| Change bot token | Replace the configured bot's private credential. |
| Start connection / Stop connection | Control this instance's Telegram service. |

Use **Find chats automatically** to review recent bot updates. Stop the managed
service and any other poller first, send `/start` in a DM or `/start@YourBot` in a
group, then approve the exact sender and chat. Discovery does not grant access or
advance the service's update cursor. Manual registration is also available.
A bot can discover chats it receives updates from; it cannot list every group in
your personal account.

Group input requires a mention, a reply to the bot or `/listen`. `/quiet` restores
selective input. For `/listen` to receive ordinary group messages, configure the
bot's [privacy mode](https://core.telegram.org/bots/features#privacy-mode) in
BotFather or give it group admin rights.

From a selected terminal, use **t** to choose one of the registered chats. Names
are labels; routing uses the exact chat, terminal generation and conversation.
Changing a chat name does not change access or its destination.

## Service controls

Telegram runs as a separate process of the Daedalus binary. No separate daemon
package or systemd unit is required. Closing the TUI leaves a started connection
running; stop it explicitly through the Telegram menu when needed. Setup and
Windows TUI startup do not start `telegram serve`. A Windows install or uninstall
can ask a saved instance to stop so `daedalus.exe` can be replaced; install does
not start it afterwards. Start the connection again from Telegram settings after
the new version opens. `telegram serve` itself refuses a temporary updater image
when that image is not the installed file.

```sh
daedalus telegram status --config-dir /absolute/private/instance
```

The TUI offers reviewed start and stop actions. Local automation can preview
`daedalus telegram start|stop`, then apply with the returned approval digest.
These actions request a transition; check status to confirm the result.
On Linux, stop signals the pinned process through `pidfd`. On macOS and Windows,
stop writes a private request naming the service's exact process incarnation; the
service consumes it at its next stop poll and then releases ownership.
`daedalus telegram serve --config-dir ABS` runs the service in the foreground.
See [the command reference](COMMANDS.md#telegram) for complete syntax.

## Transport and service settings

Open **Telegram configuration → Transport and service** to review saved runtime
settings. Fields prefill their saved values; Ctrl+U clears a field. Every change
shows its before/after value and requires confirmation.
Runtime settings live under `runtime` in private `telegram-service.json`. Older
files use the existing defaults; partial runtime objects use defaults for omitted
fields. Unknown fields and inconsistent timeout combinations are rejected.

Stop the managed service before saving runtime changes. The save takes the same
ownership lock as service startup and keeps a private backup. It does not stop,
start or reconnect a bot. Starting the service later loads the reviewed settings;
a configuration changed during startup is refused before connecting. Grants and
recipient changes retain their existing policy reload behavior.

The API origin selects the HTTPS server receiving the bot credential. It must be
an origin without URL credentials, a path, query or fragment. Redirects, proxies
and automatic HTTP retries remain disabled. Short discovery requests have their
own deadline; they do not inherit a long-poll timeout. A long poll's HTTP deadline
is the larger of the minimum poll deadline and server wait plus connection slack.

```bash
daedalus telegram config show --file /abs/instance/telegram-service.json
daedalus telegram config set --file /abs/instance/telegram-service.json --key runtime.loop_interval_ms --value 250
daedalus telegram config set --file /abs/instance/telegram-service.json --key runtime.loop_interval_ms --value 250 --apply
```

`show` lists every saved value, numeric bound and description. `set` previews by
default; `--apply` saves the exact reviewed value and refuses stale configuration
or active ownership. Neither command reads token contents or contacts Telegram.
The [service example](../config/telegram-service.example.json) contains all defaults.

| Runtime setting | Default |
| --- | --- |
| `api_origin` | `https://api.telegram.org` |
| `connect_timeout_ms` / `request_timeout_ms` | `10000` / `60000` |
| `feedback_connect_timeout_ms` / `feedback_timeout_ms` | `2000` / `3000` |
| `pairing_connect_timeout_ms` / `pairing_timeout_ms` | `3000` / `8000` |
| `poll_timeout_seconds` | `50` |
| `poll_timeout_slack_ms` / `poll_request_min_ms` | `20000` / `30000` |
| `poll_retry_ms` | `5000` |
| `startup_retry_initial_ms` / `startup_retry_max_ms` | `1000` / `60000` |
| `loop_interval_ms` / `stop_poll_ms` / `policy_reload_ms` | `100` / `100` / `1000` |
| `outbound_batch_limit` / `inbound_batch_limit` / `hook_batch_limit` | `32` / `8` / `32` |
| `updates_batch_limit` | `100` |
| `json_limit_bytes` / `file_limit_bytes` | `4194304` / `20971520` |
| `feedback_typing_enabled` / `feedback_ack_enabled` / `feedback_commands_enabled` | `true` / `true` / `true` |
| `feedback_queue_limit` | `64` |
| `feedback_typing_interval_ms` | `4000` |
| `feedback_typing_ttl_ms` / `feedback_ack_ttl_ms` / `feedback_commands_ttl_ms` | `1000` / `10000` / `10000` |
| `progress_interval_ms` / `progress_batch_limit` | `2000` / `32` |
| `stream_event_batch_limit` | `64` |
| `stream_input_retention` / `stream_subscription_limit` | `10000` / `1024` |
| `stream_text_limit_bytes` / `stream_chunk_utf16_limit` | `64000` / `4000` |
| `stream_message_limit_per_event` | `16` |
| `outbound_retry_ms` / `feeder_retry_ms` | `5000` / `2000` |
| `outbound_lease_ms` / `inbound_lease_ms` | `90000` / `90000` |
| `stage_batch_limit` / `control_batch_limit` | `100` / `100` |
| `update_limit_bytes` / `input_text_limit_bytes` | `1048576` / `60000` |
| `callback_ttl_ms` / `callback_busy_timeout_ms` | `300000` / `5000` |
| `feeder_registration_limit` / `feeder_prompt_limit_bytes` | `1024` / `64000` |
| `feedback_ack_buffer_limit` | `100` |

Connection timeouts cannot exceed their request deadlines. Polling slack must
cover the normal connection timeout, and initial startup backoff cannot exceed its
maximum. API batch and attachment ceilings remain enforced. Server `retry_after`
takes priority over local backoff settings; uncertain sends are never retried
automatically.

While the service is running, policy reloads, feedback/progress cadence, cosmetic
action expiry and polling/sending retry waits use monotonic elapsed time, so
system-clock adjustments do not shorten or extend those waits. The journal
retains UTC timestamps and retry deadlines for
restart recovery; after a restart, a saved deadline is evaluated against the
current wall clock. An elapsed local send wait releases only its matching
known-not-sent attempt, and the next attempt rechecks recipient authorization.
Feeder readiness retries retain durable UTC deadlines, including across service
restarts. Changing either attempt lease never permits replay of an expired,
uncertain attempt. An exact correlated outbound receipt can still settle its
original attempt after expiry; a stale attempt cannot settle its replacement.

Confirmation lifetime applies to newly issued persisted claims. Reloading with
a longer lifetime does not revive an expired existing confirmation. Identity,
actor/chat authorization, one-use consumption and uncertain-outcome retention
remain required. The confirmation store uses its saved SQLite busy timeout.
Reduce selected registrations before lowering the feeder registration limit;
the service schema retains its 1024-registration ceiling. The encoded prompt
budget covers the complete escaped transport envelope, not just message text;
an oversized envelope is refused before reserving terminal readiness.

The saved attachment budget also controls photo-variant selection and checks
downloaded bytes before storage. A smaller allowed variant can be selected when
the largest exceeds that budget. Text and individual-update budgets are separate:
oversized input text is recorded as unsupported input after admission, while an
oversized individual update refuses admission and does not advance its cursor.

During offline cutover, the effective final service's `file_limit_bytes` also
governs pending attachment relocation. Verified private files retain their digest
basename and safe extension; missing, invalid or disallowed files become blocked
drafts. Separate source-instance controls bound attachment reference count,
combined reference bytes and queued-frame size. Those quota failures refuse the
operation before publication, so raise the source setting and review again.
Uncertain rows and receipts remain evidence without replay. Relocating a pending
file preserves unrelated payload metadata and changes its path and payload digest.

Feedback actions are cosmetic: a full queue or expired action is discarded,
and a failed action is never replayed. The worker honors the complete server
retry delay. Typing, queue reactions and startup command-menu registration can
be disabled independently. Disabling registration does not delete an already
registered Telegram command menu.

Streaming limits apply to the service and reviewed `/stream` start operations.
New streams exceeding the active subscription limit are refused; an existing
stream can restart without consuming another slot. Stop surplus subscriptions
before lowering that limit. Pruning old accepted-prompt provenance can suppress
output; it never selects a historical prompt as a fallback. Oversized text events
show `stream_text_limit_exceeded` in stream status. Unicode chunks retain whole
characters, with a configurable minimum of two UTF-16 units and the existing
4000-unit ceiling. Stream text keeps the existing 64000-byte ceiling. Exact
prompt, conversation, recipient and subscription-epoch checks remain required.
`stream_message_limit_per_event` bounds the number of messages produced by one
event, including when very small chunks are selected. An event exceeding that
budget queues no chunks and reports `stream_message_limit_exceeded` in stream
status. Increase the chunk size or select a suitable message budget; the default
16-message limit covers the existing 64000-byte/4000-unit defaults.

## Messages and feedback

| Feature | Behavior |
| --- | --- |
| Text and captions | Sent to the authorized destination conversation. |
| Photos | Largest variant within the saved attachment limit, up to 20 MiB; stored privately for the harness to open. |
| Documents | Incoming files within the saved attachment limit, up to 20 MiB; outbound file and media delivery is unavailable. |
| Voice, audio, video and stickers | Media is ignored; an accompanying caption can be used. |
| Replies | Verified assistant text, split at Telegram's message limit; plain text only. |
| Receipt | 👀 means the message was saved to the queue, not processed by the agent. |
| Typing | Refreshed while verified hooks report the conversation busy. |
| Progress | One message edited in place per binding. |
| Optional streams | Assistant text or tool names where supported; no raw console mirror, tool arguments, results or hidden reasoning. Grok live streaming is unavailable. |

Input waits until verified hooks report that the selected conversation is ready.
A replaced terminal, changed route or revoked grant blocks delivery rather than
redirecting the message. An uncertain submission or send is kept for review and
is not automatically replayed. Cosmetic reactions and typing are best effort.

Remote controls include `/status`, `/windows`, `/use`, `/say`, `/capture`,
`/interrupt`, `/stop`, checkpoints and supported goals. Permissions and harness
capabilities apply to every command. Use the [command reference](COMMANDS.md#telegram)
for the full roster, aliases and streaming controls.

## If a message stays queued

1. Check that this instance's Telegram service is running and that the sender and
   chat are authorized.
2. Confirm the chat's destination and reply binding. Open the intended native
   conversation; a launcher or home screen may not have a session ID.
3. Check hook diagnostics. Approve or reload the hooks in the harness as required,
   then complete one normal native turn to establish readiness.
4. After relaunching a harness, reconnect its hooks to the current process and
   conversation. See [OpenCode relaunches](HOOKS.md#opencode-conversations-and-relaunches) and
   [Windows Hermes activation](HOOKS.md#hermes-on-windows).
5. Inspect Activity to distinguish queued, submitted, delivered and uncertain
   outcomes. Review uncertain work before retrying it.

Windows hook support varies by harness. Check the
[compatibility matrix](HARNESSES.md#native-windows-boundaries) before configuring a
new destination. A terminal appearing in the inventory does not prove Telegram
readiness.
