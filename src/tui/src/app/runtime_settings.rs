//! Saved local operation budgets. No account or process identities.
use super::interface_settings::{Kind, Setting};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub native_launch_cols: u16,
    pub native_launch_rows: u16,
    pub native_scrollback_rows: u64,
    pub shell_cols: u16,
    pub shell_rows: u16,
    pub terminal_query_timeout_ms: u64,
    pub terminal_query_limit_bytes: u64,
    pub discovery_workers: u64,
    pub discovery_endpoint_limit: u64,
    pub discovery_socket_scan_limit: u64,
    pub discovery_timeout_ms: u64,
    pub discovery_metadata_limit_bytes: u64,
    pub discovery_metadata_value_limit: u64,
    pub discovery_frame_limit_bytes: u64,
    pub discovery_relationship_limit: u64,
    pub discovery_relationship_limit_bytes: u64,
    pub discovery_projection_timeout_ms: u64,
    pub discovery_layout_pane_limit: u64,
    pub discovery_layout_limit_bytes: u64,
    pub discovery_layout_timeout_ms: u64,
    pub terminal_resize_pane_limit: u64,
    pub terminal_resize_max_cells: u64,
    pub terminal_resize_min_cols: u64,
    pub terminal_resize_min_rows: u64,
    pub terminal_view_history_limit: u64,
    pub terminal_view_history_limit_bytes: u64,
    pub terminal_view_history_value_limit: u64,
    pub terminal_view_history_timeout_ms: u64,
    pub native_discovery_entry_limit: u64,
    pub native_discovery_timeout_ms: u64,
    pub native_discovery_rpc_timeout_ms: u64,
    pub external_discovery_entry_limit: u64,
    pub external_discovery_timeout_ms: u64,
    pub external_discovery_query_limit: u64,
    pub harness_probe_timeout_ms: u64,
    pub harness_probe_limit_bytes: u64,
    pub native_login_timeout_ms: u64,
    pub native_login_output_limit_bytes: u64,
    pub database_snapshot_timeout_ms: u64,
    pub database_snapshot_limit_bytes: u64,
    pub database_snapshot_pages_per_step: u64,
    pub database_snapshot_busy_wait_ms: u64,
    pub database_snapshot_progress_ops: u64,
    pub instance_import_limit_bytes: u64,
    pub instance_import_entry_limit: u64,
    pub instance_import_json_value_limit: u64,
    pub instance_import_work_limit_bytes: u64,
    pub instance_import_timeout_ms: u64,
    pub instance_import_attachment_limit: u64,
    pub instance_import_attachment_limit_bytes: u64,
    pub instance_import_frame_limit_bytes: u64,
    pub checkpoint_file_limit_bytes: u64,
    pub checkpoint_work_limit_bytes: u64,
    pub checkpoint_entry_limit: u64,
    pub checkpoint_value_limit: u64,
    pub checkpoint_timeout_ms: u64,
}
impl Default for Settings {
    fn default() -> Self {
        let snapshot = crate::adapters::sqlite::SnapshotOptions::default();
        let checkpoints = crate::core::checkpoint_budget::Policy::default();
        Self {
            native_launch_cols: 120,
            native_launch_rows: 32,
            native_scrollback_rows: super::native_terminal::DEFAULT_SCROLLBACK_ROWS,
            checkpoint_file_limit_bytes: checkpoints.file_bytes,
            checkpoint_work_limit_bytes: checkpoints.work_bytes,
            checkpoint_entry_limit: checkpoints.entries,
            checkpoint_value_limit: checkpoints.values,
            checkpoint_timeout_ms: checkpoints.timeout_ms,
            shell_cols: 120,
            shell_rows: 36,
            terminal_query_timeout_ms: 5000,
            terminal_query_limit_bytes: 8_000_000,
            discovery_workers: 8,
            discovery_endpoint_limit: 1024,
            discovery_socket_scan_limit: 4096,
            discovery_timeout_ms: 15000,
            discovery_metadata_limit_bytes: 2097152,
            discovery_metadata_value_limit: 100000,
            discovery_frame_limit_bytes: 6291456,
            discovery_relationship_limit: 16384,
            discovery_relationship_limit_bytes: 2097152,
            discovery_projection_timeout_ms: 2000,
            discovery_layout_pane_limit: 4096,
            discovery_layout_limit_bytes: 2097152,
            discovery_layout_timeout_ms: 2000,
            terminal_resize_pane_limit: 128,
            terminal_resize_max_cells: 4096,
            terminal_resize_min_cols: 4,
            terminal_resize_min_rows: 3,
            terminal_view_history_limit: 200,
            terminal_view_history_limit_bytes: 8388608,
            terminal_view_history_value_limit: 100000,
            terminal_view_history_timeout_ms: 2000,
            native_discovery_entry_limit: 1024,
            native_discovery_timeout_ms: 1000,
            native_discovery_rpc_timeout_ms: 250,
            external_discovery_entry_limit: 65536,
            external_discovery_timeout_ms: 2000,
            external_discovery_query_limit: 512,
            harness_probe_timeout_ms: 2000,
            harness_probe_limit_bytes: 16384,
            native_login_timeout_ms: 120000,
            native_login_output_limit_bytes: 262144,
            database_snapshot_timeout_ms: snapshot.timeout.as_millis() as u64,
            database_snapshot_limit_bytes: snapshot.limit_bytes,
            database_snapshot_pages_per_step: snapshot.pages_per_step as u64,
            database_snapshot_busy_wait_ms: snapshot.busy_wait.as_millis() as u64,
            database_snapshot_progress_ops: snapshot.progress_ops as u64,
            instance_import_limit_bytes: 536870912,
            instance_import_entry_limit: 4096,
            instance_import_json_value_limit: 1000000,
            instance_import_work_limit_bytes: 2147483648,
            instance_import_timeout_ms: 120000,
            instance_import_attachment_limit: 256,
            instance_import_attachment_limit_bytes: 134217728,
            instance_import_frame_limit_bytes: 262144,
        }
    }
}
macro_rules! setting {
    ($key:ident, $label:literal, $group:literal, $description:literal, $min:literal, $max:literal) => {
        Setting {
            key: stringify!($key),
            label: $label,
            group: $group,
            description: $description,
            kind: Kind::Number {
                min: $min,
                max: $max,
            },
        }
    };
}
pub const SETTINGS: &[Setting] = &[
    setting!(
        native_launch_cols,
        "New terminal columns",
        "Native terminals",
        "Initial columns for native terminals created by workspace and checkpoint actions. Attach may resize the terminal.",
        2,
        512
    ),
    setting!(
        native_launch_rows,
        "New terminal rows",
        "Native terminals",
        "Initial rows for native terminals created by workspace and checkpoint actions. Attach may resize the terminal.",
        2,
        256
    ),
    setting!(
        shell_cols,
        "Shell columns",
        "Native terminals",
        "Initial columns for shell and terminal-native start with --config-dir. An explicit --cols takes precedence; attach may resize.",
        2,
        512
    ),
    setting!(
        shell_rows,
        "Shell rows",
        "Native terminals",
        "Initial rows for shell and terminal-native start with --config-dir. An explicit --rows takes precedence; attach may resize.",
        2,
        256
    ),
    setting!(
        terminal_query_timeout_ms,
        "Terminal query timeout (ms)",
        "Discovery",
        "Deadline for tmux and Herdr subprocess queries and actions using this instance. A timeout after a change leaves its outcome unconfirmed.",
        100,
        60000
    ),
    setting!(
        terminal_query_limit_bytes,
        "Terminal query output limit (bytes)",
        "Discovery",
        "Combined stdout and stderr budget for each tmux or Herdr subprocess query or action. Refused output remains unverified; a mutation is never retried automatically.",
        1024,
        8000000
    ),
    setting!(
        discovery_workers,
        "Terminal discovery workers",
        "Discovery",
        "Concurrent queries in each native or tmux/Herdr discovery stage. Stages run in sequence, workers take the next endpoint after finishing, and results retain stable ordering.",
        1,
        32
    ),
    setting!(
        discovery_endpoint_limit,
        "Terminal discovery endpoint limit",
        "Discovery",
        "Maximum tmux sockets and Herdr sessions queried per refresh, including explicit endpoints. Extra endpoints make discovery visibly incomplete. Queries follow sorted tmux paths, then sorted Herdr sessions.",
        1,
        8192
    ),
    setting!(
        discovery_socket_scan_limit,
        "Automatic socket scan entry limit",
        "Discovery",
        "Directory entries inspected across automatic tmux or psmux socket roots per refresh, including unrelated files. Reaching this bound with entries remaining makes discovery incomplete; explicit paths remain candidates.",
        1,
        65536
    ),
    setting!(
        discovery_timeout_ms,
        "Whole discovery deadline (ms)",
        "Discovery",
        "Cooperative elapsed budget shared by native registry, socket, endpoint and external-process discovery. Owned queries use the remaining time; expired results are refused. Filesystem and operating-system calls can finish later.",
        100,
        300000
    ),
    setting!(
        discovery_metadata_limit_bytes,
        "Discovery metadata budget (bytes)",
        "Discovery",
        "Total JSON volume admitted across wire replies, normalized records and diagnostics per refresh. Shared by all workers. Repeated representations count separately; root and final incomplete warnings reserve 1024 bytes. In-flight query buffers have their own bounded capacity.",
        4096,
        4194304
    ),
    setting!(
        discovery_metadata_value_limit,
        "Discovery metadata value limit",
        "Discovery",
        "Total JSON values and object keys admitted across wire replies and normalized records. Counting nested values before parsing bounds many tiny records. Reserves 128 values for the root and final warnings.",
        256,
        1000000
    ),
    setting!(
        discovery_frame_limit_bytes,
        "Terminal display size limit (bytes)",
        "Discovery",
        "Maximum serialized local TUI state frame, within the 8 MiB protocol ceiling. A limited display preserves compiled settings actions and exact admitted node IDs, reports PARTIAL, and never treats omitted records as absent. Lowering this does not change terminal authority.",
        1048576,
        8388608
    ),
    setting!(
        discovery_relationship_limit,
        "Relationship detail limit",
        "Display relationships",
        "Optional view-place details and nested, declared, checkpoint and route links admitted per tree projection. Base terminals and verified totals remain present. Exhaustion reports PARTIAL; omitted links do not prove absence.",
        1,
        262144
    ),
    setting!(
        discovery_relationship_limit_bytes,
        "Relationship detail budget (bytes)",
        "Display relationships",
        "Cumulative serialized JSON size of optional view details and relationship nodes per projection. Shared view details count once per exact terminal identity. Separate from discovery wire data and state-frame size; not a total RAM measurement.",
        1024,
        8388608
    ),
    setting!(
        discovery_projection_timeout_ms,
        "Relationship projection deadline (ms)",
        "Display relationships",
        "Cooperative deadline for optional relationship indexes and details. Base terminal construction still completes. Late optional details are omitted with PARTIAL; incomplete client observations never claim nobody else is typing.",
        1,
        60000
    ),
    setting!(
        discovery_layout_pane_limit,
        "Layout panes per group",
        "Layout geometry",
        "Maximum base panes whose complete geometry is projected for a tab or window. Larger groups retain all terminal identities but omit geometry with a discovery warning. Increase before raising the resize pane limit.",
        1,
        65536
    ),
    setting!(
        discovery_layout_limit_bytes,
        "Layout metadata budget (bytes)",
        "Layout geometry",
        "Cumulative serialized complete layout metadata admitted per tree projection. Size is checked before JSON materialization. Refused groups retain terminals and report PARTIAL; this is separate from the state-frame limit and total process RAM.",
        1024,
        8388608
    ),
    setting!(
        discovery_layout_timeout_ms,
        "Layout geometry deadline (ms)",
        "Layout geometry",
        "Cooperative elapsed budget for geometry indexing, validation, ordering and serialization. Late or unfinished group geometry is omitted with a discovery warning; base terminals remain present.",
        1,
        60000
    ),
    setting!(
        terminal_resize_pane_limit,
        "Resize panes per group",
        "Split resizing",
        "Maximum panes in a complete layout accepted for divider rendering and backend resizing. Both sides use this saved policy. Geometry projection may have a smaller separate limit.",
        2,
        65536
    ),
    setting!(
        terminal_resize_max_cells,
        "Resize movement limit (cells)",
        "Split resizing",
        "Maximum absolute native-cell movement in one drag or resize request. The frontend clamps movement and the backend independently checks the same saved limit.",
        1,
        65535
    ),
    setting!(
        terminal_resize_min_cols,
        "Minimum resized columns",
        "Split resizing",
        "Minimum native columns remaining on either side of a horizontal divider. This is separate from the screen space needed to draw a preview.",
        1,
        32767
    ),
    setting!(
        terminal_resize_min_rows,
        "Minimum resized rows",
        "Split resizing",
        "Minimum native rows remaining on either side of a vertical divider. Tiny previews may require selecting their terminal from the tree.",
        1,
        32767
    ),
    setting!(
        native_discovery_entry_limit,
        "Native registry entry limit",
        "Native discovery",
        "Directory entries inspected in the native terminal registry, including unrelated entries. Extra entries make discovery incomplete.",
        1,
        65536
    ),
    setting!(
        native_discovery_timeout_ms,
        "Native discovery deadline (ms)",
        "Native discovery",
        "Cooperative elapsed budget for native registry enumeration and authenticated metadata queries, capped by the remaining whole discovery deadline.",
        1,
        60000
    ),
    setting!(
        native_discovery_rpc_timeout_ms,
        "Native metadata query timeout (ms)",
        "Native discovery",
        "Deadline for each complete authenticated native metadata RPC, including connection, partial reads/writes and response checks. Capped by remaining stage and whole-refresh time.",
        1,
        60000
    ),
    setting!(
        external_discovery_entry_limit,
        "External process entry limit",
        "External discovery",
        "Entries inspected during display-only external process discovery. Linux counts all directory entries; Windows counts process snapshot rows. Excess entries refuse the incomplete external projection.",
        1,
        1048576
    ),
    setting!(
        external_discovery_timeout_ms,
        "External process discovery deadline (ms)",
        "External discovery",
        "Cooperative elapsed budget for display-only process enumeration and projection, capped by remaining whole discovery time. Operating-system calls can finish later; expired results are refused.",
        1,
        60000
    ),
    setting!(
        external_discovery_query_limit,
        "Windows external process query limit",
        "External discovery",
        "Maximum candidate Windows shell/terminal processes whose owned identity is queried during external discovery. Linux reads its bounded proc entries instead.",
        1,
        65536
    ),
    setting!(
        harness_probe_timeout_ms,
        "Account discovery timeout (ms)",
        "Discovery",
        "Deadline for each local account-status command. A failed or timed-out probe keeps sign-in unverified.",
        100,
        60000
    ),
    setting!(
        harness_probe_limit_bytes,
        "Account discovery output limit (bytes)",
        "Discovery",
        "Maximum output accepted from each local account-status command. Raw status output and account identities are never displayed.",
        1024,
        1048576
    ),
    setting!(
        native_login_timeout_ms,
        "Private sign-in timeout (ms)",
        "Native accounts",
        "Deadline for noninteractive native sign-in that accepts private stdin. A timeout leaves the outcome unconfirmed; interactive sessions keep their own lifetime.",
        100,
        900000
    ),
    setting!(
        native_login_output_limit_bytes,
        "Private sign-in output limit (bytes)",
        "Native accounts",
        "Maximum captured output for a private-stdin native sign-in command. Native output is not displayed.",
        1024,
        1048576
    ),
    setting!(
        terminal_view_history_limit,
        "Saved viewer history limit",
        "Terminals and discovery",
        "Newest viewer records retained after a confirmed viewer registration. Existing history is read without pruning; older records are removed only on the next registration. This never closes a terminal.",
        1,
        65536
    ),
    setting!(
        terminal_view_history_limit_bytes,
        "Viewer history file limit (bytes)",
        "Terminals and discovery",
        "Maximum serialized viewer history read or saved. Over-budget files remain unchanged and relationships are incomplete; raise the limit to inspect them. Reads never prune history.",
        4096,
        67108864
    ),
    setting!(
        terminal_view_history_value_limit,
        "Viewer history JSON values",
        "Terminals and discovery",
        "Cumulative JSON values and object keys admitted while reading and projecting or saving viewer history. Preflight runs before parsed values are allocated. Repeated representations count again.",
        128,
        8388608
    ),
    setting!(
        terminal_view_history_timeout_ms,
        "Viewer history processing deadline (ms)",
        "Terminals and discovery",
        "Cooperative time for viewer-history read, validation, identity resolution and projection or save admission. Filesystem calls can finish later. Late reads are refused without rewriting history.",
        1,
        60000
    ),
    setting!(
        database_snapshot_timeout_ms,
        "Database snapshot timeout (ms)",
        "Database snapshots",
        "Cooperative deadline for each database copied by instance import or cutover. Uses the source instance settings. Filesystem calls can finish later; an expired copy is refused.",
        1,
        900000
    ),
    setting!(
        database_snapshot_limit_bytes,
        "Database snapshot size limit (bytes)",
        "Database snapshots",
        "Maximum physical or logical database size for import and cutover, including committed WAL data. The supported maximum is 256 MiB.",
        4096,
        268435456
    ),
    setting!(
        database_snapshot_pages_per_step,
        "Database snapshot pages per step",
        "Database snapshots",
        "Pages copied before checking elapsed time and source growth again. Smaller steps check the budget more often.",
        1,
        4096
    ),
    setting!(
        database_snapshot_busy_wait_ms,
        "Database snapshot busy wait (ms)",
        "Database snapshots",
        "Pause between busy or locked SQLite copy steps, capped by the remaining snapshot deadline. This never retries an external action.",
        1,
        1000
    ),
    setting!(
        database_snapshot_progress_ops,
        "Database snapshot progress interval",
        "Database snapshots",
        "SQLite VM operations between cooperative deadline checks during metadata and integrity queries. Smaller values check the deadline more often.",
        1,
        1000000
    ),
    setting!(
        instance_import_limit_bytes,
        "Instance import retained bytes",
        "Instance import and cutover",
        "Total retained bytes admitted in each staged or compared instance snapshot, including metadata and cutover proof. Temporary atomic-write and SQLite scratch files are not a disk-usage guarantee.",
        4096,
        4294967296
    ),
    setting!(
        instance_import_entry_limit,
        "Instance import directory entries",
        "Instance import and cutover",
        "Maximum files and directories in each staged or compared snapshot, including metadata and retained route review. Exhaustion refuses publication without retry.",
        1,
        65536
    ),
    setting!(
        instance_import_json_value_limit,
        "Instance import JSON work values",
        "Instance import and cutover",
        "Cumulative object keys and values validated in structured staged JSON across capture, writes and repeated verification. Opaque JSONL and databases use byte work limits instead.",
        128,
        16777216
    ),
    setting!(
        instance_import_work_limit_bytes,
        "Instance import byte work",
        "Instance import and cutover",
        "Cumulative logical payload capture, staged writes, database copies and bytes read by snapshot hashing. Repeated verification consumes the same budget; this is not total filesystem IO or memory usage.",
        4096,
        34359738368
    ),
    setting!(
        instance_import_timeout_ms,
        "Instance import deadline (ms)",
        "Instance import and cutover",
        "Cooperative elapsed budget from captured source settings through preparation, cutover and final publication admission. Review time counts; filesystem operations and cleanup can finish later.",
        1,
        900000
    ),
    setting!(
        instance_import_attachment_limit,
        "Queued attachment reference limit",
        "Instance import and cutover",
        "Maximum verified pending or retryable attachment references relocated during cutover. Exhaustion refuses the operation; duplicate content references each count.",
        1,
        65536
    ),
    setting!(
        instance_import_attachment_limit_bytes,
        "Queued attachment reference bytes",
        "Instance import and cutover",
        "Combined bytes of verified pending attachment references relocated during cutover. Each reference counts, even for shared content. Exhaustion refuses publication and requires a fresh review.",
        1,
        4294967296
    ),
    setting!(
        instance_import_frame_limit_bytes,
        "Queued delivery frame bytes",
        "Instance import and cutover",
        "Maximum UTF-8 payload bytes per selected inbound queued frame before materializing its owned JSON and typed Frame during cutover. The shared JSON, byte work and deadline budgets also apply.",
        1024,
        8388608
    ),
    setting!(
        native_scrollback_rows,
        "Native terminal scrollback rows",
        "Native terminals",
        "History retained by newly launched native terminals. Zero disables retention. Existing sessions keep their launch limit; each viewer scrolls independently and scrolling does not send terminal input.",
        0,
        65536
    ),
    setting!(
        checkpoint_file_limit_bytes,
        "Checkpoint file byte limit",
        "Saved checkpoints",
        "Maximum input or encoded output bytes per checkpoint registry or removal index. Reads and writes preserve existing bytes on refusal.",
        4096,
        67108864
    ),
    setting!(
        checkpoint_work_limit_bytes,
        "Checkpoint byte-work limit",
        "Saved checkpoints",
        "Cumulative bytes read and serialized while processing canonical/legacy checkpoints, removal entries and checkpoint observations in one operation. This is logical work, not a hard RAM or physical I/O limit.",
        4096,
        1073741824
    ),
    setting!(
        checkpoint_entry_limit,
        "Checkpoint entry limit",
        "Saved checkpoints",
        "Combined raw checkpoint and removal entries admitted in one operation, before duplicate collapse. A new save/removal must also fit; existing evidence is never pruned to fit.",
        1,
        1048576
    ),
    setting!(
        checkpoint_value_limit,
        "Checkpoint JSON value limit",
        "Saved checkpoints",
        "Cumulative structured JSON values and object keys in checkpoint reads, normalization and serialization. Duplicate keys and conflicting profile aliases are refused.",
        128,
        16777216
    ),
    setting!(
        checkpoint_timeout_ms,
        "Checkpoint processing deadline (ms)",
        "Saved checkpoints",
        "Cooperative processing deadline for checkpoint reads, normalization and write admission. Discovery/import also honor their own earlier deadline; blocking OS calls are not preempted.",
        1,
        60000
    ),
];
impl Settings {
    pub fn checkpoint_policy(&self) -> crate::core::checkpoint_budget::Policy {
        crate::core::checkpoint_budget::Policy {
            file_bytes: self.checkpoint_file_limit_bytes,
            work_bytes: self.checkpoint_work_limit_bytes,
            entries: self.checkpoint_entry_limit,
            values: self.checkpoint_value_limit,
            timeout_ms: self.checkpoint_timeout_ms,
        }
    }
    pub fn resize_policy(&self) -> crate::core::geometry::ResizePolicy {
        crate::core::geometry::ResizePolicy {
            pane_limit: self.terminal_resize_pane_limit,
            max_cells: self.terminal_resize_max_cells,
            min_cols: self.terminal_resize_min_cols,
            min_rows: self.terminal_resize_min_rows,
        }
    }
    pub fn snapshot_options(&self) -> crate::adapters::sqlite::SnapshotOptions {
        crate::adapters::sqlite::SnapshotOptions {
            timeout: std::time::Duration::from_millis(self.database_snapshot_timeout_ms),
            limit_bytes: self.database_snapshot_limit_bytes,
            pages_per_step: i32::try_from(self.database_snapshot_pages_per_step)
                .unwrap_or(i32::MAX),
            busy_wait: std::time::Duration::from_millis(self.database_snapshot_busy_wait_ms),
            progress_ops: i32::try_from(self.database_snapshot_progress_ops).unwrap_or(i32::MAX),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        for setting in SETTINGS {
            let Kind::Number { min, max } = setting.kind else {
                unreachable!()
            };
            let value = self
                .value(setting.key)?
                .as_u64()
                .ok_or("Invalid runtime number")?;
            if !(min..=max).contains(&value) {
                return Err(format!("{} must be between {min} and {max}", setting.label));
            }
        }
        Ok(())
    }
    pub fn value(&self, key: &str) -> Result<Value, String> {
        let key = key.strip_prefix("runtime.").unwrap_or(key);
        if !SETTINGS.iter().any(|setting| setting.key == key) {
            return Err("Unknown local runtime setting".into());
        }
        Ok(serde_json::to_value(self).map_err(|_| "Cannot encode runtime settings")?[key].clone())
    }
    pub fn display(&self, key: &str) -> Result<String, String> {
        Ok(self.value(key)?.to_string())
    }
    pub fn set(&self, key: &str, text: &str) -> Result<Self, String> {
        let key = key.strip_prefix("runtime.").unwrap_or(key);
        self.value(key)?;
        let number = text
            .trim()
            .parse::<u64>()
            .map_err(|_| "Enter a whole number")?;
        let mut value = serde_json::to_value(self).map_err(|_| "Cannot encode runtime settings")?;
        value[key] = Value::from(number);
        let updated: Self = serde_json::from_value(value).map_err(|_| "Invalid runtime number")?;
        updated.validate()?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_saved_fields_are_editable_and_match_public_defaults() {
        let defaults = Settings::default();
        defaults.validate().unwrap();
        assert_eq!(
            defaults.checkpoint_policy(),
            crate::core::checkpoint_budget::Policy::default()
        );
        assert_eq!(
            defaults.resize_policy(),
            crate::core::geometry::ResizePolicy::default()
        );
        assert_eq!(
            defaults.snapshot_options(),
            crate::adapters::sqlite::SnapshotOptions::default()
        );
        let encoded = serde_json::to_value(&defaults).unwrap();
        let keys: std::collections::BTreeSet<_> = SETTINGS.iter().map(|s| s.key).collect();
        assert_eq!(keys.len(), SETTINGS.len());
        assert_eq!(
            keys,
            encoded
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect()
        );
        for setting in SETTINGS {
            let Kind::Number { min, max } = setting.kind else {
                unreachable!()
            };
            let below = min
                .checked_sub(1)
                .map_or_else(|| "-1".into(), |n| n.to_string());
            assert!(defaults.set(setting.key, &below).is_err());
            assert!(defaults.set(setting.key, &(max + 1).to_string()).is_err());
            assert!(defaults.set(setting.key, "private-account-value").is_err());
            for value in [min, max] {
                defaults
                    .set(setting.key, &value.to_string())
                    .unwrap()
                    .checkpoint_policy()
                    .validate()
                    .unwrap();
                defaults
                    .set(setting.key, &value.to_string())
                    .unwrap()
                    .snapshot_options()
                    .validate()
                    .unwrap();
            }
        }
        let example: Value =
            serde_json::from_str(include_str!("../../../../config/control.example.json")).unwrap();
        assert_eq!(example["runtime"], encoded);
        assert_eq!(
            defaults
                .set("runtime.native_launch_rows", "45")
                .unwrap()
                .native_launch_rows,
            45
        );
        assert_eq!(defaults.native_launch_rows, 32);
        assert!(defaults.set("unlisted", "1").is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"token":"private"}"#).is_err());
    }
}
