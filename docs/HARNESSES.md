# Agent tools

Daedalus runs seven agent tools ("harnesses") in tmux or Herdr terminals. All seven
get terminal discovery, launch, sign-in through the tool's own flow, a selectable
profile directory and terminal controls. Credentials stay with each tool; a local
signed-in indicator does not prove the account can use a given model.

| Tool | Hook profile | Guided first launch with hooks | Goal continuation |
| --- | --- | --- | --- |
| Claude Code | JSON matcher groups, 2.1.295+ | Yes | Stop-hook block |
| Codex | `hooks.json`; approve it in Codex's `/hooks` trust review | No; register a running process | Native hook response |
| Grok | Native lifecycle; goals need 1.0.46 | Yes | End-turn gated |
| Hermes | YAML shell hooks | No; register a running process | Not available |
| Cursor | `hooks.json` version 1 | No; register a running process | Bounded follow-ups |
| Pi | Extension bridge, 0.87+ or 1.1.x | Yes, 1.1.x | Before-settle |
| OpenCode | Local server plugin, 2.0.28+ | No; register the server and its terminal process | Not available |

A plain launch does not register hooks or route Telegram. To hook a running
process, register it on the Hooks page; the registration must match its process
start, terminal generation and conversation. See [hooks](HOOKS.md) and
[goals](GOALS.md).

## Sessions

Claude Code, Codex, Grok and Pi support checkpoint resume, fork and cross-tool
handoff. Resume and fork are offered only when the installed tool's `--help` lists
the needed option. Pi uses its session file and header. A resumed or forked process
needs its own hook registration. Handoff passes reviewed context to another tool; it
does not copy the first tool's internal state.

## Readiness

Daedalus marks an agent idle only from verified events: Cursor and Hermes from
correlated turn events, OpenCode from its server bridge, Pi from `agent_settled`.
A Grok Stop event leaves readiness Unknown because Grok may continue the turn.

Telegram and `terminal control` slash commands need Claude Code 2.1.296, the exact
live process and an idle registration. They submit the command; they do not confirm
that a setting changed.

## AGY

AGY (`agy`) shows up in the inventory and can be watched, attached, previewed and
controlled through tmux or Herdr. Start it with its own CLI. It has no Daedalus
sign-in, hooks, readiness or resume/fork support.

## Native Windows boundaries

Windows x64 runs the same TUI and native operations. Herdr is the default;
compatible native multiplexers can be configured explicitly with
`DAEDALUS_TMUX_BIN` and endpoint selection. Detection of a terminal backend does
not establish its hook or process-correlation contract.

| Windows operation | Current boundary |
| --- | --- |
| Ordinary terminal launch/watch/capture/control | Native executable and exact backend identity required |
| Guided first launch with hooks | Unavailable; register an existing verified process |
| Existing Claude Code hooks | Native Herdr registration, audited 2.1.296+ contract |
| Existing Hermes, Pi and OpenCode hooks | Native Herdr registration with provider-specific proof; OpenCode pins server and UI separately |
| Codex, Grok and Cursor hooks | Unverified on Windows; registration/launch fails closed |
| Hooks through native tmux-compatible backends | Unverified; fail closed rather than infer identity |
| Herdr duplicate attachment view | Unavailable; use read-only Watch |
| Native session resume/fork | Still requires installed capability and exact provider identity; no blanket Windows-provider claim |

Hook availability is implementation/contract coverage, not evidence of live
provider acceptance. Linux tests and Windows cross-compilation do not validate
Windows terminal/ConPTY behavior on real hardware. See [Windows installation and
local checks](INSTALL.md#native-windows-install).

## Upstream references

- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
- [Codex](https://github.com/openai/codex)
- [Grok hooks](https://docs.x.ai/build/features/hooks)
- [Hermes hooks](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/hooks.md)
- [Cursor hooks](https://cursor.com/docs/hooks)
- [Pi extensions](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md)
- [OpenCode plugins](https://opencode.ai/v2/docs/build/plugins)
