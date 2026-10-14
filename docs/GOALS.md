# Goals

A goal is a directive bound to one hook registration. When the agent stops, the
goal can ask it to continue, using the tool's own continuation mechanism. Arming a
goal does not start an agent or route Telegram.

| State | Meaning |
| --- | --- |
| Armed / Waiting | Waiting for a matching hook |
| Pending | Continuation requested, not yet confirmed |
| Running | The tool acknowledged the continuation |
| Paused | Suspended; later events do not resume it |
| Complete | Done; no further continuation |
| Blocked | Timed out or rejected; needs review |

```bash
daedalus goal arm --registration /abs/registration.json < directive.txt
daedalus goal status --registration /abs/registration.json
daedalus goal pause --registration /abs/registration.json
daedalus goal resume --registration /abs/registration.json
daedalus goal complete --registration /abs/registration.json
```

Telegram has the same controls through `/goal`.

## Rules

- The terminal, process start, conversation and directive version must all match.
- A duplicate event cannot trigger a second continuation, and a stale
  acknowledgement cannot mark a newer directive Running.
- Pause and complete win over delayed or replayed events.
- After a restart, state is reconciled; no request is re-sent blindly.
- A pending continuation that times out becomes Blocked.
- Removing the hook profile disables the goal.

## Per tool

| Tool | Continuation |
| --- | --- |
| Claude Code | Stop-hook block response |
| Codex | Native hook response |
| Cursor | Bounded follow-up message at the end of a generation |
| Pi | `agent_before_settle`; `agent_settled` marks readiness |
| Grok 1.0.46 | Matching end-turn reason, prompt ID and stop-hook state; Grok's own continuation cap applies, then the goal becomes Blocked |
| Hermes, OpenCode | Not available |

A Grok Stop event does not prove the agent is idle, because Grok may continue the
turn. Its readiness stays Unknown.
