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

/// Full argument vector. Linux reads `/proc/PID/cmdline` (bounded); macOS
/// uses `KERN_PROCARGS2`, which the kernel only returns for this user's
/// processes. Arguments must be UTF-8.
pub fn process_argv(root: &Path, pid: u32) -> Option<Vec<String>> {
    if pid == 0 {
        return None;
    }
    platform_argv(root, pid)
}

#[cfg(target_os = "macos")]
fn platform_argv(root: &Path, pid: u32) -> Option<Vec<String>> {
    if synthetic(root, pid) {
        linux_argv(root, pid)
    } else {
        macos_argv(pid)
    }
}
#[cfg(target_os = "linux")]
fn platform_argv(root: &Path, pid: u32) -> Option<Vec<String>> {
    linux_argv(root, pid)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_argv(_: &Path, _: u32) -> Option<Vec<String>> {
    None
}

/// Environment as `KEY=value` strings: Linux `/proc/PID/environ`, macOS
/// `KERN_PROCARGS2`. Only this user's processes are readable.
pub fn process_environment(root: &Path, pid: u32) -> Option<Vec<String>> {
    if pid == 0 {
        return None;
    }
    platform_environment(root, pid)
}
#[cfg(target_os = "macos")]
fn platform_environment(root: &Path, pid: u32) -> Option<Vec<String>> {
    if synthetic(root, pid) {
        linux_environment(root, pid)
    } else {
        macos_procargs(pid).map(|(_, environment)| environment)
    }
}
#[cfg(target_os = "linux")]
fn platform_environment(root: &Path, pid: u32) -> Option<Vec<String>> {
    linux_environment(root, pid)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_environment(_: &Path, _: u32) -> Option<Vec<String>> {
    None
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_environment(root: &Path, pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(root.join(pid.to_string()).join("environ")).ok()?;
    Some(
        bytes
            .split(|byte| *byte == 0)
            .filter(|value| !value.is_empty())
            .map(|value| String::from_utf8_lossy(value).into_owned())
            .collect(),
    )
}

/// Direct children of `pid`, at most `limit`. Linux reads
/// `/proc/PID/task/PID/children`; macOS uses `proc_listchildpids`. `None` when
/// the list is unreadable or larger than `limit`.
pub fn child_pids(root: &Path, pid: u32, limit: usize) -> Option<Vec<u32>> {
    if pid == 0 {
        return None;
    }
    platform_children(root, pid, limit)
}
#[cfg(target_os = "macos")]
fn platform_children(root: &Path, pid: u32, limit: usize) -> Option<Vec<u32>> {
    if root != Path::new("/proc") {
        return linux_children(root, pid, limit);
    }
    let capacity = limit.checked_add(1)?;
    let mut pids = vec![0 as libc::pid_t; capacity];
    let count = unsafe {
        libc::proc_listchildpids(
            pid as libc::pid_t,
            pids.as_mut_ptr().cast(),
            (capacity * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if count < 0 || count as usize > limit {
        return None;
    }
    pids.truncate(count as usize);
    Some(
        pids.into_iter()
            .filter(|p| *p > 0)
            .map(|p| p as u32)
            .collect(),
    )
}
#[cfg(target_os = "linux")]
fn platform_children(root: &Path, pid: u32, limit: usize) -> Option<Vec<u32>> {
    linux_children(root, pid, limit)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_children(_: &Path, _: u32, _: usize) -> Option<Vec<u32>> {
    None
}
// `/proc/PID/task/TID/children` lists the children of one thread, so every
// thread of the process is read.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_children(root: &Path, pid: u32, limit: usize) -> Option<Vec<u32>> {
    let tasks = std::fs::read_dir(root.join(pid.to_string()).join("task")).ok()?;
    let mut pids = Vec::new();
    for task in tasks {
        let text = match std::fs::read_to_string(task.ok()?.path().join("children")) {
            Ok(text) => text,
            // A thread that exited after the listing has no children of its
            // own; Linux re-parents them within the process.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue;
            }
            Err(_) => return None,
        };
        for word in text.split_whitespace() {
            let child = word.parse::<u32>().ok().filter(|p| *p > 0)?;
            if !pids.contains(&child) {
                pids.push(child);
            }
            if pids.len() > limit {
                return None;
            }
        }
    }
    Some(pids)
}

/// A file a process holds open, identified by the kernel's device and inode.
/// `path` is the kernel's last known name, display and matching evidence only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFile {
    pub device: u64,
    pub inode: u64,
    pub path: Option<PathBuf>,
}

/// Regular and other vnode-backed files a process holds open, at most `limit`.
/// `None` when the descriptor table cannot be read or exceeds `limit`, so a
/// caller never mistakes a truncated table for absence.
pub fn open_files(root: &Path, pid: u32, limit: usize) -> Option<Vec<OpenFile>> {
    if pid == 0 {
        return None;
    }
    platform_open_files(root, pid, limit)
}

#[cfg(target_os = "macos")]
fn platform_open_files(root: &Path, pid: u32, limit: usize) -> Option<Vec<OpenFile>> {
    if synthetic(root, pid) {
        linux_open_files(root, pid, limit)
    } else {
        macos_open_files(pid, limit)
    }
}
#[cfg(target_os = "linux")]
fn platform_open_files(root: &Path, pid: u32, limit: usize) -> Option<Vec<OpenFile>> {
    linux_open_files(root, pid, limit)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_open_files(_: &Path, _: u32, _: usize) -> Option<Vec<OpenFile>> {
    None
}

/// A native executable header for this host: ELF on Linux; thin or universal
/// Mach-O on macOS. Scripts and foreign images are not native harness binaries.
pub fn native_image(magic: &[u8; 4]) -> bool {
    #[cfg(target_os = "macos")]
    {
        matches!(
            magic,
            [0xcf, 0xfa, 0xed, 0xfe] | [0xce, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe]
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        magic == b"\x7fELF"
    }
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_argv(root: &Path, pid: u32) -> Option<Vec<String>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(root.join(pid.to_string()).join("cmdline"))
        .ok()?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    // An empty vector is a readable, empty command line (kernel threads).
    bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8(arg.to_vec()).ok())
        .collect()
}

// Following /proc/PID/fd/N is intentional: the kernel holds that inode.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn linux_open_files(root: &Path, pid: u32, limit: usize) -> Option<Vec<OpenFile>> {
    use std::os::unix::fs::MetadataExt;
    let entries = std::fs::read_dir(root.join(pid.to_string()).join("fd")).ok()?;
    let mut files = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= limit {
            return None;
        }
        let entry = entry.ok()?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        files.push(OpenFile {
            device: meta.dev(),
            inode: meta.ino(),
            path: std::fs::read_link(entry.path()).ok(),
        });
    }
    Some(files)
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
        // NODEV (all ones) means no controlling terminal; Linux reports 0.
        tty: if info.e_tdev == u32::MAX {
            0
        } else {
            i64::from(info.e_tdev)
        },
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

/// Current working directory of a live macOS process owned by this user.
/// Display evidence only; `None` when the kernel withholds it.
#[cfg(target_os = "macos")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|c| *c as u8)
        .take_while(|b| *b != 0)
        .collect();
    let path = String::from_utf8(bytes).ok()?;
    (path.starts_with('/') && !path.chars().any(char::is_control)).then(|| PathBuf::from(path))
}

/// Effective owner of a macOS process from the short BSD record, which the
/// kernel still returns where the full record is withheld (such as setuid
/// `login` or `sudo` in this user's sessions).
#[cfg(target_os = "macos")]
pub fn process_owner(pid: u32) -> Option<u32> {
    if pid == 0 {
        return None;
    }
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdshortinfo).cast(),
            size,
        )
    };
    (read == size).then_some(info.pbsi_uid)
}

/// Kernel short command name (`p_comm`, at most 16 bytes) for a macOS process.
#[cfg(target_os = "macos")]
pub fn process_name(pid: u32) -> Option<String> {
    let info = macos_bsd(pid)?;
    let bytes: Vec<u8> = info
        .pbi_comm
        .iter()
        .map(|c| *c as u8)
        .take_while(|b| *b != 0)
        .collect();
    let name = String::from_utf8(bytes).ok()?;
    (!name.is_empty()).then_some(name)
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
    macos_procargs(pid).map(|(argv, _)| argv)
}

/// Arguments and environment from `KERN_PROCARGS2`. The kernel returns them
/// only for the caller's own processes (or to root).
#[cfg(target_os = "macos")]
fn macos_procargs(pid: u32) -> Option<(Vec<String>, Vec<String>)> {
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
    if argv.is_empty() {
        return None;
    }
    // KERN_PROCARGS2 continues with NUL-terminated environment strings up to
    // the first empty string.
    let mut environment = Vec::new();
    while cursor < bytes.len() {
        let end = bytes[cursor..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(bytes.len(), |n| n + cursor);
        if end == cursor {
            break;
        }
        environment.push(String::from_utf8_lossy(&bytes[cursor..end]).into_owned());
        cursor = end + 1;
    }
    Some((argv, environment))
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

// <sys/proc_info.h>: libc does not export these.
#[cfg(target_os = "macos")]
const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;
#[cfg(target_os = "macos")]
#[repr(C)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: i64,
    fi_type: i32,
    fi_guardflags: u32,
}
#[cfg(target_os = "macos")]
#[repr(C)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

#[cfg(target_os = "macos")]
fn macos_open_files(pid: u32, limit: usize) -> Option<Vec<OpenFile>> {
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    let capacity = limit.checked_add(1)?;
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        capacity
    ];
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            (capacity * entry) as libc::c_int,
        )
    };
    if bytes <= 0 {
        return None;
    }
    let count = bytes as usize / entry;
    // A full buffer may be a truncated table.
    if count > limit {
        return None;
    }
    fds.truncate(count);
    let mut files = Vec::new();
    for fd in fds {
        if fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
            continue;
        }
        let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<VnodeFdInfoWithPath>() as libc::c_int;
        let read = unsafe {
            libc::proc_pidfdinfo(
                pid as libc::c_int,
                fd.proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                (&mut info as *mut VnodeFdInfoWithPath).cast(),
                size,
            )
        };
        // The descriptor may have closed since the listing.
        if read != size {
            continue;
        }
        let stat = &info.pvip.vip_vi.vi_stat;
        let name: Vec<u8> = info
            .pvip
            .vip_path
            .iter()
            .flatten()
            .map(|c| *c as u8)
            .take_while(|b| *b != 0)
            .collect();
        let path = String::from_utf8(name)
            .ok()
            .filter(|p| p.starts_with('/') && !p.chars().any(char::is_control))
            .map(PathBuf::from);
        files.push(OpenFile {
            // std reports st_dev (a signed dev_t) sign-extended to u64.
            device: stat.vst_dev as i32 as i64 as u64,
            inode: stat.vst_ino,
            path,
        });
    }
    Some(files)
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

    #[cfg(target_os = "macos")]
    #[test]
    fn current_process_cwd_and_name_are_available() {
        let pid = std::process::id();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(
            process_cwd(pid).map(|p| p.canonicalize().unwrap()),
            Some(cwd)
        );
        assert!(process_name(pid).is_some_and(|name| !name.is_empty()));
        assert!(process_cwd(0).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fd_path_info_layout_matches_the_kernel() {
        // sizeof(struct vnode_fdinfowithpath) in <sys/proc_info.h>.
        assert_eq!(std::mem::size_of::<VnodeFdInfoWithPath>(), 1200);
    }

    #[test]
    fn open_files_report_a_held_file_by_device_and_inode() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("dd-open-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("held.jsonl");
        let file = std::fs::File::create(&path).unwrap();
        let meta = file.metadata().unwrap();
        let root = Path::new("/proc");
        let files = open_files(root, std::process::id(), 4096).unwrap();
        let held = files
            .iter()
            .find(|f| f.device == meta.dev() && f.inode == meta.ino())
            .expect("held file");
        assert_eq!(
            held.path.as_ref().map(|p| p.canonicalize().unwrap()),
            Some(path.canonicalize().unwrap())
        );
        // A table larger than the limit is unknown, never a partial answer.
        assert!(open_files(root, std::process::id(), 1).is_none());
        assert!(open_files(root, 0, 4096).is_none());
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn argv_is_complete_for_the_current_process() {
        let argv = process_argv(Path::new("/proc"), std::process::id()).unwrap();
        let expected: Vec<String> = std::env::args().collect();
        assert_eq!(argv, expected);
    }

    #[test]
    fn host_executables_are_native_images_and_scripts_are_not() {
        use std::io::Read;
        let mut magic = [0; 4];
        std::fs::File::open(std::env::current_exe().unwrap())
            .unwrap()
            .read_exact(&mut magic)
            .unwrap();
        assert!(native_image(&magic));
        assert!(!native_image(b"#!/b"));
        let foreign: &[u8; 4] = if cfg!(target_os = "macos") {
            b"\x7fELF"
        } else {
            &[0xcf, 0xfa, 0xed, 0xfe]
        };
        assert!(!native_image(foreign));
    }

    #[test]
    fn environment_of_the_current_process_is_readable() {
        // A key that was never exported must not appear.
        let key = format!("DAEDALUS_ENV_PROBE_{}=", std::process::id());
        let environment = process_environment(Path::new("/proc"), std::process::id()).unwrap();
        // The kernel copy is the environment at exec time.
        assert!(
            environment
                .iter()
                .any(|v| v.starts_with("PATH=") || v.starts_with("HOME="))
        );
        assert!(!environment.iter().any(|v| v.starts_with(&key)));
        assert!(process_environment(Path::new("/proc"), 0).is_none());
    }

    #[test]
    fn procfs_children_cover_every_thread() {
        let root = std::env::temp_dir().join(format!(
            "dd-children-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        for (task, children) in [("42", "43"), ("50", "44 43")] {
            let directory = root.join("42/task").join(task);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("children"), children).unwrap();
        }
        // A thread that exited after the listing has no children file.
        std::fs::create_dir_all(root.join("42/task/51")).unwrap();
        let mut children = child_pids(&root, 42, 8).unwrap();
        children.sort();
        assert_eq!(children, [43, 44]);
        assert!(child_pids(&root, 42, 1).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_children_are_listed_and_bounded() {
        let root = Path::new("/proc");
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let listed = child_pids(root, std::process::id(), 4096).unwrap();
        let bounded = child_pids(root, std::process::id(), 0);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(listed.contains(&child.id()));
        assert!(bounded.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn short_record_reports_the_effective_owner() {
        let uid = unsafe { libc::geteuid() };
        assert_eq!(process_owner(std::process::id()), Some(uid));
        // launchd is root-owned.
        assert_eq!(process_owner(1), Some(0));
        assert!(process_owner(0).is_none());
    }

    #[test]
    fn invalid_pid_is_rejected() {
        assert!(process_info(Path::new("/proc"), 0).is_none());
        assert!(process_start(Path::new("/proc"), u32::MAX).is_none());
    }
}
