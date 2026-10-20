# Security

Report vulnerabilities privately to the repository owner. Do not put credentials
or private session content in public issues.

Keep credentials, transcripts, token files and runtime databases in the private
configuration directory, outside any repository.

## Authorization and identity

- Telegram sender grants, chat policy, routes and reply recipients are separate,
  explicit settings. Seeing a sender does not grant it anything, and forwarded
  messages cannot issue commands.
- Terminal changes require the exact backend, endpoint and pane generation, plus the
  process start and conversation where relevant. Display names are not identities.
- Queued effects recheck authorization and the route before dispatch. One-time
  confirmations are bound to the sender, chat and reviewed target. Revoking a
  permission cannot recall a request already sent.

## Hooks, streams and capture

- A hook registration pins one live tool process, terminal and conversation. Hooks
  are not a sandbox for the agent's tools.
- Streams are tied to the accepted Telegram input by a hash and to the tool's turn.
  The hash is a correlation check, not a signature. Missing or ambiguous evidence
  stops the stream.
- Streams send assistant text or tool names only: never tool arguments, results,
  hidden reasoning or console output.
- Capture, watch and attach show terminal output and can expose private text.
- Recalled memory is reference data, not instructions.

## External effects and installation

- An unknown Telegram send result is kept as uncertain and never re-sent.
- Profile changes use reviewed digests and private backup and rollback receipts.
  Files edited since the review are not overwritten.
- Dependency installers are third-party code. Sign-in stays with each tool; a local
  credential file does not prove the provider accepts it.
- Gateway, Herdr and Hindsight endpoints are explicit configuration. Do not expose
  private control sockets or memory services without their own access controls.

## Limits

Security tests cover authorization, stale-identity rejection, replay handling and
private-file behavior. If credentials enter version control, rotate them and
remove them from both the current tree and Git history.
