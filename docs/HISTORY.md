# Control action history

**Settings → Control action history** manages the selected instance's receipts.
Choose **Global dashboard history** to manage receipts for global preference
changes, which may use a different directory. The dialog always shows that
directory. Policy lives in private `control-history.json`; it also works without
a `control.json`, including during first-run setup.

| Setting | Default | Range |
| --- | --- | --- |
| `entry_limit` | 1000 | 2–8192 |
| `byte_limit` | 1048576 | 4096–8388608 |
| `display_limit` | 50 | 1–1024 |
| `archive_scan_limit` | 1024 | 1–65536 |

Fields prefill their saved values. Ctrl+U clears a field. Invalid values return to
the field for correction. Each save has a separate review with Cancel selected
first, a private prior-policy backup and exact-byte checks. Policy changes take
effect on the next history write or read. A concurrent policy edit invalidates
the review. Lower limits must still fit protected receipts and reserved outcomes;
completed operations can rotate on a later write. Reading never prunes history.

Active entries live in private `control-audit.json`. Rotation removes whole
completed or rejected operations, including their attempted receipts. Attempted,
accepted, submitted and unconfirmed outcomes stay protected. Before starting an
effect, its attempted receipt reserves one entry and enough bytes for its outcome.
If protected history fills either budget, a new effect is refused before it
starts. A final receipt failure reports uncertainty and never retries the action.

**Recent receipts** and **Unresolved outcomes** offer pages at the saved display
limit. Select a receipt to inspect its operation, action, state and time. Follow
**Next page** to see older receipts. Names, target paths, user text, provider URLs,
credentials and native error text are excluded from the receipt schema.

**Archive active receipts** reviews an exact private archive before starting a
new empty active history. The review shows the archive filename, pending and
protected counts. Cancel leaves the active file unchanged. Archiving retains
uncertain states; it does not establish completion or retry effects. In-flight
actions can still finish after an archive; a late receipt remains unconfirmed
if recording it fails. The archive retains the earlier attempt in that case.
**Saved receipt archives** lists the private archives in this directory. Select
one to browse its receipts and inspect their original outcomes. The viewer marks
archive records separately from active history and verifies the file's digest
before reading it. Modified files, links and filenames outside the archive
namespace are refused. Catalogs examine at most `archive_scan_limit` directory
entries; a partial listing says that some archives may be omitted. Raise the scan
limit or use an exact archive filename in the CLI to inspect an omitted file.
Archives are never removed automatically. A stale original or conflicting archive
refuses replacement.

If policy cannot be decoded, history review remains available using explicitly
labelled viewing defaults. Effect writes still require a valid policy and enough
protected capacity. Settings shows an **unsaved recovery draft**. Edit the limits,
then choose **Review policy recovery**. A separate **Recover history policy?**
review has Cancel selected first. Read-only receipt review retains that draft
while the policy bytes stay unchanged. Cancel or a scope change discards it;
a changed policy is reported and the draft is reloaded. Apply keeps the exact original policy in a
private backup and saves the draft only if protected receipts and reserved
outcomes fit. Recovery also allows multiple limits to be raised together when
the existing policy cannot fit protected history. Reading and cancellation never
reset policy or receipts.

If the active receipts cannot be decoded, their counts are shown as unknown.
Archive their exact private bytes before recovering policy. The archive retains
those unknown bytes; the receipt viewer refuses an unrecognized schema instead
of displaying arbitrary contents. Existing healthy archives remain reviewable
through the archive catalog and CLI even while active policy or receipts are
damaged. Policy recovery captures at most 1 MiB; normal policy files remain
limited to 8 KiB. Unsafe private-file references or larger files require local
repair and are never silently replaced.

The CLI provides the same settings and pages:

```sh
daedalus history config show --config-dir /abs/instance
daedalus history config set --config-dir /abs/instance --key entry_limit --value 2000
daedalus history config set --config-dir /abs/instance --key entry_limit --value 2000 --apply
daedalus history list --config-dir /abs/instance --unresolved
daedalus history list --config-dir /abs/instance --unresolved --offset 50
daedalus history archives --config-dir /abs/instance
daedalus history list --config-dir /abs/instance --archive ARCHIVE_FILENAME --unresolved
daedalus history archive --config-dir /abs/instance
daedalus history archive --config-dir /abs/instance --apply --expect-sha REVIEW_SHA
daedalus history config recover --config-dir /abs/instance --entry-limit 2048 --byte-limit 2097152
daedalus history config recover --config-dir /abs/instance --entry-limit 2048 --byte-limit 2097152 --apply --expect-sha REVIEW_SHA
```

Archive and recovery application require the digest from their preview. Settings preview by
default; `--apply` prepares and saves the current setting change. File-format
bounds are independent of saved policy: at most 8192 entries and 8 MiB remain
readable when a lower operational limit is selected. Invalid policy or receipt
data remains unchanged until an explicit guarded recovery or archive.

Instance import copies portable history policy and retains current source
receipts as an exact archive with their original states. It starts a separate
active history for destination actions. Existing source archives remain with the
source instance and are identified as such in the import notices.
