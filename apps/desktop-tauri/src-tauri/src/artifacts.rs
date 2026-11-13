//! The artifact library: content-addressed payloads plus a JSON catalog.
//!
//! Ported from the Electron host's `ArtifactLibrary`
//! (`packages/studio-host/src/artifact-library.ts`). The record shapes are
//! identical field for field because the renderer validates them
//! (`validateArtifactRecord`); the storage layout is this shell's own, under
//! `.step-studio/artifact-library/v1`, so a catalog written by one shell is never
//! read by the other by accident.
//!
//! Three properties are load-bearing, and each has a reason:
//!
//! - **The catalog is replaced atomically.** A partially-written catalog reads as
//!   an empty library, which is data loss wearing the costume of a first run.
//!   Write a temp file, then rename.
//! - **A payload appears at its final name only once it is whole.** The
//!   `.partial` suffix plus rename means a crash mid-import leaves a file the
//!   catalog does not mention, never a half-file it does.
//! - **The payload path never leaves this module.** It is a private absolute path
//!   into wherever the user keeps files; putting it in a query result or an event
//!   would disclose that to anything reading the log.
//!
//! Serialisation is one process-wide lock held across each catalog
//! read-modify-write. Two concurrent imports that each load, append and save
//! would lose one of them silently: the save that lands second overwrites the
//! first's entry while both payloads sit on disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::chrome::{ChromeError, ChromeResult};

/// The ten kinds the protocol allows. Anything else is refused rather than stored
/// under a kind the renderer will reject when it reads the row back.
pub(crate) const ARTIFACT_KINDS: [&str; 10] = [
    "graphic",
    "image",
    "video",
    "audio",
    "transcript",
    "judgment",
    "benchmark",
    "recording",
    "annotation",
    "export",
];

/// Matches the Electron host's limit. Import is a copy, so the ceiling is about
/// disk rather than memory — but it is also what stops a mis-clicked 40 GB disk
/// image from filling the volume.
const MAX_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A catalog larger than this is refused, not truncated: 50,000 records is
/// already far past what the UI pages through.
const CATALOG_MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 50_000;

/// The protocol's page ceiling. A `limit` above it is refused rather than
/// clamped, because a caller asking for 500 has a bug and answering 200 would
/// hide it.
const MAX_PAGE: u64 = 200;

/// Extension to MIME, for the import dialog. Mirrors the Electron table so a
/// `.glb` is `model/gltf-binary` in both shells.
const MIME_BY_EXTENSION: &[(&str, &str)] = &[
    (".tar", "application/x-tar"),
    (".obj", "model/obj"),
    (".ply", "application/x-ply"),
    (".wrl", "model/vrml"),
    (".x3dv", "model/x3d-vrml"),
    (".stl", "model/stl"),
    (".gltf", "model/gltf+json"),
    (".glb", "model/gltf-binary"),
    (".usda", "model/vnd.usda"),
    (".zip", "application/zip"),
    (".chart", "application/json"),
    (".mmd", "text/plain"),
    (".mermaid", "text/plain"),
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".webp", "image/webp"),
    (".gif", "image/gif"),
    (".svg", "image/svg+xml"),
    (".mp4", "video/mp4"),
    (".webm", "video/webm"),
    (".mp3", "audio/mpeg"),
    (".wav", "audio/wav"),
    (".ogg", "audio/ogg"),
    (".opus", "audio/opus"),
    (".flac", "audio/flac"),
    (".txt", "text/plain"),
    (".json", "application/json"),
    (".jsonl", "application/x-ndjson"),
    (".ompcast", "application/x-ompcast"),
    (".studiocast", "application/x-studio-terminalcast"),
];

/// Serialises every catalog read-modify-write in this process.
///
/// A field on [`Library`] would not do: commands construct a `Library` per call,
/// so each would carry its own lock and none would exclude the others.
static CATALOG_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    CATALOG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Serialize, Deserialize)]
struct StoredArtifact {
    record: Value,
    /// Which entry of `roots` holds the payload. Kept per entry so moving the
    /// active root does not orphan what is already stored elsewhere.
    root_id: String,
    /// A basename, never a path. Re-checked on load for the same reason.
    filename: String,
}

#[derive(Serialize, Deserialize)]
struct Catalog {
    version: u8,
    active_root: String,
    roots: BTreeMap<String, String>,
    artifacts: Vec<StoredArtifact>,
}

impl Catalog {
    fn empty(default_root: &Path) -> Self {
        Self {
            version: 1,
            active_root: "default".to_string(),
            roots: BTreeMap::from([(
                "default".to_string(),
                default_root.to_string_lossy().to_string(),
            )]),
            artifacts: Vec::new(),
        }
    }

    fn active_directory(&self) -> ChromeResult<&str> {
        self.roots
            .get(&self.active_root)
            .map(String::as_str)
            .ok_or_else(|| {
                ChromeError::failed("the artifact catalog points at a location that is gone")
            })
    }
}

/// One artifact library rooted at a directory.
///
/// Cheap to construct and not a handle to anything open: every operation reads
/// the catalog from disk. That is deliberate — the shell's commands are short and
/// rare, and caching would mean a second shell writing the catalog leaves this
/// one serving stale rows.
pub struct Library {
    directory: PathBuf,
}

impl Library {
    /// The library whose catalog lives at `<directory>/catalog.json`.
    pub fn open(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
        }
    }

    fn catalog_path(&self) -> PathBuf {
        self.directory.join("catalog.json")
    }

    fn default_payload_root(&self) -> PathBuf {
        self.directory.join("content")
    }

    /// Read the catalog, or the empty one when there is none yet.
    ///
    /// A missing catalog is the normal first-run state, not an error. A present
    /// but malformed one *is*: refusing is recoverable (the user can point the
    /// library somewhere else) and silently starting from empty would hide every
    /// artifact they have.
    fn load(&self) -> ChromeResult<Catalog> {
        let path = self.catalog_path();
        // `symlink_metadata`: a catalog that is a symlink is refused rather than
        // followed, the same rule the Skillshare secrets apply.
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            // Only `NotFound` is a first run. Every other error — a permission
            // denial, an AV or backup lock, a TCC refusal on macOS — means the
            // catalog is *there and we cannot read it*, and the difference is the
            // whole library: the first version of this matched `Err(_)`, so a
            // `chmod 000` catalog read as "nothing stored yet" and `save` renamed a
            // fresh empty catalog over it. Every record gone, every payload in
            // other roots orphaned, and the UI reporting `total: 1`.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // First run: make the default payload root now, so the
                // writability probe in `state` has somewhere to write — and
                // *canonicalise* it, because the state directory is derived from
                // the cwd and is relative when `STEP_STUDIO_CWD` is unset. A
                // catalog written with a relative root is then refused by the
                // absolute-root check below on the very next load, which is a
                // library that can never be read again.
                let root = self.default_payload_root();
                let _ = std::fs::create_dir_all(&root);
                let root = std::fs::canonicalize(&root).unwrap_or(root);
                return Ok(Catalog::empty(&root));
            }
            Err(error) => {
                return Err(ChromeError::failed(format!(
                    "the artifact catalog cannot be read: {error}"
                )));
            }
        };
        if !metadata.is_file() || metadata.len() > CATALOG_MAX_BYTES {
            return Err(ChromeError::failed("the artifact catalog is unreadable"));
        }
        let text = std::fs::read_to_string(&path).map_err(|error| {
            ChromeError::failed(format!("cannot read the artifact catalog: {error}"))
        })?;
        let catalog: Catalog = serde_json::from_str(&text).map_err(|error| {
            ChromeError::failed(format!("the artifact catalog is corrupt: {error}"))
        })?;
        if catalog.version != 1
            || catalog.roots.is_empty()
            || !catalog.roots.contains_key(&catalog.active_root)
            || catalog.artifacts.len() > MAX_RECORDS
        {
            return Err(ChromeError::failed(
                "the artifact catalog is not one this shell knows",
            ));
        }
        // Every root must be absolute: a relative one resolves against whatever
        // the process cwd happened to be, which is not a stable place.
        for root in catalog.roots.values() {
            if !Path::new(root).is_absolute() {
                return Err(ChromeError::failed(
                    "the artifact catalog names a relative location",
                ));
            }
        }
        for item in &catalog.artifacts {
            let basename = Path::new(&item.filename)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if item.root_id.is_empty()
                || !catalog.roots.contains_key(&item.root_id)
                || item.filename != basename
            {
                return Err(ChromeError::failed(
                    "the artifact catalog has an entry that is not safe to read",
                ));
            }
        }
        Ok(catalog)
    }

    /// Write the catalog atomically.
    fn save(&self, catalog: &Catalog) -> ChromeResult<()> {
        let text = serde_json::to_string(catalog).map_err(|error| {
            ChromeError::failed(format!("cannot serialise the artifact catalog: {error}"))
        })?;
        if text.len() as u64 > CATALOG_MAX_BYTES || catalog.artifacts.len() > MAX_RECORDS {
            return Err(ChromeError::failed(
                "the artifact library is full; export or remove artifacts",
            ));
        }
        std::fs::create_dir_all(&self.directory).map_err(|error| {
            ChromeError::failed(format!("cannot create the artifact directory: {error}"))
        })?;
        // Temp-then-rename, and fsync between them. Without the sync the rename can
        // be durable while the content is not, which on a crash or power loss is
        // exactly the "partially-written catalog" the module doc promises to
        // prevent — the rename survives, the bytes do not, and the library reads as
        // an empty one.
        let temporary = self
            .directory
            .join(format!(".catalog-{}.tmp", std::process::id()));
        write_and_sync(&temporary, text.as_bytes())?;
        std::fs::rename(&temporary, self.catalog_path()).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            ChromeError::failed(format!("cannot replace the artifact catalog: {error}"))
        })?;
        // The directory entry too: a rename is not durable until the directory that
        // holds it is. Best-effort — some filesystems refuse to fsync a directory,
        // and refusing the save over that would be worse than the risk.
        if let Ok(dir) = std::fs::File::open(&self.directory) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    /// Where artifacts live and how much is there.
    pub fn state(&self) -> ChromeResult<Value> {
        let _guard = lock();
        self.state_locked()
    }

    /// `state` without taking the lock, for callers that already hold it.
    ///
    /// The split exists because `std::sync::Mutex` is not reentrant:
    /// `set_directory` holds the lock across its own write and then wants the
    /// resulting state, and calling `state` there deadlocked against itself —
    /// the same mistake the Skillshare tests made, in a second module.
    fn state_locked(&self) -> ChromeResult<Value> {
        let catalog = self.load()?;
        let directory = catalog.active_directory()?.to_string();
        let writable = probe_writable(Path::new(&directory));
        Ok(json!({
            "locationName": Path::new(&directory)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or(directory),
            "writable": writable,
            "total": catalog.artifacts.len(),
            "totalBytes": catalog.artifacts.iter()
                .filter_map(|item| item.record.get("bytes").and_then(Value::as_u64))
                .sum::<u64>(),
        }))
    }

    /// Point the library at a new directory. Existing entries keep their root.
    pub fn set_directory(&self, directory: &str) -> ChromeResult<Value> {
        let _guard = lock();
        let path = Path::new(directory);
        if !path.is_absolute() {
            return Err(ChromeError::invalid(
                "the artifact location must be an absolute path",
            ));
        }
        std::fs::create_dir_all(path).map_err(|error| {
            ChromeError::failed(format!("cannot create the artifact location: {error}"))
        })?;
        if !probe_writable(path) {
            return Err(ChromeError::failed(
                "the chosen location is not writable; artifacts cannot be stored there",
            ));
        }
        let mut catalog = self.load()?;
        // The root id is the digest of the path, so choosing the same directory
        // twice reuses one entry rather than accumulating duplicates.
        let root_id = format!("{:x}", Sha256::digest(directory.as_bytes()));
        catalog.roots.insert(root_id.clone(), directory.to_string());
        catalog.active_root = root_id;
        self.save(&catalog)?;
        self.state_locked()
    }

    /// Copy a file in, hashing as it goes.
    ///
    /// The digest is computed from the bytes actually written, so it describes
    /// what is on disk rather than what the source claimed.
    pub fn register(&self, input: &Value, source: &Path) -> ChromeResult<Value> {
        let _guard = lock();
        let kind = input.get("kind").and_then(Value::as_str).unwrap_or("");
        if !ARTIFACT_KINDS.contains(&kind) {
            return Err(ChromeError::invalid("that is not an artifact kind"));
        }
        let name = input.get("name").and_then(Value::as_str).unwrap_or("");
        // A basename, never a path. The name is shown in the UI and is not used to
        // build one, but a name with a separator in it eventually becomes a
        // filename in someone's code.
        let basename = Path::new(name)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty()
            || name.len() > 512
            || name.contains(['\0', '\r', '\n'])
            || name != basename
        {
            return Err(ChromeError::invalid("choose an artifact filename"));
        }
        let mime_type = input.get("mimeType").and_then(Value::as_str).unwrap_or("");
        if !is_mime_type(mime_type) {
            return Err(ChromeError::invalid("that is not a MIME type"));
        }
        let workspace_id = scope(input, "workspaceId")?;
        let session_id = scope(input, "sessionId")?;

        // `symlink_metadata`, not `metadata`: an import that is a symlink is
        // refused rather than followed.
        let metadata = std::fs::symlink_metadata(source).map_err(|error| {
            ChromeError::failed(format!("cannot read the chosen file: {error}"))
        })?;
        if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(ChromeError::failed(
                "the chosen file is not a file this shell will import",
            ));
        }
        // A picture imported as an audio artifact is a mis-click, and storing it
        // would put a row in the library that no player can open.
        if matches!(kind, "image" | "audio" | "video") && !mime_type.starts_with(&format!("{kind}/"))
        {
            return Err(ChromeError::invalid(
                "the file type does not match the artifact type",
            ));
        }

        let catalog = self.load()?;
        let root_id = catalog.active_root.clone();
        let directory = catalog.active_directory()?.to_string();
        std::fs::create_dir_all(&directory).map_err(|error| {
            ChromeError::failed(format!("cannot create the artifact location: {error}"))
        })?;

        let artifact_id = uuid::Uuid::new_v4().to_string();
        let extension = Path::new(name)
            .extension()
            .map(|ext| ext.to_string_lossy().to_lowercase())
            .filter(|ext| !ext.is_empty() && ext.len() <= 12 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(|ext| format!(".{ext}"))
            .unwrap_or_else(|| ".bin".to_string());
        let filename = format!("{artifact_id}{extension}");
        let target = Path::new(&directory).join(&filename);
        // The payload reaches its final name only once it is whole.
        let partial = Path::new(&directory).join(format!("{filename}.partial"));

        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        let committed = copy_hashing(source, &partial, &mut hasher, &mut written).and_then(|()| {
            std::fs::rename(&partial, &target)
                .map_err(|error| ChromeError::failed(format!("cannot store the artifact: {error}")))
        });
        if let Err(error) = committed {
            let _ = std::fs::remove_file(&partial);
            let _ = std::fs::remove_file(&target);
            return Err(error);
        }

        let record = json!({
            "artifactId": artifact_id,
            "kind": kind,
            "name": name,
            "mimeType": mime_type,
            "bytes": written,
            "createdAt": crate::chrome::now_iso_millis(),
            "sha256": format!("{:x}", hasher.finalize()),
            "workspaceId": workspace_id,
            "sessionId": session_id,
        });

        let mut catalog = self.load()?;
        catalog.artifacts.push(StoredArtifact {
            record: record.clone(),
            root_id,
            filename,
        });
        self.save(&catalog)?;
        Ok(record)
    }

    /// The active payload root, created if it is not there yet.
    pub fn active_directory(&self) -> ChromeResult<PathBuf> {
        let _guard = lock();
        let catalog = self.load()?;
        let directory = catalog.active_directory()?.to_string();
        std::fs::create_dir_all(&directory).map_err(|error| {
            ChromeError::failed(format!("cannot create the artifact location: {error}"))
        })?;
        Ok(PathBuf::from(directory))
    }

    /// Register a payload the caller has already written into the active root.
    ///
    /// Split from [`Self::register`] because the media upload streams chunks in
    /// over IPC rather than copying a file: it owns the payload and the digest,
    /// and this owns the catalog entry. Two writers of the same record would be
    /// two chances to get the digest wrong.
    pub fn commit(
        &self,
        input: &Value,
        filename: &str,
        digest: &str,
        bytes: u64,
    ) -> ChromeResult<Value> {
        let _guard = lock();
        let kind = input.get("kind").and_then(Value::as_str).unwrap_or("");
        if !ARTIFACT_KINDS.contains(&kind) {
            return Err(ChromeError::invalid("that is not an artifact kind"));
        }
        let name = input.get("name").and_then(Value::as_str).unwrap_or("");
        if name.is_empty()
            || name.len() > 512
            || name.contains(['\0', '\r', '\n'])
            || name != Path::new(name).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        {
            return Err(ChromeError::invalid("choose an artifact filename"));
        }
        let mime_type = input.get("mimeType").and_then(Value::as_str).unwrap_or("");
        if !is_mime_type(mime_type) {
            return Err(ChromeError::invalid("that is not a MIME type"));
        }
        let workspace_id = scope(input, "workspaceId")?;
        let session_id = scope(input, "sessionId")?;
        // A basename that starts with the artifact id, exactly as `register`
        // names one — so a caller cannot commit a file the catalog did not create.
        //
        // The basename check is the part that matters, and the first version had
        // only the prefix: `load()` rejects an entry whose `filename` is not a
        // basename, so one caller passing `<uuid>../../x` stored a row that made
        // *every* subsequent read of the library fail — `list`, `state`,
        // `register`, `remove`, `resolve` — until `catalog.json` was deleted by
        // hand. The write site has to enforce the invariant the read site assumes.
        let artifact_id = filename
            .split('.')
            .next()
            .unwrap_or_default()
            .to_string();
        let basename = Path::new(filename)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if !crate::artifacts::is_artifact_id(&artifact_id)
            || filename != basename
            || !filename.starts_with(&artifact_id)
        {
            return Err(ChromeError::invalid("that is not an artifact filename"));
        }
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ChromeError::invalid("that is not an artifact digest"));
        }

        let mut catalog = self.load()?;
        let root_id = catalog.active_root.clone();
        catalog.artifacts.push(StoredArtifact {
            record: json!({
                "artifactId": artifact_id,
                "kind": kind,
                "name": name,
                "mimeType": mime_type,
                "bytes": bytes,
                "createdAt": crate::chrome::now_iso_millis(),
                "sha256": digest,
                "workspaceId": workspace_id,
                "sessionId": session_id,
            }),
            root_id,
            filename: filename.to_string(),
        });
        self.save(&catalog)?;
        Ok(catalog
            .artifacts
            .last()
            .map(|item| item.record.clone())
            .unwrap_or(Value::Null))
    }

    /// One page of artifacts, newest first.
    ///
    /// The cursor is the sort key of the last row returned, so paging is stable
    /// while artifacts are being added: a new import sorts above the cursor and
    /// never shifts the window.
    pub fn list(&self, input: &Value) -> ChromeResult<Value> {
        let _guard = lock();
        let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(50);
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(ChromeError::invalid("that is not an artifact page size"));
        }
        let workspace_id = input.get("workspaceId").and_then(Value::as_str);
        let session_id = input.get("sessionId").and_then(Value::as_str);
        let kind = input.get("kind").and_then(Value::as_str);
        let catalog = self.load()?;
        let mut rows: Vec<&Value> = catalog
            .artifacts
            .iter()
            .map(|item| &item.record)
            .filter(|record| {
                let matches = |field: &str, wanted: Option<&str>| {
                    wanted.is_none_or(|wanted| {
                        record.get(field).and_then(Value::as_str) == Some(wanted)
                    })
                };
                matches("workspaceId", workspace_id)
                    && matches("sessionId", session_id)
                    && kind.is_none_or(|kind| {
                        record.get("kind").and_then(Value::as_str) == Some(kind)
                    })
            })
            .collect();
        rows.sort_by_key(|record| std::cmp::Reverse(sort_key(record)));

        let cursor = input.get("cursor").and_then(Value::as_str);
        let start = match cursor {
            None => 0,
            Some(cursor) => rows
                .iter()
                .position(|record| sort_key(record) == cursor)
                .map(|position| position + 1)
                .ok_or_else(|| ChromeError::failed("the artifact list moved; refresh it"))?,
        };
        let page: Vec<Value> = rows
            .iter()
            .skip(start)
            .take(limit as usize)
            .map(|record| (*record).clone())
            .collect();
        let next_cursor = (start + page.len() < rows.len() && !page.is_empty())
            .then(|| sort_key(&page[page.len() - 1]));
        Ok(json!({
            "artifacts": page,
            "total": rows.len(),
            "totalBytes": rows.iter()
                .filter_map(|record| record.get("bytes").and_then(Value::as_u64))
                .sum::<u64>(),
            "nextCursor": next_cursor,
        }))
    }

    /// The record and the private payload path for one artifact.
    ///
    /// The path stays inside this module: it is an absolute path into wherever
    /// the user keeps their files.
    pub fn resolve(&self, artifact_id: &str) -> ChromeResult<(Value, PathBuf)> {
        if !is_artifact_id(artifact_id) {
            return Err(ChromeError::invalid("that is not an artifact id"));
        }
        let _guard = lock();
        let catalog = self.load()?;
        let item = catalog
            .artifacts
            .iter()
            .find(|item| {
                item.record.get("artifactId").and_then(Value::as_str) == Some(artifact_id)
            })
            .ok_or_else(|| ChromeError::failed("that artifact is not in the library"))?;
        let root = catalog
            .roots
            .get(&item.root_id)
            .cloned()
            .ok_or_else(|| ChromeError::failed("that artifact's location is gone"))?;
        let path = Path::new(&root).join(&item.filename);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            ChromeError::failed(format!("that artifact's file is missing: {error}"))
        })?;
        let bytes = item
            .record
            .get("bytes")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        if !metadata.is_file() || metadata.len() != bytes {
            return Err(ChromeError::failed(
                "that artifact's file is missing or changed",
            ));
        }
        Ok((item.record.clone(), path))
    }

    /// Remove one artifact and its payload.
    pub fn remove(&self, artifact_id: &str) -> ChromeResult<()> {
        let _guard = lock();
        if !is_artifact_id(artifact_id) {
            return Err(ChromeError::invalid("that is not an artifact id"));
        }
        let mut catalog = self.load()?;
        let Some(position) = catalog.artifacts.iter().position(|item| {
            item.record.get("artifactId").and_then(Value::as_str) == Some(artifact_id)
        }) else {
            return Ok(());
        };
        let item = catalog.artifacts.remove(position);
        if let Some(root) = catalog.roots.get(&item.root_id) {
            // One derived filename, never a directory. A payload root the user
            // chose can be a directory with other things in it.
            let _ = std::fs::remove_file(Path::new(root).join(&item.filename));
        }
        self.save(&catalog)
    }
}

/// Write bytes and fsync them, so a later rename cannot outrun the content.
fn write_and_sync(path: &Path, bytes: &[u8]) -> ChromeResult<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path).map_err(|error| {
        ChromeError::failed(format!("cannot write the artifact catalog: {error}"))
    })?;
    file.write_all(bytes).map_err(|error| {
        ChromeError::failed(format!("cannot write the artifact catalog: {error}"))
    })?;
    file.sync_all().map_err(|error| {
        ChromeError::failed(format!("cannot write the artifact catalog: {error}"))
    })
}

/// The library this shell owns, under `.step-studio/artifact-library/v1`.
pub fn library() -> Library {
    Library::open(&crate::chrome::shell_state_dir().join("artifact-library").join("v1"))
}

/// The MIME type the import dialog should claim for `name`.
pub fn mime_for(name: &str) -> &'static str {
    let extension = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    MIME_BY_EXTENSION
        .iter()
        .find(|(ext, _)| *ext == format!(".{extension}"))
        .map(|(_, mime)| *mime)
        .unwrap_or("application/octet-stream")
}

/// Copy `source` to `target`, feeding every chunk to `hasher` and counting bytes.
///
/// Hashing the copy rather than re-reading the file afterwards means the digest
/// describes the bytes in the library, which is the only thing a later read can
/// compare against.
fn copy_hashing(
    source: &Path,
    target: &Path,
    hasher: &mut Sha256,
    written: &mut u64,
) -> ChromeResult<()> {
    use std::io::{Read, Write};
    let mut reader = std::fs::File::open(source)
        .map_err(|error| ChromeError::failed(format!("cannot open the chosen file: {error}")))?;
    let mut writer = std::fs::File::create(target)
        .map_err(|error| ChromeError::failed(format!("cannot write the artifact: {error}")))?;
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|error| ChromeError::failed(format!("cannot read the chosen file: {error}")))?;
        if read == 0 {
            break;
        }
        *written += read as u64;
        if *written > MAX_ARTIFACT_BYTES {
            return Err(ChromeError::failed(
                "that file is larger than an artifact may be",
            ));
        }
        hasher.update(&chunk[..read]);
        writer
            .write_all(&chunk[..read])
            .map_err(|error| ChromeError::failed(format!("cannot write the artifact: {error}")))?;
    }
    writer
        .flush()
        .map_err(|error| ChromeError::failed(format!("cannot write the artifact: {error}")))
}

/// Can this process create and delete a file in `directory`?
///
/// A probe, not a permission bit: a volume can be mounted read-only with every
/// bit set, and the UI's "read only" label has to mean the truth.
fn probe_writable(directory: &Path) -> bool {
    let probe = directory.join(format!(".omp-write-probe-{}", std::process::id()));
    let created = std::fs::File::create(&probe).is_ok();
    if created {
        let _ = std::fs::remove_file(&probe);
    }
    created
}

/// `type/subtype`, both restricted to token characters.
fn is_mime_type(value: &str) -> bool {
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    let token =
        |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
    token(kind) && token(subtype)
}

/// A bounded, control-character-free optional scope id.
fn scope(input: &Value, field: &str) -> ChromeResult<Option<String>> {
    match input.get(field).and_then(Value::as_str) {
        None => Ok(None),
        Some(value)
            if !value.trim().is_empty()
                && value.len() <= 512
                && !value.contains('\0') =>
        {
            Ok(Some(value.to_string()))
        }
        Some(_) => Err(ChromeError::invalid("that is not a valid artifact scope")),
    }
}

/// The sort key: creation time then id, so two artifacts written in the same
/// millisecond still have a stable order.
fn sort_key(record: &Value) -> String {
    format!(
        "{}/{}",
        record.get("createdAt").and_then(Value::as_str).unwrap_or(""),
        record.get("artifactId").and_then(Value::as_str).unwrap_or("")
    )
}

/// Whether a string is a canonical UUID, which is what an `artifactId` is.
pub fn is_artifact_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let expected_dash = matches!(index, 8 | 13 | 18 | 23);
        if expected_dash != (*byte == b'-') {
            return false;
        }
        if !expected_dash && !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own, so parallel tests never share a catalog.
    fn library(tag: &str) -> (PathBuf, Library) {
        let dir = std::env::temp_dir().join(format!("artifacts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a library directory");
        let library = Library::open(&dir.join("v1"));
        (dir, library)
    }

    fn source(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("artifact-src-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("write a source file");
        path
    }

    fn import(library: &Library, name: &str, kind: &str, bytes: &[u8]) -> Value {
        let file = source(name, bytes);
        let record = library
            .register(
                &json!({ "kind": kind, "name": name, "mimeType": mime_for(name) }),
                &file,
            )
            .expect("imported");
        let _ = std::fs::remove_file(&file);
        record
    }

    #[test]
    fn a_fresh_library_is_empty_and_writable() {
        let (dir, library) = library("empty");
        let state = library.state().expect("state");
        assert_eq!(state.get("total").and_then(Value::as_u64), Some(0));
        assert_eq!(state.get("totalBytes").and_then(Value::as_u64), Some(0));
        assert_eq!(state.get("writable").and_then(Value::as_bool), Some(true));
        assert!(
            state
                .get("locationName")
                .and_then(Value::as_str)
                .is_some_and(|name| !name.is_empty()),
            "the location is named, not shown as a path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_import_lands_once_with_the_digest_of_what_was_written() {
        let (dir, library) = library("import");
        let bytes = b"not really a png";
        let record = import(&library, "shot.png", "image", bytes);
        assert_eq!(record.get("kind").and_then(Value::as_str), Some("image"));
        assert_eq!(
            record.get("bytes").and_then(Value::as_u64),
            Some(bytes.len() as u64)
        );
        assert_eq!(
            record.get("mimeType").and_then(Value::as_str),
            Some("image/png")
        );
        assert!(is_artifact_id(
            record.get("artifactId").and_then(Value::as_str).unwrap_or("")
        ));
        let digest = record.get("sha256").and_then(Value::as_str).unwrap_or("");
        assert_eq!(
            digest,
            format!("{:x}", Sha256::digest(bytes)),
            "the digest is of the stored bytes"
        );

        // And it survives a reload, which is the point of the catalog.
        let page = library.list(&json!({})).expect("list");
        assert_eq!(page.get("total").and_then(Value::as_u64), Some(1));
        assert_eq!(
            page.get("totalBytes").and_then(Value::as_u64),
            Some(bytes.len() as u64)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_imported_as_audio_is_refused_rather_than_stored() {
        // Storing it would put a row in the library no player can open.
        let (dir, library) = library("kindmismatch");
        let file = source("clip.png", b"png bytes");
        let outcome = library.register(
            &json!({ "kind": "audio", "name": "clip.png", "mimeType": "image/png" }),
            &file,
        );
        assert!(outcome.is_err(), "a mismatched kind is refused");
        let page = library.list(&json!({})).expect("list");
        assert_eq!(
            page.get("total").and_then(Value::as_u64),
            Some(0),
            "and nothing was stored"
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_with_a_path_separator_is_refused() {
        let (dir, library) = library("traversal");
        let file = source("plain.txt", b"x");
        for name in ["../../etc/passwd", "a/b.txt", ".."] {
            let outcome = library.register(
                &json!({ "kind": "export", "name": name, "mimeType": "text/plain" }),
                &file,
            );
            assert!(outcome.is_err(), "{name} must be refused");
        }
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_catalog_is_an_error_rather_than_a_first_run() {
        // The bug this pins: the first version matched `Err(_)` as "nothing stored
        // yet", so a catalog the process could not open - a permission denial, an
        // AV or backup lock, a TCC refusal on macOS - read as an empty library, and
        // the next `save` renamed a fresh empty catalog over it. Every record gone,
        // every payload in other roots orphaned, and the UI reporting `total: 1`.
        //
        // The unreadable case is produced for real: the library directory is made
        // non-traversable, so `symlink_metadata` fails with EACCES rather than
        // succeeding the way it does on a directory or a dangling symlink. A
        // directory at the catalog path is *not* a substitute - that one makes
        // `symlink_metadata` succeed and the `is_file` check refuse, which is a
        // different branch. The first version of this test used a directory and so
        // passed with the `Err(_)` bug still in place.
        let (dir, library) = library("unreadable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // `open` only stores the path; the directory appears on the first
            // write. Created here so there is something to lock.
            std::fs::create_dir_all(&library.directory).expect("make the library directory");
            let locked = std::fs::metadata(&library.directory).expect("metadata");
            let original = locked.permissions().mode();
            std::fs::set_permissions(&library.directory, std::fs::Permissions::from_mode(0o000))
                .expect("lock the library directory");
            let outcome = library.list(&json!({}));
            // Restored before any assertion, so a failure still cleans up.
            std::fs::set_permissions(&library.directory, std::fs::Permissions::from_mode(original))
                .expect("restore the library directory");
            assert!(
                outcome.is_err(),
                "a catalog that cannot be opened is an error, not an empty library: {outcome:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_catalog_is_refused_rather_than_read_as_empty() {
        // Reading it as empty would hide every artifact the user has, and look
        // like a first run.
        let (dir, library) = library("corrupt");
        std::fs::create_dir_all(library.directory.join("content")).expect("make the payload root");
        std::fs::write(library.catalog_path(), b"{ not json").expect("write a corrupt catalog");
        assert!(library.state().is_err(), "a corrupt catalog is an error");
        assert!(library.list(&json!({})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paging_walks_the_whole_library_without_repeating_a_row() {
        let (dir, library) = library("paging");
        for index in 0..5 {
            import(
                &library,
                &format!("row-{index}.txt"),
                "export",
                format!("{index}").as_bytes(),
            );
        }
        let mut seen: Vec<String> = Vec::new();
        let mut cursor = Value::Null;
        for _ in 0..10 {
            let page = library
                .list(&json!({ "limit": 2, "cursor": cursor.as_str() }))
                .expect("list");
            let rows = page
                .get("artifacts")
                .and_then(Value::as_array)
                .expect("artifacts");
            if rows.is_empty() {
                break;
            }
            seen.extend(rows.iter().filter_map(|row| {
                row.get("artifactId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }));
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(next) => cursor = json!(next),
                None => break,
            }
        }
        assert_eq!(seen.len(), 5, "every row is seen exactly once: {seen:?}");
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 5, "no row repeats");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_cursor_is_an_error_rather_than_a_restart() {
        // Silently restarting at page one would duplicate rows the renderer has
        // already appended.
        let (dir, library) = library("stale");
        import(&library, "only.txt", "export", b"x");
        let outcome = library.list(&json!({ "cursor": "1970/00000000-0000-0000-0000-000000000000" }));
        assert!(outcome.is_err(), "a cursor that names nothing is stale");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_an_artifact_takes_its_payload_with_it() {
        let (dir, library) = library("remove");
        let record = import(&library, "gone.txt", "export", b"bye");
        let artifact_id = record.get("artifactId").and_then(Value::as_str).unwrap_or("");
        let (_, path) = library.resolve(artifact_id).expect("resolve");
        assert!(path.exists(), "the payload is on disk before removal");
        library.remove(artifact_id).expect("removed");
        assert!(!path.exists(), "and gone after");
        assert!(library.resolve(artifact_id).is_err());
        let page = library.list(&json!({})).expect("list");
        assert_eq!(page.get("total").and_then(Value::as_u64), Some(0));
        // Idempotent: removing it again is not an error.
        library.remove(artifact_id).expect("removing twice is fine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_refuses_a_payload_that_changed_under_the_catalog() {
        let (dir, library) = library("tamper");
        let record = import(&library, "tampered.txt", "export", b"original");
        let artifact_id = record.get("artifactId").and_then(Value::as_str).unwrap_or("");
        let (_, path) = library.resolve(artifact_id).expect("resolve");
        std::fs::write(&path, b"something else entirely").expect("overwrite the payload");
        assert!(
            library.resolve(artifact_id).is_err(),
            "a payload whose size no longer matches the record is refused"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moving_the_library_keeps_what_was_already_stored() {
        // The whole point of a root id per entry: the old root stays in `roots`,
        // so an artifact stored before the move still resolves afterwards.
        //
        // Two moves, not one. With a single move the surviving entry is still in
        // the default root, so a `resolve` that ignored the per-entry root and
        // always used the active one would pass — which is exactly what the first
        // version of this test did, and it took a mutation to notice.
        let (dir, library) = library("move");
        let first = import(&library, "kept.txt", "export", b"kept");
        let first_id = first.get("artifactId").and_then(Value::as_str).unwrap_or("");

        let first_move = dir.join("location-a");
        std::fs::create_dir_all(&first_move).expect("make location A");
        library
            .set_directory(&first_move.to_string_lossy())
            .expect("moved to A");
        let second = import(&library, "also-kept.txt", "export", b"also kept");
        let second_id = second.get("artifactId").and_then(Value::as_str).unwrap_or("");

        let second_move = dir.join("location-b");
        std::fs::create_dir_all(&second_move).expect("make location B");
        let state = library
            .set_directory(&second_move.to_string_lossy())
            .expect("moved to B");
        assert_eq!(
            state.get("locationName").and_then(Value::as_str),
            Some("location-b"),
            "the new location is what the state reports"
        );

        // Both survive, and neither is where the active root points.
        let page = library.list(&json!({})).expect("list");
        assert_eq!(
            page.get("total").and_then(Value::as_u64),
            Some(2),
            "both rows survive"
        );
        for id in [first_id, second_id] {
            let (_, path) = library.resolve(id).expect("resolved");
            assert!(path.exists(), "{id} is still on disk");
        }
        // The second one was written while A was active, and B is active now:
        // reading it back has to go through the entry's own root, not the
        // active one. The first was written before any move, so it is in the
        // default root and stays there.
        let (_, second_path) = library.resolve(second_id).expect("resolved");
        assert!(
            second_path.starts_with(&first_move),
            "an artifact is read from where it was written, not from the active root: {}",
            second_path.display()
        );
        let (_, first_path) = library.resolve(first_id).expect("resolved");
        assert!(
            first_path.starts_with(library.default_payload_root()),
            "an artifact written before any move stays in the default root: {}",
            first_path.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relative_location_is_refused() {
        let (dir, library) = library("relative");
        assert!(library.set_directory("somewhere/relative").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_mime_table_answers_the_extensions_the_dialog_needs() {
        assert_eq!(mime_for("a.glb"), "model/gltf-binary");
        assert_eq!(mime_for("a.PNG"), "image/png");
        assert_eq!(mime_for("a.unknownext"), "application/octet-stream");
        assert!(is_mime_type("image/png"));
        assert!(is_mime_type("application/x-ndjson"));
        assert!(!is_mime_type("image"));
        assert!(!is_mime_type("image/"));
        assert!(!is_mime_type("image/png; charset=utf-8"));
    }

    #[test]
    fn a_generated_id_is_a_canonical_uuid() {
        for _ in 0..50 {
            let id = uuid::Uuid::new_v4().to_string();
            assert!(is_artifact_id(&id), "{id} is not a canonical UUID");
            assert_eq!(id.as_bytes()[14], b'4', "version 4");
            assert!(
                matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
                "variant 1"
            );
        }
    }
}
