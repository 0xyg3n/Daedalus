//! Bounded Git inspection and reviewed local mutations. No network publication.
use crate::adapters::process;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
type Result<T> = std::result::Result<T, String>;
const LIMIT: usize = 2 * 1024 * 1024;
const STAGE_FILE_LIMIT: u64 = 64 * 1024 * 1024;
pub fn display_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if (c.is_control() && c != '\n' && c != '\t')
                || matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')
            {
                c.escape_unicode().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn git(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .args([
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    let output = process::run(&mut command, Duration::from_secs(20), LIMIT).map_err(|_| {
        "Git operation timed out or exceeded its output limit; inspect before retrying".to_string()
    })?;
    if !output.status.success() {
        return Err("Git rejected the operation; inspect repository state before retrying".into());
    }
    Ok(output.stdout)
}
fn text(cwd: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(git(cwd, args)?).map_err(|_| "Git returned non-UTF-8 paths or text".into())
}
fn path_from_git(value: &str) -> &Path {
    #[cfg(windows)]
    let value = value.strip_suffix("\r\n").unwrap_or(value);
    Path::new(value.strip_suffix('\n').unwrap_or(value))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn file_digest(path: &Path) -> Result<String> {
    use std::io::Read;
    let metadata = fs::symlink_metadata(path).map_err(|_| "A reviewed file disappeared")?;
    if metadata.file_type().is_symlink() {
        return Ok(digest(
            fs::read_link(path)
                .map_err(error)?
                .as_os_str()
                .as_encoded_bytes(),
        ));
    }
    if !metadata.is_file() || metadata.len() > STAGE_FILE_LIMIT {
        return Err("Review files up to 64 MiB individually; directory or special-file staging is unavailable".into());
    }
    let mut file = fs::File::open(path)
        .map_err(error)?
        .take(STAGE_FILE_LIMIT + 1);
    let mut hash = Sha256::new();
    let size = std::io::copy(&mut file, &mut hash).map_err(error)?;
    if size > STAGE_FILE_LIMIT {
        return Err("The file exceeded 64 MiB while it was being reviewed".into());
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn clean_path(path: &str) -> Result<()> {
    let p = Path::new(path);
    if path.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || path.contains('\0')
    {
        return Err("Git path must remain inside the selected checkout".into());
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct File {
    pub status: String,
    pub path: String,
    pub original: Option<String>,
}
pub fn parse_status(raw: &[u8]) -> Result<Vec<File>> {
    let mut result = Vec::new();
    let mut fields = raw.split(|b| *b == 0).peekable();
    while let Some(entry) = fields.next() {
        if entry.is_empty() {
            continue;
        }
        if entry.len() < 4 || entry[2] != b' ' {
            return Err("Invalid Git status entry".into());
        }
        let status = std::str::from_utf8(&entry[..2])
            .map_err(|_| "Invalid Git status")?
            .to_string();
        let path = std::str::from_utf8(&entry[3..])
            .map_err(|_| "Non-UTF-8 Git path")?
            .to_string();
        clean_path(&path)?;
        let original = if status.contains(['R', 'C']) {
            let value = std::str::from_utf8(fields.next().ok_or("Missing rename source")?)
                .map_err(|_| "Invalid rename path")?
                .to_string();
            clean_path(&value)?;
            Some(value)
        } else {
            None
        };
        result.push(File {
            status,
            path,
            original,
        });
        if result.len() > 4096 {
            return Err("Git file list exceeds 4096 entries; narrow the repository".into());
        }
    }
    Ok(result)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub branch: String,
    pub head: String,
    pub files: Vec<File>,
    pub index: String,
    pub status: String,
}
impl Repository {
    pub fn inspect(cwd: &Path) -> Result<Self> {
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err("The selected pane has no accessible local project directory".into());
        }
        let root = fs::canonicalize(path_from_git(&text(
            cwd,
            &["rev-parse", "--show-toplevel"],
        )?))
        .map_err(|_| "Cannot resolve repository root")?;
        let git_dir = fs::canonicalize(path_from_git(&text(
            &root,
            &["rev-parse", "--absolute-git-dir"],
        )?))
        .map_err(|_| "Cannot resolve checkout metadata")?;
        let head = text(&root, &["rev-parse", "--verify", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let branch = text(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "(detached HEAD)".into());
        let raw = git(
            &root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?;
        let files = parse_status(&raw)?;
        let index_path = git_dir.join("index");
        let index = if index_path.exists() {
            file_digest(&index_path)?
        } else {
            String::new()
        };
        Ok(Self {
            root,
            git_dir,
            branch,
            head,
            files,
            index,
            status: digest(&raw),
        })
    }
    pub fn summary(&self) -> String {
        let staged = self
            .files
            .iter()
            .filter(|f| f.status.as_bytes()[0] != b' ' && f.status != "??")
            .count();
        let conflicts = self
            .files
            .iter()
            .filter(|f| f.status.contains('U') || matches!(f.status.as_str(), "AA" | "DD"))
            .count();
        format!(
            "{}\nBranch: {}\nHEAD: {}\n{} changed paths · {} staged · {} conflicts",
            self.root.display(),
            self.branch,
            self.head,
            self.files.len(),
            staged,
            conflicts
        )
    }
    pub fn diff(&self, path: Option<&str>, staged: bool) -> Result<String> {
        if let Some(path) = path {
            clean_path(path)?;
        }
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        if let Some(path) = path {
            args.push(path);
        }
        text(&self.root, &args)
    }
    pub fn report(&self, kind: &str) -> Result<String> {
        let args: &[&str] = match kind {
            "branches" => &["branch", "--no-color", "-vv"],
            "worktrees" => &["worktree", "list", "--porcelain"],
            "commits" => &["log", "-30", "--date=short", "--format=%h %ad %s"],
            "summary" => return Ok(self.summary()),
            _ => return Err("Unknown project report".into()),
        };
        text(&self.root, args)
    }
    pub fn preview(&self, path: &str) -> Result<String> {
        use std::io::Read;

        clean_path(path)?;
        let candidate = self.root.join(path);
        let resolved = fs::canonicalize(&candidate).map_err(|_| "File is unavailable")?;
        if !resolved.starts_with(&self.root) {
            return Err("File points outside this checkout".into());
        }
        let metadata = fs::metadata(&resolved).map_err(error)?;
        if !metadata.is_file() || metadata.len() > LIMIT as u64 {
            return Err("Preview supports regular text files up to 2 MiB".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(resolved)
            .map_err(error)?
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() > LIMIT {
            return Err("The file exceeded 2 MiB while it was being previewed".into());
        }
        if bytes.contains(&0) {
            return Err("Binary file; use a native viewer".into());
        }
        String::from_utf8(bytes).map_err(|_| "File is not UTF-8 text".into())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Change {
    Stage { path: String },
    Unstage { path: String },
    Commit { message: String },
    Worktree { branch: String, directory: PathBuf },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub pane: Value,
    pub repository: Repository,
    pub change: Change,
    pub file: Option<String>,
    pub preview: String,
}
impl Plan {
    pub fn prepare(pane: Value, repository: Repository, change: Change) -> Result<Self> {
        let file = match &change {
            Change::Stage { path } => {
                clean_path(path)?;
                if !repository.files.iter().any(|f| f.path == *path) {
                    return Err("Path is not in the reviewed change list".into());
                }
                let path = repository.root.join(path);
                if path.exists() || path.is_symlink() {
                    Some(file_digest(&path)?)
                } else {
                    None
                }
            }
            Change::Unstage { path } => {
                clean_path(path)?;
                if repository.head.is_empty() {
                    return Err(
                        "Unstage in an unborn repository using native Git; no HEAD exists yet"
                            .into(),
                    );
                }
                None
            }
            Change::Commit { message } => {
                if message.trim().is_empty() || message.len() > 4096 || message.contains('\0') {
                    return Err("Enter a commit message up to 4096 bytes".into());
                }
                if !repository
                    .files
                    .iter()
                    .any(|f| f.status != "??" && f.status.as_bytes()[0] != b' ')
                {
                    return Err("Stage files before committing".into());
                }
                None
            }
            Change::Worktree { branch, directory } => {
                if repository.head.is_empty() {
                    return Err("Create an initial commit before making a worktree".into());
                }
                git(&repository.root, &["check-ref-format", "--branch", branch])?;
                if !directory.is_absolute()
                    || directory.exists()
                    || directory.to_string_lossy().chars().any(char::is_control)
                {
                    return Err("Choose a new absolute worktree directory".into());
                }
                crate::core::layouts::check_path(directory)?;
                if !directory.parent().is_some_and(Path::is_dir) {
                    return Err("The worktree parent directory must already exist".into());
                }
                let parent = fs::canonicalize(directory.parent().unwrap()).map_err(error)?;
                if parent.starts_with(&repository.root) || parent.starts_with(&repository.git_dir) {
                    return Err(
                        "Choose a worktree directory outside this checkout and its Git metadata"
                            .into(),
                    );
                }
                None
            }
        };
        let preview = match &change {
            Change::Commit { .. } => repository.diff(None, true)?,
            Change::Unstage { path } => repository.diff(Some(path), true)?,
            Change::Stage { path } => {
                if repository
                    .files
                    .iter()
                    .any(|f| f.path == *path && f.status == "??")
                {
                    repository.preview(path).unwrap_or_else(|_|"Binary, symlink or large file: inspect it with the native tool before staging.".into())
                } else {
                    repository.diff(Some(path), false)?
                }
            }
            Change::Worktree { .. } => String::new(),
        };
        Ok(Self {
            pane,
            repository,
            change,
            file,
            preview,
        })
    }
    pub fn fingerprint(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self).map_err(error)?))
    }
    pub fn review(&self) -> String {
        let action = match &self.change {
            Change::Stage { path } => format!("Stage exactly {:?}", path),
            Change::Unstage { path } => format!("Unstage exactly {:?}; keep working files", path),
            Change::Commit { message } => format!(
                "Commit only the reviewed index\nMessage: {message}\nNative Git hooks may run."
            ),
            Change::Worktree { branch, directory } => format!(
                "Create branch {branch} at {} in {}\nExisting uncommitted edits stay in the current checkout. Native Git checkout hooks may run.",
                self.repository.head,
                directory.display()
            ),
        };
        let paths = self
            .repository
            .files
            .iter()
            .map(|f| format!("{} {:?}", f.status, f.path))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "{}\n\n{}\n\n{}\n\n{}\n\nThe checkout, HEAD, index and reviewed path are checked again before applying. This is local only; nothing is pushed or published.",
            self.repository.summary(),
            action,
            paths,
            self.preview
        )
    }
    pub fn apply(&self, approval: &str) -> Result<String> {
        if self.fingerprint()? != approval
            || Repository::inspect(&self.repository.root)? != self.repository
        {
            return Err(
                "Repository changed after review; reopen Git review. Nothing was retried.".into(),
            );
        }
        let checked = Self::prepare(
            self.pane.clone(),
            self.repository.clone(),
            self.change.clone(),
        )?;
        if checked.file != self.file {
            return Err("File content changed after review".into());
        }
        let output = match &self.change {
            Change::Stage { path } => {
                let candidate = self.repository.root.join(path);
                let current = if candidate.exists() || candidate.is_symlink() {
                    Some(file_digest(&candidate)?)
                } else {
                    None
                };
                if current != self.file {
                    return Err("File content changed after review".into());
                }
                let output = git(&self.repository.root, &["add", "--", path])?;
                let after = if candidate.exists() || candidate.is_symlink() {
                    Some(file_digest(&candidate)?)
                } else {
                    None
                };
                if after != self.file {
                    return Err("File changed while Git was staging it. Inspect the index before retrying; the result is uncertain.".into());
                }
                output
            }
            Change::Unstage { path } => {
                git(&self.repository.root, &["restore", "--staged", "--", path])?
            }
            Change::Commit { message } => git(&self.repository.root, &["commit", "-m", message])?,
            Change::Worktree { branch, directory } => git(
                &self.repository.root,
                &[
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    directory.to_str().ok_or("Worktree path is not UTF-8")?,
                    &self.repository.head,
                ],
            )?,
        };
        Ok(format!(
            "Local Git operation completed.\n{}",
            String::from_utf8_lossy(&output)
        ))
    }
}
pub enum Request {
    Inspect(Value),
    Diff {
        pane: Value,
        path: Option<String>,
        staged: bool,
    },
    Report {
        pane: Value,
        kind: String,
    },
    Preview {
        pane: Value,
        path: String,
    },
    Prepare {
        pane: Value,
        change: Change,
    },
    Apply {
        plan: Box<Plan>,
        approval: String,
    },
}
pub enum Reply {
    Repository(Repository),
    Text(String),
    Plan(Box<Plan>),
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        directory: PathBuf,
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "daedalus-git-{}-{}",
                std::process::id(),
                super::super::orchestration::new_id()
            ));
            fs::create_dir(&directory).unwrap();
            let root = directory.join("checkout");
            fs::create_dir(&root).unwrap();
            git(&root, &["init", "--initial-branch=main"]).unwrap();
            for (k, v) in [
                ("user.name", "Fixture"),
                ("user.email", "fixture@example.invalid"),
                ("commit.gpgSign", "false"),
                ("core.hooksPath", "disabled-fixture-hooks"),
            ] {
                git(&root, &["config", "--local", k, v]).unwrap();
            }
            fs::write(root.join("file.txt"), "initial\n").unwrap();
            git(&root, &["add", "--", "file.txt"]).unwrap();
            git(&root, &["commit", "-m", "Fixture initial commit"]).unwrap();
            Self { directory, root }
        }
        fn plan(&self, change: Change) -> Plan {
            Plan::prepare(
                Value::Null,
                Repository::inspect(&self.root).unwrap(),
                change,
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    #[test]
    #[cfg(unix)]
    fn git_review_preserves_whitespace_in_checkout_paths() {
        let f = Fixture::new();
        let renamed = f.directory.join("checkout ");
        fs::rename(&f.root, &renamed).unwrap();
        assert_eq!(Repository::inspect(&renamed).unwrap().root, renamed);
    }
    #[test]
    fn local_review_stages_unstages_and_commits_only_reviewed_index() {
        let f = Fixture::new();
        fs::write(f.root.join("file.txt"), "reviewed\n").unwrap();
        fs::write(f.root.join("leave.txt"), "leave alone\n").unwrap();
        let repository = Repository::inspect(&f.root).unwrap();
        assert_eq!(repository.files.len(), 2);
        assert!(
            repository
                .diff(Some("file.txt"), false)
                .unwrap()
                .contains("+reviewed")
        );
        let stage = f.plan(Change::Stage {
            path: "file.txt".into(),
        });
        stage.apply(&stage.fingerprint().unwrap()).unwrap();
        assert!(
            Repository::inspect(&f.root)
                .unwrap()
                .diff(None, true)
                .unwrap()
                .contains("+reviewed")
        );
        let unstage = f.plan(Change::Unstage {
            path: "file.txt".into(),
        });
        unstage.apply(&unstage.fingerprint().unwrap()).unwrap();
        assert!(
            Repository::inspect(&f.root)
                .unwrap()
                .diff(None, true)
                .unwrap()
                .is_empty()
        );
        let stage = f.plan(Change::Stage {
            path: "file.txt".into(),
        });
        stage.apply(&stage.fingerprint().unwrap()).unwrap();
        let commit = f.plan(Change::Commit {
            message: "Reviewed fixture change".into(),
        });
        commit.apply(&commit.fingerprint().unwrap()).unwrap();
        let repo = Repository::inspect(&f.root).unwrap();
        assert_eq!(repo.files.len(), 1);
        assert_eq!(repo.files[0].path, "leave.txt");
        assert!(
            repo.report("commits")
                .unwrap()
                .contains("Reviewed fixture change")
        );
    }
    #[test]
    fn changed_content_head_and_index_refuse_stale_approval_without_mutation() {
        let f = Fixture::new();
        fs::write(f.root.join("file.txt"), "first edit\n").unwrap();
        let p = f.plan(Change::Stage {
            path: "file.txt".into(),
        });
        fs::write(f.root.join("file.txt"), "concurrent edit\n").unwrap();
        assert!(p.apply(&p.fingerprint().unwrap()).is_err());
        assert!(
            Repository::inspect(&f.root)
                .unwrap()
                .diff(None, true)
                .unwrap()
                .is_empty()
        );
        let stage = f.plan(Change::Stage {
            path: "file.txt".into(),
        });
        stage.apply(&stage.fingerprint().unwrap()).unwrap();
        let commit = f.plan(Change::Commit {
            message: "Must not commit".into(),
        });
        git(&f.root, &["restore", "--staged", "--", "file.txt"]).unwrap();
        assert!(commit.apply(&commit.fingerprint().unwrap()).is_err());
        assert!(
            !Repository::inspect(&f.root)
                .unwrap()
                .report("commits")
                .unwrap()
                .contains("Must not commit")
        );
    }
    #[test]
    fn new_worktree_is_local_isolated_and_preserves_existing_edits() {
        let f = Fixture::new();
        fs::write(f.root.join("file.txt"), "keep local edits\n").unwrap();
        let target = f.directory.join("isolated branch");
        let p = f.plan(Change::Worktree {
            branch: "fixture/task".into(),
            directory: target.clone(),
        });
        p.apply(&p.fingerprint().unwrap()).unwrap();
        assert_eq!(
            fs::read_to_string(f.root.join("file.txt")).unwrap(),
            "keep local edits\n"
        );
        assert_eq!(
            fs::read_to_string(target.join("file.txt")).unwrap(),
            "initial\n"
        );
        assert_eq!(Repository::inspect(&target).unwrap().branch, "fixture/task");
        assert!(
            Repository::inspect(&f.root)
                .unwrap()
                .report("worktrees")
                .unwrap()
                .contains("isolated branch")
        );
        assert!(git(&f.root, &["remote"]).unwrap().is_empty());
    }
    #[test]
    fn porcelain_paths_are_literal_and_rename_sources_are_not_lost() {
        let files =
            parse_status(b"R  renamed file\0old file\0?? -option;literal\0 M nested/name\0")
                .unwrap();
        assert_eq!(files[0].original.as_deref(), Some("old file"));
        assert_eq!(files[1].path, "-option;literal");
        for path in ["../escape", "/outside", "nested/../../escape", ""] {
            assert!(clean_path(path).is_err());
        }
        assert!(parse_status(b"R  new\0").is_err());
        assert_eq!(
            display_text("safe\x1b[31m\u{202e}"),
            "safe\\u{1b}[31m\\u{202e}"
        );
    }
    #[test]
    fn nested_untracked_files_are_individually_reviewable_and_deletions_stage() {
        let f = Fixture::new();
        fs::create_dir(f.root.join("nested")).unwrap();
        fs::write(f.root.join("nested/new.txt"), "new\n").unwrap();
        fs::remove_file(f.root.join("file.txt")).unwrap();
        let repository = Repository::inspect(&f.root).unwrap();
        assert!(repository.files.iter().any(|p| p.path == "nested/new.txt"));
        let p = f.plan(Change::Stage {
            path: "file.txt".into(),
        });
        p.apply(&p.fingerprint().unwrap()).unwrap();
        assert!(
            Repository::inspect(&f.root)
                .unwrap()
                .files
                .iter()
                .any(|f| f.path == "file.txt" && f.status == "D ")
        );
        assert!(
            Plan::prepare(
                Value::Null,
                repository,
                Change::Stage {
                    path: "nested".into()
                }
            )
            .is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn pathspec_magic_in_a_filename_never_stages_an_unreviewed_file() {
        let f = Fixture::new();
        fs::write(f.root.join(":(glob)*"), "literal filename").unwrap();
        fs::write(f.root.join("other.txt"), "unreviewed").unwrap();
        let p = f.plan(Change::Stage {
            path: ":(glob)*".into(),
        });
        p.apply(&p.fingerprint().unwrap()).unwrap();
        let repo = Repository::inspect(&f.root).unwrap();
        assert!(
            repo.files
                .iter()
                .any(|f| f.path == ":(glob)*" && f.status == "A ")
        );
        assert!(
            repo.files
                .iter()
                .any(|f| f.path == "other.txt" && f.status == "??")
        );
    }
    #[cfg(unix)]
    #[test]
    fn preview_refuses_symlink_escape_and_worktree_refuses_nested_metadata() {
        let f = Fixture::new();
        fs::write(f.directory.join("private.txt"), "outside checkout").unwrap();
        std::os::unix::fs::symlink(f.directory.join("private.txt"), f.root.join("link")).unwrap();
        let r = Repository::inspect(&f.root).unwrap();
        assert!(r.preview("link").is_err());
        assert!(
            Plan::prepare(
                Value::Null,
                r.clone(),
                Change::Worktree {
                    branch: "nested".into(),
                    directory: f.root.join("nested")
                }
            )
            .is_err()
        );
        assert!(
            Plan::prepare(
                Value::Null,
                r,
                Change::Worktree {
                    branch: "--bad".into(),
                    directory: f.directory.join("new")
                }
            )
            .is_err()
        );
    }
}
