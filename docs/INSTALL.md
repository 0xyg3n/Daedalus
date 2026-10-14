# Installation

Daedalus is one `daedalus` executable for Linux x86_64 and ARM64. It is the TUI,
the CLI, the Telegram service and the hook runtime. Agent tools are installed
separately and keep their own runtimes and accounts. macOS and Windows are not
supported.

## Install

```bash
git clone https://github.com/0xyg3n/Daedalus.git
bash Daedalus/install.sh --source "$PWD/Daedalus"
~/.local/bin/daedalus setup
```

The installer builds the committed `HEAD` of `--source`; uncommitted edits are not
installed. It stores the binary at `PREFIX/share/daedalus/native/SHA256/daedalus`
and links `PREFIX/bin/daedalus` to it. The default prefix is `~/.local`
(`DAEDALUS_PREFIX` overrides it).

| Option | Effect |
|---|---|
| `--plan` | Print the plan and exit without changes |
| `--yes` | Accept the plan without prompting |
| `--prefix ABS` | Install under another prefix |
| `--ref REF` | Build this revision. Without `--source`, the installer clones `main` (or `DAEDALUS_REF`) |
| `--no-deps` | Fail instead of installing missing build tools |
| `--binary ABS --sha256 HEX` | Install a prebuilt binary after checking its checksum and ELF architecture |

Building needs Rust 1.88+, a C linker, Git and curl. After you approve, the
installer adds missing build tools with apt, dnf or pacman, and Rust with the
official rustup script (it prints the script's SHA-256 first). It never replaces a
`daedalus` command it did not install. A checksum proves integrity only if you got
it from a source you trust. ARM64 builds have not been tested on real hardware.

## First run

`daedalus setup`, or the first `daedalus` launch, opens setup in the TUI.
`daedalus setup --cli` asks the same questions as prompts. Setup:

1. chooses tmux, Herdr or both as terminal backends;
2. selects agent tools and lists the missing ones it would install;
3. writes a private configuration directory (mode 0700).

Each tool signs in through its own login flow. Cancelling setup saves nothing.
**Finish and explore terminals** saves the configuration without installing
anything or signing in. Setup never starts the Telegram service.

Missing tools are installed only after review:

- tmux, Node.js and npm: the system package manager.
- Claude Code, Codex, Pi and OpenCode: npm, into `~/.local`.
- Herdr, Cursor, Grok and Hermes: their official install scripts over HTTPS.

Daedalus also finds OpenCode in `~/.opencode/bin`. Upstream installers are
third-party code that Daedalus does not control.

Next, open `daedalus` and start a shell or agent in a workspace ([control panel](TUI.md)).
Telegram is configured separately ([commands](COMMANDS.md#telegram)).

## Configuration directory

Daedalus uses the first of: `--config-dir`, `DAEDALUS_CONFIG_DIR`,
`$XDG_CONFIG_HOME/daedalus`, `~/.config/daedalus`. Keep it outside Git. The files
are described in [config/README.md](../config/README.md).

To copy an existing instance into a new directory, run
`daedalus config import-instance --config-dir NEW --from OLD` (preview; `--apply`
writes, `--credentials` also copies token files). If the old instance ran the
Telegram service, stop it and hand over with `daedalus telegram cutover preview|apply`.

## Official component sources

| Component | Installer or distribution reference |
|---|---|
| tmux | [github.com/tmux/tmux](https://github.com/tmux/tmux/wiki/Installing) |
| Herdr | [herdr.dev](https://herdr.dev/) |
| Claude Code | [Anthropic setup](https://code.claude.com/docs/en/setup) |
| Codex | [OpenAI Codex](https://github.com/openai/codex) |
| Grok CLI | [xAI build documentation](https://docs.x.ai/build/overview) |
| Hermes Agent | [Nous Research documentation](https://hermes-agent.nousresearch.com/docs/) |
| Cursor | [Cursor CLI](https://cursor.com/docs/cli/overview) |
| Pi | [Pi installation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/README.md) |
| OpenCode | [OpenCode installation](https://opencode.ai/docs/) |
| Node.js | [Official distributions and checksums](https://nodejs.org/dist/) |

## Update and removal

`daedalus update check [--json]`, or **AI tools → Check for updates** in the panel,
reports whether a newer GitHub release exists. It installs nothing. To upgrade, run
the installer again. Older releases stay under `PREFIX/share/daedalus/native/`;
installing an older revision relinks it. Restart a running Telegram service after
switching binaries.

To remove Daedalus, stop its Telegram service, then delete `PREFIX/bin/daedalus`
and `PREFIX/share/daedalus/`. The configuration directory stays until you delete
it. Agent tools and system packages are not removed.
