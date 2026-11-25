# Changelog

## 0.2.0-beta.63

Changes since the public 0.2.0-beta.51 prerelease.

### Workspaces and checkpoints

- Open **Tetris** with **F4** to see panes in their Herdr tab layout. The view was previously called Map. Pane titles show their IDs, and numbered activity indicators identify working, waiting and stale panes. Compare two workspaces, resize their views, or use **F11** to expand a pane and return to the same arrangement.
- Tetris follows changes to the native Herdr layout, including pane swaps and new splits. A separate swap action lets you rearrange the dashboard view. On Unix, leave an external native viewer with **F11** or **Ctrl+]**, then **D**, while the source terminal keeps running.
- Open the searchable action menu with **F2** or a right-click. Wide terminals show descriptions beside each action. **F5** saves a quick checkpoint. Named checkpoints can open beside the selected pane. Resume and fork use a saved session identity. A context-only checkpoint supports handoff.
- Choose from five color palettes and use the clickable shortcuts in the footer. Herdr users can preview and install a popup shortcut that opens Tetris from the current pane.

### Projects and agents

- **F6** opens Git review for the selected pane's repository. Inspect working and staged diffs, stage or unstage a path, commit the reviewed index locally, or create a branch in a separate worktree. A changed file, index or HEAD requires a fresh review.
- Git review preserves spaces in checkout paths and enforces file size limits while reading previews and preparing a stage action.
- **F7** creates a persistent task plan for identified agent conversations. Set dependencies and a parallel limit, then follow each task through submission and observed activity. After an interrupted or uncertain send, the worker holds the task for inspection.
- Connect the TUI to a configured remote POSIX instance over SSH. The header identifies the remote environment, host-key verification is required, and project operations and embedded terminals run on the selected host.
- Guided launches for new Codex and Claude panes now offer provider-native permission profiles while preserving the chosen account and model.

### Telegram

- Claude reply correlation recognizes the verified message envelope inside a pasted-content wrapper.

See the [TUI guide](docs/TUI.md) and [project controls guide](docs/TUI.md#project-workflows) for shortcuts, supported backends and recovery behavior.
