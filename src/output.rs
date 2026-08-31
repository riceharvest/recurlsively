use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRecord {
    pub page_id: i64,
    pub canonical_url: String,
    pub depth: u32,
    pub final_url: Option<String>,
    pub status_code: Option<u16>,
    pub output_path: String,
    pub bytes: u64,
    pub digest: String,
}

impl ManifestRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn for_content(
        page_id: i64,
        canonical_url: impl Into<String>,
        depth: u32,
        final_url: Option<&str>,
        status_code: Option<u16>,
        output_path: impl Into<String>,
        content: &[u8],
    ) -> Self {
        Self {
            page_id,
            canonical_url: canonical_url.into(),
            depth,
            final_url: final_url.map(Into::into),
            status_code,
            output_path: output_path.into(),
            bytes: content.len() as u64,
            digest: sha256_hex(content),
        }
    }

    pub fn verify_content(&self, content: &[u8]) -> Result<(), OutputError> {
        let actual_digest = sha256_hex(content);
        if self.bytes != content.len() as u64 || self.digest != actual_digest {
            return Err(OutputError::DigestMismatch {
                expected: self.digest.clone(),
                actual: actual_digest,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub page_id: i64,
    pub canonical_url: String,
    pub depth: u32,
    pub attempts: u32,
    pub error_kind: String,
    pub error: String,
}

pub type ManifestEntry = ManifestRecord;
pub type ErrorEntry = ErrorRecord;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitResult {
    pub path: PathBuf,
    pub duplicate: bool,
}

#[derive(Debug)]
pub enum OutputError {
    Io(io::Error),
    Json(serde_json::Error),
    PathEscape(PathBuf),
    SymlinkRoot(PathBuf),
    SymlinkPath(PathBuf),
    InvalidPath(String),
    DigestMismatch { expected: String, actual: String },
    ConflictingCommit(PathBuf),
}

impl fmt::Display for OutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::PathEscape(path) => {
                write!(formatter, "output path escapes root: {}", path.display())
            }
            Self::SymlinkRoot(path) => {
                write!(formatter, "output root is a symlink: {}", path.display())
            }
            Self::SymlinkPath(path) => write!(
                formatter,
                "output path contains a symlink: {}",
                path.display()
            ),
            Self::InvalidPath(path) => write!(formatter, "invalid output path: {path}"),
            Self::DigestMismatch { expected, actual } => write!(
                formatter,
                "digest mismatch: expected {expected}, wrote {actual}"
            ),
            Self::ConflictingCommit(path) => {
                write!(formatter, "conflicting page commit: {}", path.display())
            }
        }
    }
}

impl std::error::Error for OutputError {}
impl From<io::Error> for OutputError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for OutputError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Clone)]
pub struct OutputRoot {
    root: PathBuf,
    pages: PathBuf,
    spool: PathBuf,
    tmp: PathBuf,
    manifest: PathBuf,
    errors: PathBuf,
    state: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl fmt::Debug for OutputRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputRoot")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl OutputRoot {
    pub fn setup(path: impl AsRef<Path>) -> Result<Self, OutputError> {
        let root = path.as_ref().to_path_buf();
        ensure_root_directory(&root)?;
        let pages = root.join("pages");
        let spool = root.join("spool");
        let tmp = root.join("tmp");
        ensure_child_directory(&pages)?;
        ensure_child_directory(&spool)?;
        ensure_child_directory(&tmp)?;
        let manifest = root.join("manifest.jsonl");
        let errors = root.join("errors.jsonl");
        let state = root.join("state.sqlite");
        ensure_regular_file(&manifest)?;
        ensure_regular_file(&errors)?;
        ensure_regular_file(&state)?;
        let output = Self {
            root,
            pages,
            spool,
            tmp,
            manifest,
            errors,
            state,
            lock: Arc::new(Mutex::new(())),
        };
        output.recover_jsonl()?;
        Ok(output)
    }

    pub fn new(path: impl AsRef<Path>) -> Result<Self, OutputError> {
        Self::setup(path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn pages_dir(&self) -> &Path {
        &self.pages
    }

    pub fn spool_dir(&self) -> &Path {
        &self.spool
    }

    pub fn tmp_dir(&self) -> &Path {
        &self.tmp
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest
    }

    pub fn errors_path(&self) -> &Path {
        &self.errors
    }

    pub fn state_path(&self) -> &Path {
        &self.state
    }

    pub fn page_path(&self, relative_path: &Path) -> Result<PathBuf, OutputError> {
        let components = safe_components(relative_path)?;
        let under_pages = components
            .first()
            .is_none_or(|component| component != "pages");
        let base = if under_pages { &self.pages } else { &self.root };
        let mut destination = base.clone();
        for component in components {
            destination.push(component);
        }
        Ok(destination)
    }

    pub fn append_manifest(&self, record: &ManifestRecord) -> Result<(), OutputError> {
        let _guard = self.lock()?;
        append_jsonl(&self.manifest, record)
    }

    pub fn append_error(&self, record: &ErrorRecord) -> Result<(), OutputError> {
        let _guard = self.lock()?;
        append_jsonl(&self.errors, record)
    }

    pub fn read_manifest(&self) -> Result<Vec<ManifestRecord>, OutputError> {
        let _guard = self.lock()?;
        read_jsonl(&self.manifest)
    }

    pub fn read_errors(&self) -> Result<Vec<ErrorRecord>, OutputError> {
        let _guard = self.lock()?;
        read_jsonl(&self.errors)
    }

    pub fn recover_jsonl(&self) -> Result<(), OutputError> {
        let _guard = self.lock()?;
        recover_partial_tail(&self.manifest)?;
        recover_partial_tail(&self.errors)?;
        Ok(())
    }

    pub fn recover_partial_tail(&self, path: &Path) -> Result<(), OutputError> {
        let _guard = self.lock()?;
        recover_partial_tail(path)
    }

    pub fn commit_page(
        &self,
        record: &ManifestRecord,
        content: &[u8],
    ) -> Result<CommitResult, OutputError> {
        record.verify_content(content)?;
        let _guard = self.lock()?;
        ensure_root_directory(&self.root)?;
        let destination = self.page_path(&PathBuf::from(&record.output_path))?;
        let parent = destination
            .parent()
            .ok_or_else(|| OutputError::InvalidPath(record.output_path.clone()))?;
        ensure_directory_chain(parent, &self.root)?;
        if fs::symlink_metadata(&destination).is_ok() {
            if is_symlink(&destination)? {
                return Err(OutputError::SymlinkPath(destination));
            }
            if !destination.is_file() {
                return Err(OutputError::ConflictingCommit(destination));
            }
            let existing = fs::read(&destination)?;
            if existing != content {
                return Err(OutputError::ConflictingCommit(destination));
            }
        } else {
            atomic_write(&destination, content)?;
        }

        let existing_records = read_jsonl::<ManifestRecord>(&self.manifest)?;
        for existing in existing_records {
            if existing.page_id == record.page_id {
                if existing == *record {
                    return Ok(CommitResult {
                        path: destination,
                        duplicate: true,
                    });
                }
                return Err(OutputError::ConflictingCommit(destination));
            }
        }
        append_jsonl(&self.manifest, record)?;
        Ok(CommitResult {
            path: destination,
            duplicate: false,
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, OutputError> {
        self.lock
            .lock()
            .map_err(|_| OutputError::InvalidPath("output mutex was poisoned".to_owned()))
    }
}

fn ensure_root_directory(path: &Path) -> Result<(), OutputError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(OutputError::SymlinkRoot(path.to_path_buf()));
            }
            if !metadata.is_dir() {
                return Err(OutputError::InvalidPath(format!(
                    "output root is not a directory: {}",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            let metadata = fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(OutputError::SymlinkRoot(path.to_path_buf()));
            }
        }
        Err(error) => return Err(OutputError::Io(error)),
    }
    Ok(())
}

fn ensure_child_directory(path: &Path) -> Result<(), OutputError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(OutputError::SymlinkPath(path.to_path_buf()));
            }
            if !metadata.is_dir() {
                return Err(OutputError::InvalidPath(format!(
                    "output directory is not a directory: {}",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            if is_symlink(path)? {
                return Err(OutputError::SymlinkPath(path.to_path_buf()));
            }
        }
        Err(error) => return Err(OutputError::Io(error)),
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<(), OutputError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(OutputError::SymlinkPath(path.to_path_buf()));
            }
            if !metadata.is_file() {
                return Err(OutputError::InvalidPath(format!(
                    "output record is not a file: {}",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
            file.flush()?;
        }
        Err(error) => return Err(OutputError::Io(error)),
    }
    Ok(())
}

fn ensure_directory_chain(path: &Path, root: &Path) -> Result<(), OutputError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| OutputError::PathEscape(path.to_path_buf()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(OutputError::PathEscape(path.to_path_buf()));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(OutputError::SymlinkPath(current));
                }
                if !metadata.is_dir() {
                    return Err(OutputError::InvalidPath(format!(
                        "output parent is not a directory: {}",
                        current.display()
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                if is_symlink(&current)? {
                    return Err(OutputError::SymlinkPath(current));
                }
            }
            Err(error) => return Err(OutputError::Io(error)),
        }
    }
    Ok(())
}

fn safe_components(path: &Path) -> Result<Vec<String>, OutputError> {
    if path.as_os_str().is_empty() {
        return Err(OutputError::InvalidPath(
            "path must not be empty".to_owned(),
        ));
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(OutputError::PathEscape(path.to_path_buf()));
            }
        }
    }
    if components.is_empty() {
        return Err(OutputError::InvalidPath(path.display().to_string()));
    }
    Ok(components)
}

fn is_symlink(path: &Path) -> Result<bool, OutputError> {
    Ok(fs::symlink_metadata(path)?.file_type().is_symlink())
}

fn atomic_write(destination: &Path, content: &[u8]) -> Result<(), OutputError> {
    let parent = destination
        .parent()
        .ok_or_else(|| OutputError::InvalidPath(destination.display().to_string()))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = destination
        .file_name()
        .ok_or_else(|| OutputError::InvalidPath(destination.display().to_string()))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.tmp-{}-{sequence}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temporary, destination)?;
        Ok::<(), io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(OutputError::Io)
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<(), OutputError> {
    if is_symlink(path)? {
        return Err(OutputError::SymlinkPath(path.to_path_buf()));
    }
    let mut encoded = serde_json::to_vec(value)?;
    encoded.push(b'\n');
    let mut file = OpenOptions::new().append(true).open(path)?;
    file.write_all(&encoded)?;
    file.flush()?;
    Ok(())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, OutputError> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let mut records = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        records.push(serde_json::from_slice(line)?);
    }
    Ok(records)
}

pub fn recover_partial_tail(path: &Path) -> Result<(), OutputError> {
    if is_symlink(path)? {
        return Err(OutputError::SymlinkPath(path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.last().copied() == Some(b'\n') {
        return Ok(());
    }
    let valid_length = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(valid_length as u64)?;
    Ok(())
}

pub fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}
