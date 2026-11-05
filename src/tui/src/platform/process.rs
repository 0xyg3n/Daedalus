//! Process identity and ancestry across Unix hosts.
//!
//! Linux reads `/proc`; macOS uses `libproc` and `sysctl` because it has no
//! procfs. Every observation is display/identity evidence only: no function
//! here authorizes terminal input or signals a process.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent: u32,
    pub start: u64,
    pub uid: u32,
    pub gid: u32,
    pub pgid: u32,
    pub foreground: i64,
    pub tty: i64,
    pub state: char,
    pub executable: Option<PathBuf>,
}

/// A stable identity for a live process. Linux uses `/proc` start ticks; macOS
/// uses the BSD start timestamp in microseconds. The value is host-local.
pub fn process_start(root: &Path, pid: u32) -> Option<u64> {
    process_info(root, pid).map(|info| info.start)
}

pub fn process_info(root: &Path, pid: u32) -> Option<ProcessInfo> {
    if pid == 0 {
        return None;
    }
    platform_info(root, pid)
}

#[cfg(target_os = "macos")]
fn platform_info(root: &Path, pid: u32) -> Option<ProcessInfo> {
    if synthetic(root, pid) {
        linux_info(root, pid)
    } else {
        macos_info(pid)
    }
}
#[cfg(target_os = "linux")]
fn platform_info(root: &Path, pid: u32) -> Option<ProcessInfo> {
    linux_info(root, pid)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_info(_: &Path, _: u32) -> Option<ProcessInfo> {
    None
}

pub fn parent_pid(root: &Path, pid: u32) -> Option<u32> {
    process_info(root, pid).map(|info| info.parent)
}

pub fn process_executable(root: &Path, pid: u32) -> Option<PathBuf> {
    process_info(root, pid).and_then(|info| info.executable)
}

/// First argv element. Linux preserves the native `/proc/PID/cmdline` value;
/// macOS uses `KERN_PROCARGS2` and falls back to the executable path.
pub fn process_command(root: &Path, pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    platform_command(root, pid)
}

#[cfg(target_os = "macos")]
fn platform_command(root: &Path, pid: u32) -> Option<String> {
    if synthetic(root, pid) {
        linux_command(root, pid)
    } else {
        macos_command(pid)
    }
}
#[cfg(target_os = "linux")]
fn platform_command(root: &Path, pid: u32) -> Option<String> {
    linux_command(root, pid)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_command(_: &Path, _: u32) -> Option<String> {
    None
}

pub fn process_list(root: &Path) -> Vec<u32> {
    platform_list(root)
}

#[cfg(target_os = "macos")]
fn platform_list(root: &Path) -> Vec<u32> {
    if root != Path::new("/proc") {
        linux_list(root)
    } else {
        macos_list()
    }
}
#[cfg(target_os = "linux")]
fn platform_list(root: &Path) -> Vec<u32> {
    linux_list(root)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_list(_: &Path) -> Vec<u32> {
    Vec::new()
}

/// Host boot identity. Linux uses the kernel boot UUID; macOS uses
/// `kern.bootsessionuuid`, falling back to `kern.boottime`.
pub fn boot_id(root: &Path) -> Option<String> {
    platform_boot_id(root)
}

#[cfg(target_os = "macos")]
fn platform_boot_id(root: &Path) -> Option<String> {
    if root != Path::new("/proc") {
        linux_boot_id(root)
    } else {
        macos_boot_id()
    }
}
#[cfg(target_os = "linux")]
fn platform_boot_id(root: &Path) -> Option<String> {
    linux_boot_id(root)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_boot_id(_: &Path) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn synthetic(root: &Path, pid: u32) -> bool {
    root != Path::new("/proc") && root.join(pid.to_string()).join("stat").is_file()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_info(root: &Path, pid: u32) -> Option<ProcessInfo> {
    use std::os::unix::fs::MetadataExt;
    let directory = root.join(pid.to_string());
    let text = std::fs::read_to_string(directory.join("stat")).ok()?;
    if text.split_whitespace().next() != Some(pid.to_string().as_str()) {
        return None;
    }
    let (_, fields) = text.rsplit_once(')')?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let parent = fields.get(1)?.parse().ok()?;
    let pgid = fields.get(2)?.parse().ok()?;
    let tty = fields.get(4)?.parse().ok()?;
    let foreground = fields.get(5)?.parse().ok()?;
    let start = fields.get(19)?.parse().ok()?;
    if start == 0 {
        return None;
    }
    let metadata = std::fs::metadata(&directory).ok()?;
    Some(ProcessInfo {
        pid,
        parent,
        start,
        uid: metadata.uid(),
        gid: metadata.gid(),
        pgid,
        foreground,
        tty,
        state,
        executable: std::fs::read_link(directory.join("exe")).ok(),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_command(root: &Path, pid: u32) -> Option<String> {
    let bytes = std::fs::read(root.join(pid.to_string()).join("cmdline")).ok()?;
    let first = bytes.split(|byte| *byte == 0).next()?;
    if first.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(first).into_owned())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_list(root: &Path) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return pids;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pid > 0 {
            pids.push(pid);
        }
    }
    pids
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_boot_id(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("sys/kernel/random/boot_id")).ok()?;
    let text = text.trim();
    (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

#[cfg(target_os = "macos")]
fn macos_info(pid: u32) -> Option<ProcessInfo> {
    let info = macos_bsd(pid)?;
    let start = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)?;
    if start == 0 {
        return None;
    }
    Some(ProcessInfo {
        pid,
        parent: info.pbi_ppid,
        start,
        uid: info.pbi_uid,
        gid: info.pbi_gid,
        pgid: info.pbi_pgid,
        foreground: i64::from(info.e_tpgid),
        tty: i64::from(info.e_tdev),
        state: match info.pbi_status {
            1 | 3 => 'S',
            2 => 'R',
            4 => 'T',
            5 => 'Z',
            _ => 'S',
        },
        executable: macos_executable(pid),
    })
}

#[cfg(target_os = "macos")]
fn macos_bsd(pid: u32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (read == size).then_some(info)
}

#[cfg(target_os = "macos")]
fn macos_executable(pid: u32) -> Option<PathBuf> {
    let mut bytes = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let read = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            bytes.as_mut_ptr().cast(),
            bytes.len() as u32,
        )
    };
    if read <= 0 {
        return None;
    }
    bytes.truncate(read as usize);
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let path = String::from_utf8(bytes[..end].to_vec()).ok()?;
    (!path.is_empty() && !path.chars().any(char::is_control)).then(|| PathBuf::from(path))
}

#[cfg(target_os = "macos")]
fn macos_command(pid: u32) -> Option<String> {
    macos_argv(pid)
        .and_then(|argv| argv.into_iter().next())
        .or_else(|| macos_executable(pid).map(|path| path.to_string_lossy().into_owned()))
}

#[cfg(target_os = "macos")]
fn macos_argv(pid: u32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size = 0usize;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size <= 4
        || size > 1024 * 1024
    {
        return None;
    }
    let mut bytes = vec![0u8; size];
    let mut actual = size;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            bytes.as_mut_ptr().cast(),
            &mut actual,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    bytes.truncate(actual);
    let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    if !(1..=4096).contains(&argc) {
        return None;
    }
    let mut cursor = 4;
    let end = bytes[cursor..].iter().position(|byte| *byte == 0)? + cursor;
    cursor = end + 1;
    while cursor < bytes.len() && bytes[cursor] == 0 {
        cursor += 1;
    }
    let mut argv = Vec::new();
    for _ in 0..argc {
        if cursor >= bytes.len() {
            break;
        }
        let end = bytes[cursor..].iter().position(|byte| *byte == 0)? + cursor;
        if end > cursor {
            argv.push(String::from_utf8_lossy(&bytes[cursor..end]).into_owned());
        }
        cursor = end + 1;
    }
    (!argv.is_empty()).then_some(argv)
}

#[cfg(target_os = "macos")]
fn macos_list() -> Vec<u32> {
    let mut capacity = 4096usize;
    for _ in 0..4 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Vec::new();
        }
        if (count as usize) < capacity {
            pids.truncate(count as usize);
            return pids
                .into_iter()
                .filter(|pid| *pid > 0)
                .map(|pid| pid as u32)
                .collect();
        }
        capacity *= 2;
    }
    Vec::new()
}

#[cfg(target_os = "macos")]
fn macos_boot_id() -> Option<String> {
    sysctl_string("kern.bootsessionuuid").or_else(|| {
        let mut value: libc::timeval = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of::<libc::timeval>();
        let name = std::ffi::CString::new("kern.boottime").ok()?;
        let status = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (status == 0 && value.tv_sec > 0).then(|| format!("{}.{}", value.tv_sec, value.tv_usec))
    })
}

#[cfg(target_os = "macos")]
fn sysctl_string(name: &str) -> Option<String> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut size = 0usize;
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size == 0
        || size > 4096
    {
        return None;
    }
    let mut bytes = vec![0u8; size];
    let mut actual = size;
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut actual,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    bytes.truncate(actual);
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let value = String::from_utf8(bytes[..end].to_vec()).ok()?;
    (!value.is_empty() && !value.chars().any(char::is_control)).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_identity_is_available() {
        let root = Path::new("/proc");
        let pid = std::process::id();
        let info = process_info(root, pid).expect("current process identity");
        assert_eq!(info.pid, pid);
        assert!(info.start > 0);
        assert!(info.parent > 0);
        assert!(process_start(root, pid).is_some());
        assert!(parent_pid(root, pid).is_some());
        assert!(process_list(root).contains(&pid));
        assert!(!boot_id(root).unwrap_or_default().is_empty());
        assert_eq!(
            process_executable(root, pid)
                .and_then(|path| path.file_name().map(|name| !name.is_empty())),
            Some(true)
        );
    }

    #[test]
    fn invalid_pid_is_rejected() {
        assert!(process_info(Path::new("/proc"), 0).is_none());
        assert!(process_start(Path::new("/proc"), u32::MAX).is_none());
    }
}
