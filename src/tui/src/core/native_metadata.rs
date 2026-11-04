//! Caller-owned, cumulative admission for native identity metadata. No history.
use super::{json_preflight, layouts};
use std::{
    io::Read,
    path::Path,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub file_bytes: u64,
    pub work_bytes: u64,
    pub entries: u64,
    pub values: u64,
    pub header_bytes: u64,
    pub timeout_ms: u64,
    pub processes: u64,
    pub snapshot_entries: u64,
    pub depth: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            file_bytes: 1048576,
            work_bytes: 16777216,
            entries: 16384,
            values: 262144,
            header_bytes: 16384,
            timeout_ms: 2000,
            processes: 128,
            snapshot_entries: 65536,
            depth: 32,
        }
    }
}
impl Policy {
    pub fn validate(self) -> Result<()> {
        if !(1024..=67108864).contains(&self.file_bytes)
            || !(1024..=1073741824).contains(&self.work_bytes)
            || !(1..=1048576).contains(&self.entries)
            || !(16..=16777216).contains(&self.values)
            || !(128..=1048576).contains(&self.header_bytes)
            || !(1..=60000).contains(&self.timeout_ms)
            || !(1..=65536).contains(&self.processes)
            || !(1..=1048576).contains(&self.snapshot_entries)
            || !(1..=256).contains(&self.depth)
        {
            return Err("Invalid native metadata policy".into());
        }
        Ok(())
    }
}
pub struct Guard<'a> {
    pub policy: Policy,
    deadline: Instant,
    work: u64,
    entries: u64,
    values: u64,
    snapshots: u64,
    processes: u64,
    failure: Option<String>,
    progress: Option<&'a dyn Fn() -> Result<()>>,
}
fn limit(key: &str) -> String {
    format!("Native metadata limit reached; review {key} in Terminals and discovery")
}
impl<'a> Guard<'a> {
    pub fn new(policy: Policy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            deadline: Instant::now() + Duration::from_millis(policy.timeout_ms),
            work: policy.work_bytes,
            entries: policy.entries,
            values: policy.values,
            snapshots: policy.snapshot_entries,
            processes: policy.processes,
            failure: None,
            progress: None,
        })
    }
    pub fn with_progress(mut self, progress: &'a dyn Fn() -> Result<()>) -> Self {
        self.progress = Some(progress);
        self
    }
    pub fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        if let Some(deadline) = deadline {
            self.deadline = self.deadline.min(deadline);
        }
        self
    }
    pub fn check(&self) -> Result<()> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if Instant::now() >= self.deadline {
            return Err(limit("native_metadata_timeout_ms"));
        }
        if let Some(progress) = self.progress {
            progress()?;
        }
        Ok(())
    }
    fn fail<T>(&mut self, key: &str) -> Result<T> {
        let error = limit(key);
        self.failure = Some(error.clone());
        Err(error)
    }
    pub fn entries(&mut self, count: usize) -> Result<()> {
        self.check()?;
        match self.entries.checked_sub(count as u64) {
            Some(left) => {
                self.entries = left;
                Ok(())
            }
            None => self.fail("native_metadata_entry_limit"),
        }
    }
    #[cfg(windows)]
    pub fn snapshot_capacity(&self) -> Result<(usize, Instant)> {
        self.check()?;
        Ok((self.snapshots as usize, self.deadline))
    }
    pub fn snapshot_entries(&mut self, count: usize) -> Result<()> {
        self.check()?;
        match self.snapshots.checked_sub(count as u64) {
            Some(left) => {
                self.snapshots = left;
                Ok(())
            }
            None => self.fail("native_metadata_process_scan_limit"),
        }
    }
    pub fn processes(&mut self, count: usize) -> Result<()> {
        self.check()?;
        match self.processes.checked_sub(count as u64) {
            Some(left) => {
                self.processes = left;
                Ok(())
            }
            None => self.fail("native_metadata_process_limit"),
        }
    }
    /// Procfs files report zero length. Bound the actual stream, refuse special
    /// files/symlinks and charge each chunk before inspecting more metadata.
    #[cfg(unix)]
    pub fn process_file(&mut self, path: &Path) -> Result<Vec<u8>> {
        use std::os::unix::fs::OpenOptionsExt;
        self.entries(1)?;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "Cannot open native process metadata")?;
        if !file
            .metadata()
            .map_err(|_| "Cannot inspect native process metadata")?
            .is_file()
        {
            return Err("Native process metadata must be a regular file".into());
        }
        let cap = self.policy.file_bytes.min(self.work);
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            self.check()?;
            let count = ((cap - bytes.len() as u64 + 1).min(buffer.len() as u64)) as usize;
            let read = file
                .read(&mut buffer[..count])
                .map_err(|_| "Cannot read native process metadata")?;
            if read == 0 {
                self.check()?;
                return Ok(bytes);
            }
            if bytes.len() as u64 + read as u64 > cap {
                return self.fail(if cap < self.policy.file_bytes {
                    "native_metadata_work_limit_bytes"
                } else {
                    "native_metadata_file_limit_bytes"
                });
            }
            self.work -= read as u64;
            bytes.extend_from_slice(&buffer[..read]);
        }
    }
    pub fn json(&mut self, bytes: &[u8]) -> Result<()> {
        self.check()?;
        json_preflight::check(bytes, || {
            self.check()?;
            match self.values.checked_sub(1) {
                Some(left) => {
                    self.values = left;
                    Ok(())
                }
                None => self.fail("native_metadata_value_limit"),
            }
        })?;
        self.check()
    }
    pub fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        self.read_with(path, layouts::read_private_limit)
    }
    pub fn read_with(
        &mut self,
        path: &Path,
        read: impl FnOnce(&Path, u64) -> Result<Option<Vec<u8>>>,
    ) -> Result<Option<Vec<u8>>> {
        self.check()?;
        let cap = self.policy.file_bytes.min(self.work);
        let bytes = match read(path, cap) {
            Ok(bytes) => bytes,
            Err(error)
                if error == "State file exceeds size limit"
                    || error == "Native profile exceeds size limit" =>
            {
                return self.fail(if cap < self.policy.file_bytes {
                    "native_metadata_work_limit_bytes"
                } else {
                    "native_metadata_file_limit_bytes"
                });
            }
            Err(error) => return Err(error),
        };
        if let Some(bytes) = &bytes {
            if bytes.len() as u64 > cap {
                return self.fail("native_metadata_work_limit_bytes");
            }
            self.work -= bytes.len() as u64;
        }
        self.check()?;
        Ok(bytes)
    }
    /// Read exactly through the first newline: buffered read-ahead could ingest
    /// conversation bodies even when the returned bytes contain only a header.
    pub fn header(&mut self, mut reader: impl Read) -> Result<Vec<u8>> {
        self.check()?;
        let cap = self
            .policy
            .header_bytes
            .min(self.policy.file_bytes)
            .min(self.work);
        let mut line = Vec::new();
        while (line.len() as u64) < cap {
            self.check()?;
            let mut byte = [0];
            if reader
                .read(&mut byte)
                .map_err(|_| "Cannot read native session header")?
                == 0
            {
                return Err("Native session header is incomplete".into());
            }
            self.work -= 1;
            line.push(byte[0]);
            if byte[0] == b'\n' {
                self.check()?;
                return Ok(line);
            }
        }
        self.fail(if cap == self.policy.header_bytes {
            "native_metadata_header_limit_bytes"
        } else if cap == self.policy.file_bytes {
            "native_metadata_file_limit_bytes"
        } else {
            "native_metadata_work_limit_bytes"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_and_snapshot_admission_is_cumulative_and_failure_is_sticky() {
        let mut guard = Guard::new(Policy {
            processes: 2,
            snapshot_entries: 2,
            ..Default::default()
        })
        .unwrap();
        guard.processes(1).unwrap();
        guard.processes(1).unwrap();
        assert!(
            guard
                .processes(1)
                .unwrap_err()
                .contains("native_metadata_process_limit")
        );
        assert!(
            guard
                .snapshot_entries(0)
                .unwrap_err()
                .contains("native_metadata_process_limit")
        );
        let mut guard = Guard::new(Policy {
            snapshot_entries: 2,
            ..Default::default()
        })
        .unwrap();
        guard.snapshot_entries(1).unwrap();
        guard.snapshot_entries(1).unwrap();
        assert!(
            guard
                .snapshot_entries(1)
                .unwrap_err()
                .contains("native_metadata_process_scan_limit")
        );
        assert!(
            guard
                .processes(0)
                .unwrap_err()
                .contains("native_metadata_process_scan_limit")
        );
    }
    #[cfg(unix)]
    #[test]
    fn process_streams_obey_cumulative_file_work_entry_limits_and_refuse_special_files() {
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let mut nonce = [0; 16];
        crate::platform::random_bytes(&mut nonce).unwrap();
        let fixture = Fixture(std::env::temp_dir().join(format!(
            "dd-process-metadata-{:x}",
            u128::from_le_bytes(nonce)
        )));
        std::fs::create_dir(&fixture.0).unwrap();
        let path = fixture.0.join("metadata");
        std::fs::write(&path, vec![b'x'; 600]).unwrap();
        let mut guard = Guard::new(Policy {
            work_bytes: 1024,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(guard.process_file(&path).unwrap().len(), 600);
        assert!(
            guard
                .process_file(&path)
                .unwrap_err()
                .contains("native_metadata_work_limit_bytes")
        );
        let mut guard = Guard::new(Policy {
            entries: 1,
            ..Default::default()
        })
        .unwrap();
        guard.process_file(&path).unwrap();
        assert!(
            guard
                .process_file(&path)
                .unwrap_err()
                .contains("native_metadata_entry_limit")
        );
        std::fs::write(&path, vec![b'x'; 1025]).unwrap();
        assert!(
            Guard::new(Policy {
                file_bytes: 1024,
                ..Default::default()
            })
            .unwrap()
            .process_file(&path)
            .unwrap_err()
            .contains("native_metadata_file_limit_bytes")
        );
        let alias = fixture.0.join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(
            Guard::new(Default::default())
                .unwrap()
                .process_file(&alias)
                .is_err()
        );
        let fifo = fixture.0.join("fifo");
        let cpath = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(
            Guard::new(Default::default())
                .unwrap()
                .process_file(&fifo)
                .unwrap_err()
                .contains("regular file")
        );
        let mut guard = Guard::new(Default::default())
            .unwrap()
            .with_deadline(Some(Instant::now()));
        assert!(
            guard
                .process_file(&path)
                .unwrap_err()
                .contains("native_metadata_timeout_ms")
        );
    }
    #[test]
    fn cumulative_reads_values_and_ignored_entries_cannot_reset_admission() {
        let mut guard = Guard::new(Policy {
            work_bytes: 1024,
            ..Policy::default()
        })
        .unwrap();
        let read = |_: &Path, cap: u64| -> Result<Option<Vec<u8>>> {
            if cap < 600 {
                Err("State file exceeds size limit".into())
            } else {
                Ok(Some(vec![b' '; 600]))
            }
        };
        guard.read_with(Path::new("first"), read).unwrap();
        assert!(
            guard
                .read_with(Path::new("second"), read)
                .unwrap_err()
                .contains("work_limit")
        );
        assert!(
            guard
                .read_with(Path::new("third"), |_, _| panic!(
                    "exhausted operation read again"
                ))
                .is_err()
        );
        let mut guard = Guard::new(Policy {
            values: 16,
            entries: 1,
            ..Policy::default()
        })
        .unwrap();
        guard.json(b"[1,2,3,4,5,6,7,8]").unwrap();
        assert!(
            guard
                .json(b"[1,2,3,4,5,6,7,8]")
                .unwrap_err()
                .contains("value_limit")
        );
        let mut guard = Guard::new(Policy {
            entries: 1,
            ..Policy::default()
        })
        .unwrap();
        guard.entries(1).unwrap();
        assert!(guard.entries(1).unwrap_err().contains("entry_limit"));
    }
    #[test]
    fn header_reads_no_body_and_obeys_file_work_header_and_outer_deadline() {
        let mut source = std::io::Cursor::new(b"{}\nPRIVATE BODY");
        let mut guard = Guard::new(Policy::default()).unwrap();
        assert_eq!(guard.header(&mut source).unwrap(), b"{}\n");
        assert_eq!(source.position(), 3);
        for policy in [
            Policy {
                header_bytes: 128,
                ..Policy::default()
            },
            Policy {
                file_bytes: 1024,
                header_bytes: 2048,
                ..Policy::default()
            },
            Policy {
                work_bytes: 1024,
                header_bytes: 2048,
                ..Policy::default()
            },
        ] {
            assert!(
                Guard::new(policy)
                    .unwrap()
                    .header(std::io::repeat(b'x'))
                    .unwrap_err()
                    .contains("limit")
            );
        }
        assert!(
            Guard::new(Policy::default())
                .unwrap()
                .header(b"incomplete".as_slice())
                .is_err()
        );
        let expired = || Err("outer discovery expired".into());
        let mut guard = Guard::new(Policy::default())
            .unwrap()
            .with_progress(&expired);
        assert_eq!(
            guard.header(b"{}\n".as_slice()).unwrap_err(),
            "outer discovery expired"
        );
        let mut guard = Guard::new(Policy::default()).unwrap();
        guard.deadline = Instant::now();
        assert!(guard.json(b"{}").unwrap_err().contains("timeout_ms"));
    }
    #[test]
    fn strict_json_rejects_duplicate_nested_fields_and_trailing_documents() {
        for raw in [
            br#"{"id":"one","id":"two"}"#.as_slice(),
            br#"{"entries":[{"pid":1,"pid":2}]}"#,
            b"{} {}",
        ] {
            assert!(Guard::new(Policy::default()).unwrap().json(raw).is_err());
        }
    }
}
