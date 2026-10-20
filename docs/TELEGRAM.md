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
running; stop it explicitly through the Telegram menu when needed.

```sh
daedalus telegram status --config-dir /absolute/private/instance
```

The TUI offers reviewed start and stop actions. Local automation can preview
`daedalus telegram start|stop`, then apply with the returned approval digest.
These actions request a transition; check status to confirm the result.
`daedalus telegram serve --config-dir ABS` runs the service in the foreground.
See [the command reference](COMMANDS.md#telegram) for complete syntax.

## Messages and feedback

| Feature | Behavior |
| --- | --- |
| Text and captions | Sent to the authorized destination conversation. |
| Photos | Largest available size, up to 20 MiB; stored privately for the harness to open. |
| Documents | Incoming files up to 20 MiB; outbound file and media delivery is unavailable. |
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
