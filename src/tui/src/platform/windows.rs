use std::{
    fs::{self, File},
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf},
    ptr,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Diagnostics::ToolHelp::*, IO::OVERLAPPED, Threading::*},
};
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
fn last() -> io::Error {
    io::Error::last_os_error()
}
fn denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}
struct Local(*mut std::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn token_user(process: HANDLE) -> io::Result<(File, Vec<usize>)> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(last());
        }
        let token = File::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut size,
        );
        if size == 0 {
            return Err(last());
        }
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        ) == 0
        {
            return Err(last());
        }
        Ok((token, buffer))
    }
}
fn sid_string(sid: PSID) -> io::Result<String> {
    unsafe {
        let mut text = ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut text) == 0 {
            return Err(last());
        }
        let _guard = Local(text.cast());
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        Ok(String::from_utf16_lossy(std::slice::from_raw_parts(
            text, len,
        )))
    }
}
pub fn current_identity() -> io::Result<String> {
    let (_token, user) = token_user(unsafe { GetCurrentProcess() })?;
    sid_string(unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid })
}
fn descriptor() -> io::Result<Local> {
    let sid = current_identity()?;
    let text: Vec<_> = format!("O:{sid}D:P(A;OICI;FA;;;{sid})\0")
        .encode_utf16()
        .collect();
    unsafe {
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(last());
        }
        Ok(Local(descriptor))
    }
}
/// Reject all reparse points (including junctions), not only symbolic links.
pub fn check_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(denied(
            "State path must be absolute without parent traversal",
        ));
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        if matches!(part, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                return Err(denied("Reparse state paths are forbidden"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn open_handle(path: &Path, access: u32, disposition: u32, private: bool) -> io::Result<File> {
    check_path(path)?;
    let name = wide(path)?;
    let security = if private { Some(descriptor()?) } else { None };
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.as_ref().map_or(ptr::null_mut(), |d| d.0),
        bInheritHandle: 0,
    };
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            if private { &attributes } else { ptr::null() },
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last());
    }
    let file = unsafe { File::from_raw_handle(handle) };
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied("Reparse state handles are forbidden"));
    }
    Ok(file)
}
unsafe fn trusted_privileged_sid(sid: PSID, native_profile: bool) -> bool {
    unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid) != 0
            || (native_profile && IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0)
    }
}
unsafe fn trusted_profile_owner(owner: PSID, user: PSID, native_profile: bool) -> bool {
    unsafe {
        !owner.is_null()
            && (EqualSid(owner, user) != 0
                || (native_profile && IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0))
    }
}
fn validate_acl(file: &File, native_profile: bool) -> io::Result<()> {
    unsafe {
        let (_token, current) = token_user(GetCurrentProcess())?;
        let user = (*(current.as_ptr().cast::<TOKEN_USER>())).User.Sid;
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut sd = ptr::null_mut();
        let status = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut sd,
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _descriptor = Local(sd);
        if !trusted_profile_owner(owner, user, native_profile) || dacl.is_null() {
            return Err(denied(
                "Private state must have a current-user owner and restricted ACL",
            ));
        }
        let mut owner_access = false;
        for index in 0..(*dacl).AceCount {
            let mut ace = ptr::null_mut();
            if GetAce(dacl, index as u32, &mut ace) == 0 {
                return Err(last());
            }
            let header = &*ace.cast::<ACE_HEADER>();
            if header.AceType == ACCESS_DENIED_ACE_TYPE {
                continue;
            }
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
                return Err(denied("Unsupported private-state ACL entry"));
            }
            let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            let sid = (&allowed.SidStart as *const u32).cast_mut().cast();
            if EqualSid(sid, user) != 0 {
                owner_access = true;
            } else if !trusted_privileged_sid(sid, native_profile) {
                #[cfg(test)]
                eprintln!(
                    "unexpected ACL sid {:?} flags {}",
                    sid_string(sid),
                    header.AceFlags
                );
                return Err(denied("Private state grants access to another identity"));
            }
        }
        if !owner_access {
            return Err(denied("Private state lacks explicit owner access"));
        }
        Ok(())
    }
}
pub fn validate_private_file(file: &File) -> io::Result<()> {
    validate_file(file, false)
}
fn validate_file(file: &File, native_profile: bool) -> io::Result<()> {
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(last());
        }
        if info.nNumberOfLinks != 1 {
            return Err(denied("Private file must not have hard links"));
        }
    }
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied("Expected a regular non-reparse private file"));
    }
    validate_acl(file, native_profile)
}
pub fn validate_private_dir(path: &Path) -> io::Result<()> {
    let file = open_handle(path, FILE_READ_ATTRIBUTES, OPEN_EXISTING, false)?;
    if !file.metadata()?.is_dir() {
        return Err(denied("Expected a private directory"));
    }
    validate_acl(&file, false)
}
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    check_path(path)?;
    if path.exists() {
        return validate_private_dir(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| denied("Missing directory parent"))?;
    if !parent.exists() {
        ensure_private_dir(parent)?;
    }
    let descriptor = descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(wide(path)?.as_ptr(), &attributes) } == 0 {
        let e = last();
        if e.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(e);
        }
    }
    validate_private_dir(path)
}
pub fn open_private(path: &Path, write: bool, create: bool) -> io::Result<File> {
    if create {
        ensure_private_dir(
            path.parent()
                .ok_or_else(|| denied("Missing private parent"))?,
        )?;
    }
    let access = GENERIC_READ | if write { GENERIC_WRITE } else { 0 };
    let file = open_handle(
        path,
        access,
        if create { OPEN_ALWAYS } else { OPEN_EXISTING },
        true,
    )?;
    validate_private_file(&file)?;
    Ok(file)
}
/// Validate existing database/sidecars and precreate an owner-only rollback file.
/// The caller must select and verify journal_mode=TRUNCATE before any write,
/// and temp_store=MEMORY to avoid sensitive temporary files outside this directory.
/// Existing WAL data is never deleted; SQLite performs its normal recovery.
pub fn prepare_sqlite(path: &Path) -> io::Result<File> {
    let database = open_private(path, true, true)?;
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        if sidecar.exists() {
            drop(open_private(&sidecar, false, false)?);
        }
    }
    let mut rollback = path.as_os_str().to_os_string();
    rollback.push("-journal");
    drop(open_private(&PathBuf::from(rollback), true, true)?);
    Ok(database)
}
pub fn create_new_private(path: &Path) -> io::Result<File> {
    validate_private_dir(
        path.parent()
            .ok_or_else(|| denied("Missing private parent"))?,
    )?;
    let file = open_handle(path, GENERIC_READ | GENERIC_WRITE, CREATE_NEW, true)?;
    validate_private_file(&file)?;
    Ok(file)
}
/// Existing harness profiles may be owned by, and grant access to, elevated
/// local administrators. Explicit current-user access is still required and
/// ordinary users/groups remain forbidden by the same DACL check.
/// This exception is never used for Daedalus-owned state or credential stores.
pub fn validate_native_profile_dir(path: &Path) -> io::Result<()> {
    let file = open_handle(path, FILE_READ_ATTRIBUTES, OPEN_EXISTING, false)?;
    if !file.metadata()?.is_dir() {
        return Err(denied("Expected a native profile directory"));
    }
    validate_acl(&file, true)
}
pub fn open_native_profile(path: &Path, write: bool, create: bool) -> io::Result<File> {
    if write || create {
        validate_native_profile_dir(
            path.parent()
                .ok_or_else(|| denied("Missing native profile parent"))?,
        )?;
    }
    let file = open_handle(
        path,
        GENERIC_READ | if write { GENERIC_WRITE } else { 0 },
        if create { OPEN_ALWAYS } else { OPEN_EXISTING },
        true,
    )?;
    validate_file(&file, true)?;
    Ok(file)
}
pub fn create_native_profile_new(path: &Path) -> io::Result<File> {
    validate_native_profile_dir(
        path.parent()
            .ok_or_else(|| denied("Missing native profile parent"))?,
    )?;
    let file = open_handle(path, GENERIC_READ | GENERIC_WRITE, CREATE_NEW, true)?;
    validate_private_file(&file)?;
    Ok(file)
}
pub fn replace_native_profile(source: &Path, destination: &Path) -> io::Result<()> {
    validate_native_profile_dir(
        destination
            .parent()
            .ok_or_else(|| denied("Missing native profile parent"))?,
    )?;
    validate_private_file(&open_regular(source)?)?;
    if destination.exists() {
        validate_file(&open_regular(destination)?, true)?;
    }
    if unsafe {
        MoveFileExW(
            wide(source)?.as_ptr(),
            wide(destination)?.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(last());
    }
    Ok(())
}
pub fn open_regular(path: &Path) -> io::Result<File> {
    let file = open_handle(path, GENERIC_READ, OPEN_EXISTING, false)?;
    if !file.metadata()?.is_file() {
        return Err(denied("Expected a regular file"));
    }
    Ok(file)
}
pub fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(last());
        }
        Ok((
            info.dwVolumeSerialNumber as u64,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ))
    }
}
fn lock(file: &File, exclusive: bool, nonblocking: bool) -> io::Result<()> {
    unsafe {
        let mut overlap: OVERLAPPED = std::mem::zeroed();
        let flags = if exclusive {
            LOCKFILE_EXCLUSIVE_LOCK
        } else {
            0
        } | if nonblocking {
            LOCKFILE_FAIL_IMMEDIATELY
        } else {
            0
        };
        if LockFileEx(
            file.as_raw_handle(),
            flags,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlap,
        ) == 0
        {
            return Err(last());
        }
        Ok(())
    }
}
pub fn lock_exclusive(file: &File, nonblocking: bool) -> io::Result<()> {
    lock(file, true, nonblocking)
}
pub fn lock_shared(file: &File, nonblocking: bool) -> io::Result<()> {
    lock(file, false, nonblocking)
}
pub fn unlock(file: &File) -> io::Result<()> {
    unsafe {
        let mut overlap: OVERLAPPED = std::mem::zeroed();
        if UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlap) == 0 {
            return Err(last());
        }
        Ok(())
    }
}
pub fn sync_directory(path: &Path) -> io::Result<()> {
    validate_private_dir(path) /* Windows replacement below uses MOVEFILE_WRITE_THROUGH. Directory FlushFileBuffers is not supported. */
}
pub fn replace_private(source: &Path, destination: &Path) -> io::Result<()> {
    check_path(destination)?;
    validate_private_dir(
        destination
            .parent()
            .ok_or_else(|| denied("Missing private parent"))?,
    )?;
    validate_private_file(&open_regular(source)?)?;
    if destination.exists() {
        validate_private_file(&open_regular(destination)?)?;
    }
    if unsafe {
        MoveFileExW(
            wide(source)?.as_ptr(),
            wide(destination)?.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(last());
    }
    Ok(())
}
#[derive(Debug, Clone)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub parent_pid: u32,
    pub creation_time: u64,
    pub executable: PathBuf,
    pub owner: String,
}
pub fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
    process_identity_with_parent(pid, None)
}
/// Refuse removal while any process still uses this executable. Names only
/// narrow the snapshot; file identity establishes which installation is in use.
fn exited_during_scan(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
        || error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)
}
pub fn check_executable_not_running(executable: &Path) -> io::Result<()> {
    let target = file_identity(&open_regular(executable)?)?;
    let name = executable
        .file_name()
        .ok_or_else(|| denied("Missing executable name"))?;
    for (pid, parent, observed) in process_snapshot()? {
        if !observed.eq_ignore_ascii_case(&name.to_string_lossy()) {
            continue;
        }
        // A service that exits between the snapshot and the open is not a mapping.
        // Permission failures still fail closed so a live image is not replaced.
        let process = match process_identity_with_parent(pid, Some(parent)) {
            Ok(process) => process,
            Err(error) if exited_during_scan(&error) => continue,
            Err(error) => return Err(error),
        };
        let opened = match open_regular(&process.executable) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if file_identity(&opened)? == target {
            return Err(io::Error::other("Installed executable is still running"));
        }
    }
    Ok(())
}
fn process_snapshot() -> io::Result<Vec<(u32, u32, String)>> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(last());
        }
        let snapshot = File::from_raw_handle(snap);
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut result = Vec::new();
        let mut more = Process32FirstW(snapshot.as_raw_handle(), &mut entry);
        if more == 0 && last().raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
            return Err(last());
        }
        while more != 0 {
            if result.len() >= 65536 {
                return Err(io::Error::other("Process snapshot exceeds bound"));
            }
            let length = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            result.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..length]),
            ));
            more = Process32NextW(snapshot.as_raw_handle(), &mut entry);
        }
        if last().raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
            return Err(last());
        }
        Ok(result)
    }
}
fn process_parents() -> io::Result<std::collections::BTreeMap<u32, u32>> {
    Ok(process_snapshot()?
        .into_iter()
        .map(|(pid, parent, _)| (pid, parent))
        .collect())
}
pub struct TerminalProcesses {
    pub processes: Vec<ProcessIdentity>,
    pub parents: std::collections::BTreeMap<u32, u32>,
}
/// Owned shell/host process observations only: no console attachment or argument reads.
pub fn terminal_processes() -> io::Result<TerminalProcesses> {
    let snapshot = process_snapshot()?;
    let parents = snapshot
        .iter()
        .map(|(pid, parent, _)| (*pid, *parent))
        .collect();
    let owner = current_identity()?;
    let mut processes = Vec::new();
    let mut queried = 0;
    let started = std::time::Instant::now();
    for (pid, parent, name) in snapshot {
        if started.elapsed() > std::time::Duration::from_secs(2) {
            return Err(io::Error::other(
                "Terminal process discovery exceeded its time bound",
            ));
        }
        if !matches!(
            name.to_ascii_lowercase().as_str(),
            "cmd.exe"
                | "powershell.exe"
                | "pwsh.exe"
                | "bash.exe"
                | "sh.exe"
                | "zsh.exe"
                | "fish.exe"
                | "nu.exe"
                | "dash.exe"
                | "ksh.exe"
                | "wsl.exe"
                | "wslhost.exe"
                | "git-bash.exe"
                | "windowsterminal.exe"
                | "openconsole.exe"
                | "conhost.exe"
                | "mintty.exe"
                | "alacritty.exe"
                | "wezterm-gui.exe"
                | "kitty.exe"
        ) {
            continue;
        }
        queried += 1;
        if queried > 512 {
            return Err(io::Error::other("Terminal process snapshot exceeds bound"));
        }
        let Ok(process) = process_identity_with_parent(pid, Some(parent)) else {
            continue;
        };
        if process.owner == owner {
            processes.push(process);
        }
    }
    Ok(TerminalProcesses { processes, parents })
}
/// Display-only descendant evidence, never terminal ownership authorization.
/// One snapshot is queried; only the bounded subtree has tokens/images opened.
pub fn descendant_processes(root_pid: u32) -> io::Result<Vec<ProcessIdentity>> {
    let parents = process_parents()?;
    let root_parent = *parents
        .get(&root_pid)
        .ok_or_else(|| io::Error::other("Root process unavailable"))?;
    let root = process_identity_with_parent(root_pid, Some(root_parent))?;
    if root.owner != current_identity()? {
        return Err(denied("Foreign process tree"));
    }
    let mut children = std::collections::BTreeMap::<u32, Vec<u32>>::new();
    for (&pid, &parent) in &parents {
        if pid != parent {
            children.entry(parent).or_default().push(pid);
        }
    }
    let mut queue = std::collections::VecDeque::from([(root.clone(), 0usize)]);
    let mut seen = std::collections::BTreeSet::from([root_pid]);
    let mut result = Vec::new();
    while let Some((parent, depth)) = queue.pop_front() {
        for &pid in children.get(&parent.pid).into_iter().flatten() {
            if seen.len() >= 128 || depth >= 32 || !seen.insert(pid) {
                return Err(io::Error::other("Process subtree exceeds bound or cycles"));
            }
            let child = process_identity_with_parent(pid, Some(parent.pid))?;
            if child.owner != root.owner || child.creation_time < parent.creation_time {
                return Err(denied("Process ancestry changed or has a foreign owner"));
            }
            queue.push_back((child.clone(), depth + 1));
            result.push(child);
        }
    }
    let current = process_identity_with_parent(root_pid, Some(root_parent))?;
    if current.creation_time != root.creation_time
        || current.owner != root.owner
        || current.executable != root.executable
    {
        return Err(io::Error::other("Root process incarnation changed"));
    }
    Ok(result)
}
fn process_identity_with_parent(
    pid: u32,
    known_parent: Option<u32>,
) -> io::Result<ProcessIdentity> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return Err(last());
        }
        let process = File::from_raw_handle(handle);
        let (_token, user) = token_user(handle)?;
        let owner = sid_string((*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid)?;
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited = created;
        let mut kernel = created;
        let mut usertime = created;
        if GetProcessTimes(
            handle,
            &mut created,
            &mut exited,
            &mut kernel,
            &mut usertime,
        ) == 0
        {
            return Err(last());
        }
        let mut name = vec![0u16; 32768];
        let mut size = name.len() as u32;
        if QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut size) == 0 {
            return Err(last());
        }
        let executable = PathBuf::from(String::from_utf16_lossy(&name[..size as usize]));
        let parent = match known_parent {
            Some(parent) => parent,
            None => *process_parents()?
                .get(&pid)
                .ok_or_else(|| io::Error::other("Process no longer observed"))?,
        };
        if WaitForSingleObject(process.as_raw_handle(), 0) != WAIT_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Process no longer active",
            ));
        }
        let _keep_alive = process;
        Ok(ProcessIdentity {
            pid,
            parent_pid: parent,
            creation_time: ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
            executable,
            owner,
        })
    }
}
pub fn create_private_new(path: &Path) -> io::Result<File> {
    create_new_private(path)
}
/// Explicit setup-only tightening. Ordinary reads/writes never repair a broad ACL.
pub fn secure_owned_dir(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return ensure_private_dir(path);
    }
    let file = open_handle(path, FILE_READ_ATTRIBUTES | WRITE_DAC, OPEN_EXISTING, false)?;
    if !file.metadata()?.is_dir() {
        return Err(denied("Expected an owned directory"));
    }
    unsafe {
        let mut owner = ptr::null_mut();
        let mut sd = ptr::null_mut();
        let status = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _guard = Local(sd);
        if sid_string(owner)? != current_identity()? {
            return Err(denied("Directory owner does not match current user"));
        }
        let descriptor = descriptor()?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = ptr::null_mut();
        if GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(last());
        }
        if SetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.0,
        ) == 0
        {
            return Err(last());
        }
    }
    validate_acl(&file, false)
}
/// Explicit install-only repair for a user-owned executable or receipt.
/// Never relax this check for ordinary state reads.
pub fn secure_owned_file(path: &Path) -> io::Result<()> {
    let file = open_handle(path, FILE_READ_ATTRIBUTES | WRITE_DAC, OPEN_EXISTING, false)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(denied("Expected an owned file without hard links"));
    }
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(last());
        }
        if info.nNumberOfLinks != 1 {
            return Err(denied("Expected an owned file without hard links"));
        }
        let mut owner = ptr::null_mut();
        let mut sd = ptr::null_mut();
        let status = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let _guard = Local(sd);
        if owner.is_null() || sid_string(owner)? != current_identity()? {
            return Err(denied("File owner does not match current user"));
        }
        let descriptor = descriptor()?;
        if SetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.0,
        ) == 0
        {
            return Err(last());
        }
    }
    validate_private_file(&file)
}
pub fn random_bytes(bytes: &mut [u8]) -> io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    for chunk in bytes.chunks_mut(u32::MAX as usize) {
        let status = unsafe {
            BCryptGenRandom(
                ptr::null_mut(),
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(io::Error::other("System randomness unavailable"));
        }
    }
    Ok(())
}

/// A background process with no inherited console, pipes, files or locks.
/// Rust's default Windows spawn inherits additional handles even with null stdio.
pub struct BackgroundProcess(File);
impl BackgroundProcess {
    pub fn id(&self) -> u32 {
        unsafe { GetProcessId(self.0.as_raw_handle()) }
    }
    pub fn wait_timeout(&self, timeout: std::time::Duration) -> io::Result<bool> {
        let millis = u32::try_from(timeout.as_millis())
            .map_err(|_| io::Error::other("Process wait exceeds bound"))?;
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(last()),
        }
    }
    pub fn try_wait(&mut self) -> io::Result<Option<u32>> {
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                if unsafe { GetExitCodeProcess(self.0.as_raw_handle(), &mut code) } == 0 {
                    return Err(last());
                }
                Ok(Some(code))
            }
            _ => Err(last()),
        }
    }
    pub fn wait(&mut self) -> io::Result<()> {
        if unsafe { WaitForSingleObject(self.0.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            return Err(last());
        }
        Ok(())
    }
    pub fn kill(&mut self) -> io::Result<()> {
        if unsafe { TerminateProcess(self.0.as_raw_handle(), 1) } == 0 {
            return Err(last());
        }
        Ok(())
    }
}
/// Pin an owned process incarnation before requesting a cooperative shutdown.
/// The handle continues to identify this process even if its PID is recycled.
pub fn pin_owned_process(pid: u32, creation_time: u64) -> io::Result<BackgroundProcess> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return Err(last());
        }
        let process = File::from_raw_handle(handle);
        let (_token, user) = token_user(handle)?;
        if sid_string((*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid)? != current_identity()? {
            return Err(denied("Foreign process owner"));
        }
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited = created;
        let mut kernel = created;
        let mut usertime = created;
        if GetProcessTimes(
            handle,
            &mut created,
            &mut exited,
            &mut kernel,
            &mut usertime,
        ) == 0
        {
            return Err(last());
        }
        let actual = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        if actual != creation_time || WaitForSingleObject(handle, 0) != WAIT_TIMEOUT {
            return Err(denied("Process incarnation changed"));
        }
        Ok(BackgroundProcess(process))
    }
}
pub fn spawn_background(executable: &Path, args: &[&str]) -> io::Result<BackgroundProcess> {
    spawn_background_with_environment(executable, args, None)
}
pub fn spawn_background_with_environment(
    executable: &Path,
    args: &[&str],
    environment: Option<&std::collections::BTreeMap<String, String>>,
) -> io::Result<BackgroundProcess> {
    let program = wide(executable)?;
    let mut block = Vec::new();
    if let Some(environment) = environment {
        let mut entries = environment.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(key, _)| key.to_uppercase());
        for (key, value) in entries {
            if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                return Err(io::Error::other("Invalid background environment"));
            }
            block.extend(format!("{key}={value}").encode_utf16());
            block.push(0);
        }
        block.push(0);
        if block.len() == 1 {
            block.push(0);
        }
        if block.len() > 32_767 {
            return Err(io::Error::other("Background environment exceeds bound"));
        }
    }
    let mut command = Vec::new();
    for arg in std::iter::once(executable.as_os_str()).chain(args.iter().map(std::ffi::OsStr::new))
    {
        if !command.is_empty() {
            command.push(b' ' as u16);
        }
        command.push(b'"' as u16);
        let mut slashes = 0;
        for unit in arg.encode_wide() {
            if unit == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Argument contains NUL",
                ));
            }
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            command.extend(std::iter::repeat_n(
                b'\\' as u16,
                if unit == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            slashes = 0;
            command.push(unit);
        }
        command.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        command.push(b'"' as u16);
    }
    command.push(0);
    if command.len() > 32_767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Command exceeds Windows limit",
        ));
    }
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of_val(&startup) as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe {
        CreateProcessW(
            program.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
            if environment.is_some() {
                block.as_ptr().cast()
            } else {
                ptr::null()
            },
            ptr::null(),
            &startup,
            &mut process,
        )
    } == 0
    {
        return Err(last());
    }
    // Take ownership immediately; neither handle is inheritable.
    let _thread = unsafe { File::from_raw_handle(process.hThread) };
    Ok(BackgroundProcess(unsafe {
        File::from_raw_handle(process.hProcess)
    }))
}
/// Atomic no-replace move used by migration publication.
pub fn rename_new(source: &Path, destination: &Path) -> io::Result<()> {
    check_path(source)?;
    check_path(destination)?;
    if unsafe {
        MoveFileExW(
            wide(source)?.as_ptr(),
            wide(destination)?.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(last());
    }
    Ok(())
}
/// User identity namespace from the Windows Known Folder API, not environment
/// variables supplied by the invoking shell.
pub fn user_local_data_directory() -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
    };
    unsafe {
        let mut value = ptr::null_mut();
        let status = SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, ptr::null_mut(), &mut value);
        if status < 0 || value.is_null() {
            return Err(io::Error::other("Windows local data directory unavailable"));
        }
        let mut len = 0;
        while *value.add(len) != 0 {
            len += 1;
        }
        let path = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
            value, len,
        )));
        CoTaskMemFree(value.cast());
        check_path(&path)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Native child fixture for hidden-console tests"]
    fn hidden_console_child_fixture() {
        use std::io::Read;
        assert!(unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null());
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).unwrap();
        if !input.is_empty() {
            let value: serde_json::Value = serde_json::from_str(&input).unwrap();
            assert!(value["hook_event_name"].is_string());
        }
    }
    #[test]
    fn piped_background_command_has_no_console_and_keeps_output() {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "platform::windows::tests::hidden_console_child_fixture",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::null());
        crate::platform::hide_console(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
    #[test]
    fn background_process_does_not_keep_inheritable_files_open() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = fixture();
        let path = root.join("exclusive-file");
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert_ne!(
            unsafe {
                SetHandleInformation(
                    file.as_raw_handle(),
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                )
            },
            0
        );
        let executable = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut child = spawn_background(
            &executable,
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 20",
            ],
        )
        .unwrap();
        drop(file);
        let reopened = fs::OpenOptions::new().write(true).share_mode(0).open(&path);
        let running = child.try_wait().unwrap().is_none();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            running,
            "Background fixture must remain alive during the handle check"
        );
        assert!(
            reopened.is_ok(),
            "Detached child inherited an unrelated exclusive file handle"
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "Child fixture for the explicit background environment test"]
    fn background_environment_child() {
        let output = std::env::var_os("DAEDALUS_FIXTURE_OUTPUT").unwrap();
        let value = std::env::var("DAEDALUS_FIXTURE").unwrap();
        assert!(std::env::var_os("PATH").is_none());
        fs::write(output, value).unwrap();
    }
    #[test]
    fn background_environment_is_explicit_and_preserves_unicode() {
        let root = fixture();
        let output = root.join("environment.txt");
        let system = std::env::var("SystemRoot").unwrap();
        let executable = std::env::current_exe().unwrap();
        let environment = std::collections::BTreeMap::from([
            ("SystemRoot".into(), system),
            (
                "DAEDALUS_FIXTURE".into(),
                "Unicode: \u{03bb}\u{03b1}".into(),
            ),
            (
                "DAEDALUS_FIXTURE_OUTPUT".into(),
                output.to_str().unwrap().into(),
            ),
        ]);
        let mut child = spawn_background_with_environment(
            &executable,
            &[
                "--ignored",
                "--exact",
                "platform::windows::tests::background_environment_child",
                "--test-threads=1",
            ],
            Some(&environment),
        )
        .unwrap();
        if !child
            .wait_timeout(std::time::Duration::from_secs(15))
            .unwrap()
        {
            child.kill().unwrap();
            panic!("Environment fixture timed out");
        }
        assert_eq!(child.try_wait().unwrap(), Some(0));
        assert_eq!(
            fs::read_to_string(output).unwrap(),
            "Unicode: \u{03bb}\u{03b1}"
        );
        fs::remove_dir_all(root).unwrap();
    }
    fn fixture() -> PathBuf {
        let mut random = [0; 16];
        random_bytes(&mut random).unwrap();
        let name = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("daedalus-windows-{name}"));
        ensure_private_dir(&path).unwrap();
        path
    }
    #[test]
    fn private_acl_atomic_replace_and_identity() {
        let dir = fixture();
        let first = dir.join("private.json");
        let temp = dir.join("next.json");
        let mut file = create_private_new(&first).unwrap();
        use std::io::Write;
        file.write_all(b"first").unwrap();
        file.sync_all().unwrap();
        validate_private_file(&file).unwrap();
        let identity = file_identity(&file).unwrap();
        assert_eq!(
            identity,
            file_identity(&open_private(&first, false, false).unwrap()).unwrap()
        );
        let mut next = create_private_new(&temp).unwrap();
        next.write_all(b"next").unwrap();
        next.sync_all().unwrap();
        drop(next);
        drop(file);
        replace_private(&temp, &first).unwrap();
        assert_ne!(
            identity,
            file_identity(&open_private(&first, false, false).unwrap()).unwrap()
        );
        assert_eq!(fs::read(&first).unwrap(), b"next");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn locking_excludes_second_handle_and_releases() {
        let dir = fixture();
        let path = dir.join("lock");
        let first = open_private(&path, true, true).unwrap();
        let second = open_private(&path, true, false).unwrap();
        lock_exclusive(&first, false).unwrap();
        assert!(lock_exclusive(&second, true).is_err());
        unlock(&first).unwrap();
        lock_exclusive(&second, true).unwrap();
        unlock(&second).unwrap();
        drop((first, second));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn hardlinks_and_parent_traversal_rejected() {
        let dir = fixture();
        let path = dir.join("secret");
        drop(create_private_new(&path).unwrap());
        fs::hard_link(&path, dir.join("other")).unwrap();
        assert!(open_private(&path, false, false).is_err());
        assert!(open_private(&dir.join("..\\secret"), false, false).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn broad_acl_is_refused_until_explicit_setup_tightens_it() {
        let dir = fixture();
        unsafe {
            let text: Vec<_> = "D:P(A;OICI;FA;;;WD)\0".encode_utf16().collect();
            let mut descriptor = ptr::null_mut();
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut()
                ),
                0
            );
            let _guard = Local(descriptor);
            assert_ne!(
                SetFileSecurityW(
                    wide(&dir).unwrap().as_ptr(),
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    descriptor
                ),
                0
            );
        }
        assert!(validate_private_dir(&dir).is_err());
        assert!(ensure_private_dir(&dir).is_err());
        assert!(validate_native_profile_dir(&dir).is_err());
        secure_owned_dir(&dir).unwrap();
        validate_private_dir(&dir).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_profile_admin_acl_does_not_relax_private_state() {
        let dir = fixture();
        let path = dir.join("settings.json");
        drop(create_private_new(&path).unwrap());
        let sid = current_identity().unwrap();
        for target in [&dir, &path] {
            unsafe {
                let text: Vec<_> = format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;BA)\0")
                    .encode_utf16()
                    .collect();
                let mut descriptor = ptr::null_mut();
                assert_ne!(
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        text.as_ptr(),
                        SDDL_REVISION_1,
                        &mut descriptor,
                        ptr::null_mut()
                    ),
                    0
                );
                let _guard = Local(descriptor);
                let target = open_handle(target, WRITE_DAC, OPEN_EXISTING, false).unwrap();
                assert_ne!(
                    SetKernelObjectSecurity(
                        target.as_raw_handle(),
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        descriptor
                    ),
                    0
                );
            }
        }
        // Wine may normalize administrator ACEs to its Unix owner mapping.
        // Verify the actual policy against the well-known SID independently.
        unsafe {
            let user_text: Vec<_> = format!("{sid}\0").encode_utf16().collect();
            let mut user = ptr::null_mut();
            assert_ne!(ConvertStringSidToSidW(user_text.as_ptr(), &mut user), 0);
            let _user_guard = Local(user);
            let text: Vec<_> = "S-1-5-32-544\0".encode_utf16().collect();
            let mut admin = ptr::null_mut();
            assert_ne!(ConvertStringSidToSidW(text.as_ptr(), &mut admin), 0);
            let _guard = Local(admin);
            assert!(!trusted_privileged_sid(admin, false));
            assert!(trusted_privileged_sid(admin, true));
            assert!(trusted_profile_owner(user, user, false));
            assert!(!trusted_profile_owner(admin, user, false));
            assert!(trusted_profile_owner(admin, user, true));
            let everyone_text: Vec<_> = "S-1-1-0\0".encode_utf16().collect();
            let mut everyone = ptr::null_mut();
            assert_ne!(
                ConvertStringSidToSidW(everyone_text.as_ptr(), &mut everyone),
                0
            );
            let _everyone_guard = Local(everyone);
            assert!(!trusted_profile_owner(everyone, user, true));
            assert!(!trusted_profile_owner(everyone, user, false));
        }
        validate_native_profile_dir(&dir).unwrap();
        drop(open_native_profile(&path, false, false).unwrap());
        let temporary = dir.join("new-settings");
        drop(create_native_profile_new(&temporary).unwrap());
        replace_native_profile(&temporary, &path).unwrap();
        // The replacement has a new owner-only ACL despite its native parent.
        drop(open_private(&path, false, false).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn bounded_descendants_include_only_live_owned_children() {
        use std::process::{Command, Stdio};
        let mut child = Command::new("ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let found = descendant_processes(std::process::id());
        child.kill().unwrap();
        child.wait().unwrap();
        let found = found.unwrap();
        let identity = found
            .iter()
            .find(|entry| entry.pid == pid)
            .expect("owned child in subtree");
        assert_eq!(identity.parent_pid, std::process::id());
        assert_eq!(identity.owner, current_identity().unwrap());
        assert_eq!(
            identity
                .executable
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            "ping.exe"
        );
        assert!(
            !descendant_processes(std::process::id())
                .unwrap()
                .iter()
                .any(|entry| entry.pid == pid)
        );
    }
    #[test]
    fn exited_process_is_not_a_live_executable_mapping() {
        assert!(exited_during_scan(&std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Process no longer active",
        )));
        assert!(exited_during_scan(&std::io::Error::from_raw_os_error(
            ERROR_INVALID_PARAMETER as i32,
        )));
        assert!(!exited_during_scan(&denied("Private state is in use")));
    }
    #[test]
    fn process_identity_matches_current_user_and_is_stable() {
        let a = process_identity(std::process::id()).unwrap();
        let b = process_identity(std::process::id()).unwrap();
        assert_eq!(a.owner, current_identity().unwrap());
        assert_eq!(a.creation_time, b.creation_time);
        assert_eq!(a.executable, b.executable);
        assert!(a.creation_time > 0);
    }
}
