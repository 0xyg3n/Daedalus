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
    fn process_identity_matches_current_user_and_is_stable() {
        let a = process_identity(std::process::id()).unwrap();
        let b = process_identity(std::process::id()).unwrap();
        assert_eq!(a.owner, current_identity().unwrap());
        assert_eq!(a.creation_time, b.creation_time);
        assert_eq!(a.executable, b.executable);
        assert!(a.creation_time > 0);
    }
}
