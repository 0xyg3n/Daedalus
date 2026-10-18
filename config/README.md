# Configuration

Daedalus keeps each instance's settings and state in a private configuration
directory outside Git (see [installation](../docs/INSTALL.md#configuration-directory)).
Create it with `daedalus setup` or
`daedalus config init --config-dir ABS --workspace ABS --apply`.
[control.example.json](control.example.json) shows the `control.json` schema;
replace the placeholder paths before running `daedalus config validate --from FILE`.

| File | Contents |
| --- | --- |
| `control.json` | Workspace, tool subscriptions, profile directory per tool, tmux sockets and Herdr discovery endpoints |
| `telegram-service.json` | Bot ID, token file, sender grants, feeder registrations and reply recipients (`daedalus telegram config --help`) |
| `hindsight.json` | Optional Hindsight endpoint, credential file and one bank per hook registration |
| `gateway.json` | Optional endpoint (HTTPS, or HTTP on loopback) for Claude Code (Anthropic Messages) or Codex (OpenAI Responses), set under AI tools |
| `armonia.sqlite3` | Optional existing Armonia registry |

The same directory holds routes, deliveries, checkpoints, layouts, receipts and
Telegram attachments. Subscriptions are labels only; each tool keeps its own
account credentials. Setup and import show a preview and back up any settings they
replace.
