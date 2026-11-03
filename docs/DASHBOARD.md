# Dashboard preferences and instance discovery

Open **Settings → Dashboard and instance discovery** to edit global preferences.
The dialog shows the preference directory. It remains the same when managing a
different instance. Fields prefill saved values; Ctrl+U clears a field. Each edit
has a before/after review with Cancel selected first. Discovery changes apply on
the next refresh.

Preferences live in private `dashboard.json` at the platform's default preference
directory. They are separate from the selected instance's `control.json`.
Unrelated dashboard keys are preserved without being included in public reviews.
The [example](../config/dashboard.example.json) contains all discovery defaults.

| Discovery setting | Default | Purpose |
| --- | --- | --- |
| `scan_default_roots` | `true` | Include platform configuration roots |
| `scan_processes` | `true` | Discover owned Linux native service candidates |
| `instance_limit` | `128` | Bound distinct candidates and saved-directory checks |
| `root_limit` | `16` | Bound distinct configuration roots examined |
| `entries_per_root` | `1024` | Bound entries examined per root |
| `process_limit` | `4096` | Bound process-directory entries examined |
| `argv_limit_bytes` | `8192` | Bound each candidate command line |
| `search_roots` | `[]` | Additional absolute, symlink-free roots |

Explicit search roots take priority and remain selected when default scanning is
disabled. Roots are scanned one level deep. Candidate limits always retain the
current instance. Limit warnings identify partial discovery; a missing result
does not prove that an instance is stopped. During a running dashboard refresh,
invalid preferences disable automatic instance discovery and retain the current
instance with a warning. If startup preferences are unreadable, Daedalus opens the
default private instance instead of using a remembered selection. Original
preferences stay untouched. An explicit `--config-dir` still selects that instance;
an invalid `control.json` remains an error.

Saved-directory checks include the preference directory itself, matching Windows
service/update consumers. Duplicate selections consume one slot. Raise the limit
or forget saved entries before exceeding that capacity. Removing a saved entry
updates dashboard references; its configuration files remain in place. Forgetting
the remembered startup selection clears that selection. The existing instance
picker records a new startup selection when an instance is selected.

Arbitrary Windows process discovery is unavailable. Configured and saved
directories still receive explicit owner checks. A process name, command line,
saved PID or configuration file is a candidate only. Running status requires the
verified executable, process incarnation and service ownership.

The CLI exposes the same global preferences:

```sh
daedalus dashboard config show --config-dir /abs/preferences
daedalus dashboard config set --config-dir /abs/preferences --key scan_processes --value false
daedalus dashboard config set --config-dir /abs/preferences --key scan_processes --value false --apply
daedalus dashboard config add-root --config-dir /abs/preferences --path /abs/configurations --apply
daedalus dashboard config remove-root --config-dir /abs/preferences --path /abs/configurations --apply
daedalus dashboard config add-instance --config-dir /abs/preferences --path /abs/instance --apply
daedalus dashboard config remove-instance --config-dir /abs/preferences --path /abs/instance --apply
daedalus dashboard config recover --config-dir /abs/preferences
```

The directory argument selects the preference directory. Preview is the default;
`--apply` saves the frozen review. Adding an instance validates its private native
configuration before saving the reference. It does not change the current
startup selection. These commands do not launch a terminal, service or provider.

Preferences are limited to 64 KiB after serialization as well as on load. A save
that would make a readable file exceed that limit is refused before replacement.
Manual edits use a shared configuration lock, private prior-byte backups and exact
byte checks; a concurrent selection or preference edit invalidates the review.
An unchanged save does not rewrite the file or add a backup. Instance selection
also uses the lock and bounded encoder, preserving unrelated keys.

If preferences are damaged, **Settings → Dashboard and instance discovery** offers
**Recover dashboard preferences?** with Cancel selected first. The review explains
that recovery replaces all dashboard keys, clears saved instance references and
startup selection, and disables automatic instance scanning. It shows the backup
path and replacement settings without displaying the damaged contents. Only an
explicit **Back up and recover** saves the replacement. Existing instance
configurations, native accounts and terminals remain in place. Configure discovery
again afterwards to enable the sources you want.

`dashboard config recover --config-dir /abs/preferences` previews the same recovery;
add `--apply` to save. Recovery accepts damaged private regular files owned by the
current user, up to 1 MiB, so oversized legacy preferences can be backed up exactly.
Files larger than that, unsafe file references, a busy configuration lock,
conflicting backups or changes since review are refused without replacement.
Healthy and missing preferences use normal editing instead. Cancellation and
startup never reset the file automatically.

Instance import does not copy a source dashboard's global preferences. Final
cutover retains an existing destination dashboard file byte-for-byte and checks
for changes after review before retiring the source. Rollback restores the
destination's prior preferences with its other private files.
