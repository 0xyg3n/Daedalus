# Repository instructions

Daedalus is one Rust executable (`src/tui`) that coordinates existing agent tools.
Document only what the code does and tests cover.

## Layout

- `src/tui/src/app/`: shared operations, CLI and backend, Telegram, setup, hooks.
- `src/tui/src/core/`: identity, lifecycle, configuration and durable stores.
- `src/tui/src/adapters/`: terminal, tool and transport boundaries.
- `src/tui/src/*.rs`: the Ratatui interface; keep its look and navigation.
- `config/`: public examples only.
- `docs/COMMANDS.md`: command reference, compiled into the binary.

## Rules

1. Check imports, callers, configuration and service references before deleting code.
2. Test with isolated fixtures. Never start a live bot, send messages, relaunch user
   sessions or make paid model requests.
3. Keep accounts, endpoints, terminal bindings and identities configurable. Never
   commit credentials, transcripts, runtime databases or deployment-specific names.
4. Keep tool, provider, account, process, terminal and conversation identities
   separate. Reject ambiguous or stale observations.
5. Queued is not delivered; submitted is not completed. Keep uncertain external
   effects for review; do not retry them automatically.
6. A new adapter needs authorization, exact routing, event correlation and delivery
   evidence. Unsupported provider contracts must fail explicitly.
7. Do not claim live-provider compatibility from mocks.
8. Backend changes do not authorize a TUI redesign.
9. Before publishing, run `./scripts/check.sh`, the affected tests and a credential
   scan of the current tree ([testing](docs/ARCHITECTURE.md#testing)).
10. Ship user-facing changes with a new version and GitHub release: update VERSION,
    Cargo package/lockfile and release links, run checks, and publish matching
    Linux and Windows artifacts with checksums. Do not reuse a published version.
