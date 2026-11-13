//! Encrypted service definitions: the saved configurations the ServicesPane lists.
//!
//! Ported from the Electron host's `ServiceDefinitionStore`
//! (`apps/desktop/src/service-definitions.ts`), including its `SecretCodec` seam —
//! the store never decides *how* bytes are protected, only that they are. That
//! seam is what makes this module testable without an OS keystore, and it is why
//! the two implementations can be swapped without touching a line of the logic.
//!
//! Why encryption and not merely a private file: a `StudioServiceSpec` carries an
//! `env` block, and that block is where a service's API token lives. A 0600 file
//! stops another unprivileged user on the same machine; it does nothing about a
//! backup, a synced home directory, or a disk image. The Electron shell uses
//! `safeStorage` for exactly this reason.
//!
//! The construction is the standard one: a random 256-bit data-encryption key
//! held in the OS keystore, AES-256-GCM over the file with a fresh nonce per
//! write. Two rules follow from it and both are enforced below:
//!
//! - **A keystore that cannot be reached is never worked around.** Generating a
//!   fresh key instead would make every existing definition undecryptable *and*
//!   look like success. The caller is told the store is locked, and the file is
//!   left exactly as it is so granting access and refreshing recovers it.
//! - **The nonce is stored with the ciphertext.** GCM's security depends on a
//!   nonce never repeating under one key, and a counter kept across process
//!   restarts is a counter that eventually repeats.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::chrome::{ChromeError, ChromeResult};

/// The protocol's ceiling on saved configurations, not this shell's.
const MAX_ROWS: usize = 200;

/// A store larger than this is refused rather than decrypted.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Where the data-encryption key lives in the OS keystore.
const KEYRING_SERVICE: &str = "step-studio";
const KEYRING_USER: &str = "service-definitions-dek";

/// Serialises every read-modify-write in this process.
static STORE_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    STORE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Why a codec could not do what was asked.
///
/// Two variants, not three: "there is no keystore here" is answered by
/// [`SecretCodec::available`] before any codec call, so it never reaches this
/// enum. Keeping it here as well would leave two places deciding the same thing
/// and a message that could say either.
#[derive(Debug)]
pub enum CodecError {
    /// The keystore refused — Keychain access denied, Secret Service not
    /// running, an ad-hoc re-sign changed the app's identity. Recoverable: grant
    /// access and refresh, and nothing has been lost.
    Locked,
    /// The bytes were protected but will not open: wrong key, truncated, or
    /// tampered with.
    Corrupt,
}

/// The protection seam, mirroring the Electron `SecretCodec` interface.
pub trait SecretCodec: Send + Sync {
    /// Whether a keystore exists here at all.
    fn available(&self) -> bool;
    /// `nonce || ciphertext`, with the nonce freshly generated.
    fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>, CodecError>;
    fn decrypt(&self, protected: &[u8]) -> Result<String, CodecError>;
}

/// The production codec: an OS-keystore key plus AES-256-GCM.
pub struct OsKeystoreCodec;

impl OsKeystoreCodec {
    /// The 32-byte data-encryption key, creating it on first use.
    ///
    /// A failure to reach the keystore is [`CodecError::Locked`], never a new
    /// key. See the module comment: a fresh key here would silently orphan every
    /// definition already on disk.
    fn data_key() -> Result<[u8; 32], CodecError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|_| CodecError::Locked)?;
        match entry.get_password() {
            Ok(hex) => decode_key(&hex).ok_or(CodecError::Corrupt),
            Err(keyring::Error::NoEntry) => {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).map_err(|_| CodecError::Locked)?;
                entry.set_password(&encode_key(&key)).map_err(|_| CodecError::Locked)?;
                Ok(key)
            }
            // The keystore is there and said no. That is the recoverable case,
            // and it is not the same as "there is no keystore".
            Err(_) => Err(CodecError::Locked),
        }
    }
}

impl SecretCodec for OsKeystoreCodec {
    fn available(&self) -> bool {
        // Reaching the entry at all — including finding nothing in it — means
        // there is a keystore. Anything else means there is not.
        match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            Ok(entry) => matches!(entry.get_password(), Ok(_) | Err(keyring::Error::NoEntry)),
            Err(_) => false,
        }
    }

    fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>, CodecError> {
        seal(&Self::data_key()?, plaintext)
    }

    fn decrypt(&self, protected: &[u8]) -> Result<String, CodecError> {
        open(&Self::data_key()?, protected)
    }
}

fn encode_key(key: &[u8; 32]) -> String {
    key.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_key(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let high = (bytes[index * 2] as char).to_digit(16)?;
        let low = (bytes[index * 2 + 1] as char).to_digit(16)?;
        *slot = (high * 16 + low) as u8;
    }
    Some(key)
}

/// The shared AES-256-GCM step, so both codecs protect bytes identically.
///
/// Split out because the fixed-key codec is a test double for the *keystore*, not
/// for the cryptography: a test that exercised a different cipher would pass while
/// the real one was broken.
fn seal(key: &[u8; 32], plaintext: &str) -> Result<Vec<u8>, CodecError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).map_err(|_| CodecError::Corrupt)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|_| CodecError::Corrupt)?;
    let mut protected = nonce_bytes.to_vec();
    protected.extend(ciphertext);
    Ok(protected)
}

fn open(key: &[u8; 32], protected: &[u8]) -> Result<String, CodecError> {
    // A blob with no room for a nonce is not one this codec wrote.
    if protected.len() < 12 {
        return Err(CodecError::Corrupt);
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&protected[..12]), &protected[12..])
        .map_err(|_| CodecError::Corrupt)?;
    String::from_utf8(plaintext).map_err(|_| CodecError::Corrupt)
}

/// A codec with a fixed key, for tests.
#[cfg(test)]
pub struct FixedKeyCodec([u8; 32]);

#[cfg(test)]
impl SecretCodec for FixedKeyCodec {
    fn available(&self) -> bool {
        true
    }
    fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>, CodecError> {
        seal(&self.0, plaintext)
    }
    fn decrypt(&self, protected: &[u8]) -> Result<String, CodecError> {
        open(&self.0, protected)
    }
}

/// A codec that reports no keystore, for the degraded-path test.
#[cfg(test)]
pub struct NoKeystoreCodec;

#[cfg(test)]
impl SecretCodec for NoKeystoreCodec {
    fn available(&self) -> bool {
        false
    }
    // Unreachable in practice: the store checks `available()` first and answers
    // `Unavailable` without calling a codec. Reaching either of these means a
    // caller skipped that check, which is a bug worth seeing rather than hiding.
    fn encrypt(&self, _: &str) -> Result<Vec<u8>, CodecError> {
        Err(CodecError::Locked)
    }
    fn decrypt(&self, _: &[u8]) -> Result<String, CodecError> {
        Err(CodecError::Locked)
    }
}

/// One stored row, exactly the shape the renderer reads back.
#[derive(Serialize, Deserialize)]
struct Row {
    id: String,
    revision: u64,
    #[serde(rename = "workspaceId")]
    workspace_id: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    spec: Value,
}

/// The store: one encrypted JSON array of rows.
pub struct ServiceDefinitionStore {
    file: PathBuf,
    codec: Arc<dyn SecretCodec>,
}

impl ServiceDefinitionStore {
    pub fn new(file: PathBuf, codec: Arc<dyn SecretCodec>) -> Self {
        Self { file, codec }
    }

    fn read(&self) -> Result<Vec<Row>, StoreError> {
        if !self.codec.available() {
            return Err(StoreError::Unavailable);
        }
        let bytes = match std::fs::read(&self.file) {
            Ok(bytes) => bytes,
            // Only `NotFound` is a first run. Anything else means the store is
            // there and unreadable — and the difference is every saved
            // configuration, API tokens in each `env` block included. The first
            // version matched `Err(_)`, so a locked file (a backup tool or AV
            // holding it on Windows) read as "nothing stored" and the next `save`
            // renamed an empty array over it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => {
                return Err(StoreError::Failed(format!(
                    "the configuration store cannot be read: {error}"
                )));
            }
        };
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(StoreError::Failed(
                "the configuration store exceeds its limit".into(),
            ));
        }
        let plain = self.codec.decrypt(&bytes).map_err(StoreError::from)?;
        let rows: Vec<Row> = serde_json::from_str(&plain).map_err(|_| StoreError::Corrupt)?;
        if rows.len() > MAX_ROWS {
            return Err(StoreError::Failed("too many saved configurations".into()));
        }
        // Every row is re-validated on read, not just on write: a row that would
        // be refused on save must not be served on load either.
        for row in &rows {
            validate_input(
                &json!({
                    "workspaceId": row.workspace_id,
                    "id": row.id,
                    "revision": row.revision,
                    "spec": row.spec,
                }),
                Action::Save,
            )
            .map_err(|_| StoreError::Corrupt)?;
        }
        Ok(rows)
    }

    fn write(&self, rows: &[Row]) -> Result<(), StoreError> {
        if !self.codec.available() {
            return Err(StoreError::Unavailable);
        }
        if rows.len() > MAX_ROWS {
            return Err(StoreError::Failed("too many saved configurations".into()));
        }
        let plain = serde_json::to_string(rows).map_err(|_| StoreError::Corrupt)?;
        let protected = self.codec.encrypt(&plain).map_err(StoreError::from)?;
        if protected.len() as u64 > MAX_FILE_BYTES {
            return Err(StoreError::Failed(
                "the configuration store exceeds its limit".into(),
            ));
        }
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                StoreError::Failed(format!("cannot create the configuration directory: {error}"))
            })?;
        }
        // Temp-then-rename, and 0600: a half-written store is a store that will
        // not decrypt, and a world-readable one is a store that protects nothing.
        let temporary = self.file.with_extension("enc.tmp");
        write_private(&temporary, &protected)?;
        // fsync before the rename, so the rename cannot be durable while the
        // ciphertext is not. See `artifacts::write_and_sync`.
        if let Ok(file) = std::fs::File::open(&temporary) {
            let _ = file.sync_all();
        }
        std::fs::rename(&temporary, &self.file).map_err(|error| {
            let _ = std::fs::remove_file(&temporary);
            StoreError::Failed(format!("cannot replace the configuration store: {error}"))
        })
    }

    /// Every row in one workspace, as the renderer's read model.
    pub fn list(&self, workspace_id: &str) -> Result<Vec<Value>, StoreError> {
        let _guard = lock();
        validate_workspace(workspace_id).map_err(StoreError::Invalid)?;
        Ok(render(&self.read()?, workspace_id))
    }

    /// Insert or update, guarded by the revision the caller last read.
    pub fn save(&self, input: &Value) -> Result<Vec<Value>, StoreError> {
        let _guard = lock();
        validate_input(input, Action::Save).map_err(StoreError::Invalid)?;
        let workspace_id = str_field(input, "workspaceId").unwrap_or_default().to_string();
        let id = str_field(input, "id").map(str::to_string);
        let revision = input.get("revision").and_then(Value::as_u64);
        let spec = input.get("spec").cloned().unwrap_or(Value::Null);

        let mut rows = self.read()?;
        let position = rows.iter().position(|row| {
            id.as_deref().is_some_and(|id| row.id == id) && row.workspace_id == workspace_id
        });
        // Optimistic concurrency, and the error names what to do about it: the
        // renderer shows it and the user refreshes.
        match (id.as_deref(), position) {
            (Some(_), Some(position)) => {
                if rows[position].revision != revision.unwrap_or_default() {
                    return Err(StoreError::Conflict(
                        "this configuration changed; refresh before saving".into(),
                    ));
                }
                rows[position] = Row {
                    id: id.unwrap_or_default(),
                    revision: revision.unwrap_or(0) + 1,
                    workspace_id: workspace_id.clone(),
                    updated_at: crate::chrome::now_iso_millis(),
                    spec,
                };
            }
            // An id that names no row is a row that was deleted underneath the
            // caller, which is the same answer as a stale revision.
            (Some(_), None) => {
                return Err(StoreError::Conflict(
                    "this configuration changed; refresh before saving".into(),
                ));
            }
            (None, _) => {
                if rows.len() >= MAX_ROWS {
                    return Err(StoreError::Failed("too many saved configurations".into()));
                }
                rows.push(Row {
                    id: new_id(),
                    revision: 1,
                    workspace_id: workspace_id.clone(),
                    updated_at: crate::chrome::now_iso_millis(),
                    spec,
                });
            }
        }
        self.write(&rows)?;
        Ok(render(&rows, &workspace_id))
    }

    /// Delete one row, guarded by its revision.
    pub fn remove(&self, input: &Value) -> Result<Vec<Value>, StoreError> {
        let _guard = lock();
        validate_input(input, Action::Remove).map_err(StoreError::Invalid)?;
        let workspace_id = str_field(input, "workspaceId").unwrap_or_default().to_string();
        let id = str_field(input, "id").unwrap_or_default().to_string();
        let revision = input.get("revision").and_then(Value::as_u64).unwrap_or_default();

        let mut rows = self.read()?;
        let Some(position) = rows
            .iter()
            .position(|row| row.id == id && row.workspace_id == workspace_id)
        else {
            return Err(StoreError::Conflict(
                "this configuration changed; refresh before deleting".into(),
            ));
        };
        if rows[position].revision != revision {
            return Err(StoreError::Conflict(
                "this configuration changed; refresh before deleting".into(),
            ));
        }
        let workspace_id = rows[position].workspace_id.clone();
        rows.remove(position);
        self.write(&rows)?;
        Ok(render(&rows, &workspace_id))
    }
}

fn render(rows: &[Row], workspace_id: &str) -> Vec<Value> {
    rows.iter()
        .filter(|row| row.workspace_id == workspace_id)
        .map(|row| {
            json!({
                "id": row.id,
                "revision": row.revision,
                "workspaceId": row.workspace_id,
                "updatedAt": row.updated_at,
                "spec": row.spec,
            })
        })
        .collect()
}

/// Write with owner-only permissions.
///
/// `File::create` alone would leave the mode to the umask, which on a multi-user
/// machine is often world-readable — and this file is the one place a service's
/// API token is written down.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        StoreError::Failed(format!("cannot write the configuration store: {error}"))
    })?;
    file.write_all(bytes).map_err(|error| {
        StoreError::Failed(format!("cannot write the configuration store: {error}"))
    })?;
    file.flush().map_err(|error| {
        StoreError::Failed(format!("cannot write the configuration store: {error}"))
    })
}

/// What went wrong, and whether the user can do something about it.
#[derive(Debug)]
pub enum StoreError {
    /// No keystore here. The renderer says storage is unavailable and offers to
    /// start services directly.
    Unavailable,
    /// A keystore exists and refused. The renderer tells the user to allow
    /// access and refresh; nothing is lost.
    Locked,
    /// The bytes will not open, or a row on disk is not one this shell would
    /// have written.
    Corrupt,
    /// The caller sent something that is not a definition.
    Invalid(String),
    /// A revision the caller held is stale.
    Conflict(String),
    /// Anything else, with a message that says what to do.
    Failed(String),
}

impl From<CodecError> for StoreError {
    fn from(error: CodecError) -> Self {
        match error {
            CodecError::Locked => Self::Locked,
            CodecError::Corrupt => Self::Corrupt,
        }
    }
}

impl StoreError {
    /// The message the renderer shows verbatim, mirroring the Electron shell's
    /// two strings: one for a locked keystore, one for everything else.
    pub fn message(&self) -> String {
        match self {
            Self::Unavailable => "This environment does not provide encrypted configuration storage. You can start services directly."
                .to_string(),
            Self::Locked => "Secure storage is locked. Allow Step Studio when the system asks for keychain access, then refresh. Saved configurations are kept."
                .to_string(),
            Self::Corrupt => {
                "Cannot access service configurations. Refresh and check secure local storage."
                    .to_string()
            }
            Self::Invalid(message) | Self::Conflict(message) | Self::Failed(message) => message.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    List,
    Save,
    Remove,
}

fn str_field<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input.get(field).and_then(Value::as_str)
}

/// Whether a string is a canonical UUID, which is what a definition `id` is.
fn is_id(value: &str) -> bool {
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

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A bounded, control-character-free workspace identity.
fn validate_workspace(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err("Invalid workspace identity".into());
    }
    Ok(())
}

/// Text of at most `max` bytes, optionally absent.
fn text(value: Option<&Value>, max: usize) -> Result<Option<&str>, String> {
    match value {
        None => Ok(None),
        Some(Value::String(text)) if text.len() <= max && !text.contains('\0') => Ok(Some(text)),
        Some(_) => Err("Invalid text".into()),
    }
}

fn required_text(value: Option<&Value>, max: usize) -> Result<String, String> {
    text(value, max)?
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Missing text".into())
}

/// `validateDefinitionInput`, ported.
///
/// One function for all three actions rather than three, because the rules are
/// mostly shared and the differences are exactly the interesting part: what each
/// action is allowed to send.
fn validate_input(raw: &Value, action: Action) -> Result<(), String> {
    let Some(object) = raw.as_object() else {
        return Err("Invalid service definition".into());
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "workspaceId" | "id" | "revision" | "spec"))
    {
        return Err("Unknown definition field".into());
    }
    let workspace_id = required_text(object.get("workspaceId"), 512)?;
    validate_workspace(&workspace_id)?;

    let id = match object.get("id") {
        None => None,
        Some(Value::String(id)) if is_id(id) => Some(id.as_str()),
        Some(_) => return Err("Invalid definition identity".into()),
    };
    let revision = match object.get("revision") {
        None => None,
        Some(Value::Number(number)) => Some(
            number
                .as_u64()
                .filter(|revision| *revision >= 1)
                .ok_or_else(|| "A definition revision is required".to_string())?,
        ),
        Some(_) => return Err("A definition revision is required".into()),
    };
    // A revision without an id names nothing to be concurrent about.
    if id.is_none() && revision.is_some() {
        return Err("Unexpected revision".into());
    }
    // An id without a revision cannot be checked for a stale write.
    if id.is_some() && revision.is_none() {
        return Err("A definition revision is required".into());
    }
    if action == Action::Remove && id.is_none() {
        return Err("A definition identity is required".into());
    }
    match action {
        Action::Save => {
            let spec = object.get("spec").ok_or("A service specification is required")?;
            validate_spec(spec)?;
        }
        Action::Remove => {
            if object.contains_key("spec") {
                return Err("Unexpected definition fields".into());
            }
        }
        // A list is a list: no spec, and no single row either.
        Action::List => {
            if object.contains_key("spec") || id.is_some() {
                return Err("Unexpected definition fields".into());
            }
        }
    }
    Ok(())
}

/// `validateServiceSpec`, ported field for field.
fn validate_spec(raw: &Value) -> Result<(), String> {
    let Some(spec) = raw.as_object() else {
        return Err("Invalid service specification".into());
    };
    if spec
        .keys()
        .any(|key| !matches!(key.as_str(), "name" | "command" | "cwd" | "env" | "pty" | "ready" | "restart" | "mode"))
    {
        return Err("Unknown service field".into());
    }
    let name = required_text(spec.get("name"), 48)?;
    if !name.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("Invalid service name".into());
    }
    required_text(spec.get("command"), 32768)?;
    if let Some(cwd) = text(spec.get("cwd"), 4096)? {
        let drive = cwd.len() > 1
            && cwd.starts_with(|c: char| c.is_ascii_alphabetic())
            && cwd.as_bytes()[1] == b':';
        if drive
            || cwd.starts_with('/')
            || cwd.starts_with('\\')
            || cwd.split(['/', '\\']).any(|segment| segment == "..")
        {
            return Err("Service cwd must be workspace-relative".into());
        }
    }
    if let Some(env) = spec.get("env") {
        let Some(env) = env.as_object() else {
            return Err("Invalid service environment".into());
        };
        if env.len() > 128 {
            return Err("Invalid service environment".into());
        }
        for (key, value) in env {
            if !key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                || matches!(key.as_str(), "__proto__" | "constructor" | "prototype")
            {
                return Err("Invalid environment name".into());
            }
            text(Some(value), 4096)?;
        }
    }
    if let Some(pty) = spec.get("pty") {
        if !pty.is_boolean() {
            return Err("Invalid service PTY option".into());
        }
    }
    if let Some(ready) = spec.get("ready") {
        let Some(ready) = ready.as_object() else {
            return Err("Invalid readiness condition".into());
        };
        if ready.keys().any(|key| !matches!(key.as_str(), "log" | "port" | "host" | "timeoutMs")) {
            return Err("Unknown readiness field".into());
        }
        let log = text(ready.get("log"), 1024)?;
        let port = match ready.get("port") {
            None => None,
            Some(Value::Number(number)) => Some(
                number
                    .as_u64()
                    .filter(|port| (1..=65535).contains(port))
                    .ok_or_else(|| "Invalid readiness port".to_string())?,
            ),
            Some(_) => return Err("Invalid readiness port".into()),
        };
        if log.is_none() && port.is_none() {
            return Err("A readiness condition is required".into());
        }
        // The pattern is compiled here rather than at service start, so a typo
        // is refused while the user is still looking at the form.
        if let Some(log) = log {
            regex::Regex::new(log).map_err(|_| "Invalid readiness pattern".to_string())?;
        }
        text(ready.get("host"), 253)?;
        match ready.get("timeoutMs") {
            None => return Err("A readiness timeout is required".into()),
            Some(Value::Number(number)) => {
                if !number.as_u64().is_some_and(|ms| (50..=3_600_000).contains(&ms)) {
                    return Err("Invalid readiness timeout".into());
                }
            }
            Some(_) => return Err("Invalid readiness timeout".into()),
        }
    }
    if let Some(restart) = text(spec.get("restart"), 16)? {
        if !matches!(restart, "no" | "on-failure" | "always") {
            return Err("Invalid restart policy".into());
        }
    }
    if let Some(mode) = text(spec.get("mode"), 16)? {
        if !matches!(mode, "session" | "persist" | "detached") {
            return Err("Invalid service mode".into());
        }
    }
    Ok(())
}

/// The store this shell owns, under `<state>/service-definitions.enc`.
pub fn store() -> ServiceDefinitionStore {
    ServiceDefinitionStore::new(store_path(), Arc::new(OsKeystoreCodec))
}

/// Serialises the tests that install `STORE_PATH_OVERRIDE`.
///
/// `cfg(test)`: nothing outside the tests touches it, and an unused static in a
/// release build is an error under `-D warnings`.
#[cfg(test)]
static STORE_PATH_SERIALISE: Mutex<()> = Mutex::new(());

/// Where the encrypted store lives.
pub fn store_path() -> PathBuf {
    // A test may have installed a directory. Same rule as `STATE_DIR_OVERRIDE` in
    // `chrome.rs`: checked first so a test never has to touch the process
    // environment, because `std::env::set_var` is `unsafe` and this workspace
    // forbids it.
    if let Some(dir) = STORE_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return dir.join("service-definitions.enc");
    }
    store_path_from_app()
}

/// The production path, with no test override applied.
fn store_path_from_app() -> PathBuf {
    let directory = crate::app_handle()
        .and_then(|handle| crate::chrome::state_dir(handle).ok())
        .unwrap_or_else(crate::chrome::shell_state_dir);
    directory.join("service-definitions.enc")
}

/// Where a test says the store lives. `None` in production.
static STORE_PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What the store's on-disk format is, for `migrate::classify`.
///
/// A version sidecar beside the encrypted blob, so the ciphertext itself is never
/// rewritten to add a version — rewriting it would mean decrypting and
/// re-encrypting under a key that may not be reachable, which is the one operation
/// whose failure cannot be recovered from.
fn recorded_format_version() -> Option<u64> {
    let sidecar = store_path().with_extension("enc.version");
    std::fs::read_to_string(sidecar).ok().and_then(|text| text.trim().parse().ok())
}

/// Whether the store's format is one this build understands.
///
/// M5.5 的第三种纪律是「schema 无版本号则拒绝启动并提示」。这里做成**拒绝该能力**
/// 而不是拒绝启动：一个打不开的应用没法让用户去修它的配置文件，而壳的其余部分与
/// 凭据存储无关。方案要的是「不静默读错」，这条给的是同一保证的更弱形式——不可用 +
/// 理由，而不是空白。
pub fn format_verdict() -> step_studio_host::migrate::Outcome {
    step_studio_host::migrate::classify(recorded_format_version())
}

/// List one workspace's saved configurations.
#[tauri::command]
pub fn list_service_definitions(input: Value) -> ChromeResult<Value> {
    refuse_unreadable_format()?;
    respond(Action::List, &input)
}

/// Insert or update one, guarded by the revision the caller last read.
#[tauri::command]
pub fn save_service_definition(input: Value) -> ChromeResult<Value> {
    refuse_unreadable_format()?;
    respond(Action::Save, &input)
}

/// Delete one, guarded by its revision.
#[tauri::command]
pub fn remove_service_definition(input: Value) -> ChromeResult<Value> {
    refuse_unreadable_format()?;
    respond(Action::Remove, &input)
}

/// The one guard all three share.
///
/// `Unversioned` and `TooNew` both mean the same thing to a caller: this build
/// must not read the file, because it would misread it. Answering with a message
/// rather than a read is what keeps a format bump from becoming silent corruption.
fn refuse_unreadable_format() -> ChromeResult<()> {
    let verdict = format_verdict();
    if verdict.is_usable() {
        return Ok(());
    }
    Err(ChromeError::failed(format!(
        "saved configurations are in a format this build does not read ({});          they are left untouched",
        verdict.reason()
    )))
}

/// Answer one of the three IPC calls with the Electron handler's shape.
fn respond(action: Action, input: &Value) -> ChromeResult<Value> {
    let store = store();
    let outcome = match action {
        Action::List => store.list(str_field(input, "workspaceId").unwrap_or_default()),
        Action::Save => store.save(input),
        Action::Remove => store.remove(input),
    };
    match outcome {
        Ok(definitions) => Ok(json!({ "ok": true, "definitions": definitions })),
        Err(error) => Ok(json!({ "ok": false, "message": error.message() })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> (PathBuf, ServiceDefinitionStore) {
        let dir = std::env::temp_dir().join(format!("svcdef-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a store directory");
        let store = ServiceDefinitionStore::new(
            dir.join("service-definitions.enc"),
            Arc::new(FixedKeyCodec([7u8; 32])),
        );
        (dir, store)
    }

    fn spec(name: &str) -> Value {
        json!({ "name": name, "command": "npm run dev" })
    }

    fn save(store: &ServiceDefinitionStore, workspace: &str, name: &str) -> Value {
        store
            .save(&json!({ "workspaceId": workspace, "spec": spec(name) }))
            .expect("saved")
            .first()
            .cloned()
            .expect("one row")
    }

    #[test]
    fn a_saved_configuration_round_trips_through_the_encrypted_file() {
        let (dir, store) = store("roundtrip");
        let row = save(&store, "ws-1", "web");
        assert_eq!(
            row.get("spec").and_then(|spec| spec.get("name")).and_then(Value::as_str),
            Some("web")
        );
        assert_eq!(row.get("revision").and_then(Value::as_u64), Some(1));
        let id = row.get("id").and_then(Value::as_str).unwrap_or("");

        // The bytes on disk are not the JSON. This is the whole point of the
        // module, so it is asserted rather than assumed.
        let bytes = std::fs::read(dir.join("service-definitions.enc")).expect("read the file");
        assert!(
            !bytes.windows(3).any(|window| window == b"web"),
            "the name is not in the clear"
        );
        assert!(bytes.len() > 12, "there is a nonce in front of the ciphertext");

        // And a fresh store over the same file reads it back, which is the
        // reload path: the key is in the codec, not in the process.
        let reopened = ServiceDefinitionStore::new(
            dir.join("service-definitions.enc"),
            Arc::new(FixedKeyCodec([7u8; 32])),
        );
        let rows = reopened.list("ws-1").expect("listed");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("id").and_then(Value::as_str), Some(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configuration_from_another_workspace_is_not_listed() {
        let (dir, store) = store("scope");
        save(&store, "ws-1", "web");
        save(&store, "ws-2", "api");
        assert_eq!(store.list("ws-1").expect("listed").len(), 1);
        assert_eq!(store.list("ws-2").expect("listed").len(), 1);
        assert!(store.list("ws-3").expect("listed").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_revision_is_refused_with_a_message_that_says_what_to_do() {
        // Optimistic concurrency: the renderer holds the revision it read, and a
        // second writer's save must not silently overwrite the first's.
        let (dir, store) = store("conflict");
        let row = save(&store, "ws-1", "web");
        let id = row.get("id").and_then(Value::as_str).unwrap_or("");
        assert!(
            store
                .save(&json!({ "workspaceId": "ws-1", "id": id, "revision": 1, "spec": spec("web") }))
                .is_ok(),
            "the revision the caller holds is current"
        );

        let error = store
            .save(&json!({ "workspaceId": "ws-1", "id": id, "revision": 1, "spec": spec("web-renamed") }))
            .expect_err("the revision is now stale");
        assert!(
            matches!(error, StoreError::Conflict(_)),
            "a stale revision is a conflict, not a failure: {error:?}"
        );
        assert!(
            error.message().contains("refresh"),
            "the message says what to do: {}",
            error.message()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_takes_the_row_and_checks_the_revision() {
        let (dir, store) = store("remove");
        let row = save(&store, "ws-1", "web");
        let id = row.get("id").and_then(Value::as_str).unwrap_or("");
        assert!(
            store
                .remove(&json!({ "workspaceId": "ws-1", "id": id, "revision": 99 }))
                .is_err(),
            "a wrong revision is refused"
        );
        let rows = store
            .remove(&json!({ "workspaceId": "ws-1", "id": id, "revision": 1 }))
            .expect("removed");
        assert!(rows.is_empty());
        assert!(store.list("ws-1").expect("listed").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_keystore_the_store_says_unavailable_rather_than_writing_anything() {
        // The degraded path the capability manifest exists for. Writing an
        // unencrypted file here would be the worst outcome: the renderer would
        // report success and the token would be in the clear.
        let dir = std::env::temp_dir().join(format!("svcdef-nokeys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a store directory");
        let store = ServiceDefinitionStore::new(
            dir.join("service-definitions.enc"),
            Arc::new(NoKeystoreCodec),
        );
        let outcome = store.save(&json!({ "workspaceId": "ws-1", "spec": spec("web") }));
        assert!(
            matches!(outcome, Err(StoreError::Unavailable)),
            "no keystore means unavailable: {outcome:?}"
        );
        assert!(
            !dir.join("service-definitions.enc").exists(),
            "and nothing is written"
        );
        // And the write path refuses on its own, which is a *second* check rather
        // than a duplicate: `read` is called first and would have caught this, so
        // a mutation that removes the guard in `write` leaves this test green. The
        // guard earns its place for the case neither covers — the keystore
        // disappearing between the read and the write, where falling back to
        // plaintext is exactly the failure this module exists to prevent.
        assert!(
            matches!(store.write(&[]), Err(StoreError::Unavailable)),
            "the write path refuses without a keystore, on its own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_store_is_an_error_rather_than_an_empty_one() {
        // The bug this pins: the first version matched `Err(_)` as "nothing stored
        // yet", so a store the process could not open — a backup tool or AV holding
        // it on Windows is the realistic case — read as an empty list, and the next
        // `save` renamed an empty array **over** it. Every saved configuration gone,
        // API tokens in each `env` block included: the exact asset this module
        // exists to protect.
        //
        // A directory at the store's path is the same "present but not usable"
        // answer a permission denial gives, and it is what a test can produce
        // without root.
        let dir = std::env::temp_dir().join(format!("svcdef-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a store directory");
        std::fs::create_dir_all(dir.join("service-definitions.enc")).expect("make a directory at the store path");
        let store = ServiceDefinitionStore::new(
            dir.join("service-definitions.enc"),
            Arc::new(FixedKeyCodec([7u8; 32])),
        );
        let outcome = store.list("ws-1");
        assert!(
            matches!(outcome, Err(StoreError::Failed(_))),
            "an unreadable store is an error, not an empty list: {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_that_will_not_decrypt_is_corrupt_rather_than_empty() {
        let (dir, store) = store("corrupt");
        std::fs::write(dir.join("service-definitions.enc"), b"not ciphertext at all").expect("write");
        assert!(
            matches!(store.list("ws-1"), Err(StoreError::Corrupt)),
            "bytes that will not open are corrupt"
        );
        // A different key over the same bytes is the same answer, which is the
        // case a re-signed app produces.
        let wrong_key = ServiceDefinitionStore::new(
            dir.join("service-definitions.enc"),
            Arc::new(FixedKeyCodec([9u8; 32])),
        );
        assert!(matches!(wrong_key.list("ws-1"), Err(StoreError::Corrupt)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_is_owner_only() {
        // The one place a service's API token is written down.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let (dir, store) = store("mode");
            save(&store, "ws-1", "web");
            let mode = std::fs::metadata(dir.join("service-definitions.enc"))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the store is owner-only, was {mode:o}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_definition_with_an_unknown_field_or_a_bad_name_is_refused() {
        let (dir, store) = store("validate");
        for bad in [
            json!({ "workspaceId": "ws-1", "spec": spec("web"), "extra": 1 }),
            json!({ "workspaceId": "", "spec": spec("web") }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "-web", "command": "x" } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "" } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "cwd": "/etc" } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "cwd": "../escape" } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "env": { "__proto__": "x" } } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "env": { "1BAD": "x" } } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "restart": "sometimes" } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "mode": "zombie" } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "ready": { "timeoutMs": 1000 } } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "ready": { "log": "(", "timeoutMs": 1000 } } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "ready": { "port": 70000, "timeoutMs": 1000 } } }),
            json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x", "ready": { "port": 3000, "timeoutMs": 1 } } }),
        ] {
            assert!(store.save(&bad).is_err(), "must be refused: {bad}");
        }
        assert!(
            store.list("ws-1").expect("listed").is_empty(),
            "nothing was stored"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_valid_spec_with_every_optional_field_is_accepted() {
        let (dir, store) = store("full");
        let row = store
            .save(&json!({
                "workspaceId": "ws-1",
                "spec": {
                    "name": "web.dev",
                    "command": "npm run dev -- --port 3000",
                    "cwd": "apps/web",
                    "env": { "NODE_ENV": "development", "API_KEY": "sks_not-a-real-token" },
                    "pty": true,
                    "ready": { "log": "listening on", "port": 3000, "host": "127.0.0.1", "timeoutMs": 60_000 },
                    "restart": "on-failure",
                    "mode": "persist",
                },
            }))
            .expect("saved")
            .first()
            .cloned()
            .expect("one row");
        let spec = row.get("spec").expect("spec");
        assert_eq!(spec.get("mode").and_then(Value::as_str), Some("persist"));
        assert_eq!(
            spec.pointer("/env/API_KEY").and_then(Value::as_str),
            Some("sks_not-a-real-token"),
            "the token round-trips, and only encrypted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_id_that_is_not_a_uuid_is_refused_before_it_becomes_a_key() {
        let (dir, store) = store("idshape");
        for id in ["../../etc/passwd", "not-a-uuid", ""] {
            assert!(
                store
                    .remove(&json!({ "workspaceId": "ws-1", "id": id, "revision": 1 }))
                    .is_err(),
                "{id} must be refused"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_revision_without_an_id_names_nothing_and_is_refused() {
        let (dir, store) = store("revisionalone");
        assert!(store
            .save(&json!({ "workspaceId": "ws-1", "revision": 3, "spec": spec("web") }))
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_real_codec_answers_consistently_about_the_keystore() {
        // The only test that touches the real OS keystore, and it asserts an
        // invariant rather than an outcome, because the outcome depends on the
        // machine: this one has libsecret installed but no Secret Service
        // running, a macOS box has a Keychain that may be locked, a CI container
        // has neither.
        //
        // What must hold everywhere: `available()` and `data_key()` never
        // disagree. If they did, a caller could be told a keystore exists, try to
        // write, and get a fresh key generated instead — which is the silent
        // orading this module exists to prevent.
        let codec = OsKeystoreCodec;
        if codec.available() {
            assert!(
                OsKeystoreCodec::data_key().is_ok(),
                "available() said there is a keystore, so a key must be reachable"
            );
            // And a round trip through the real cipher.
            let protected = codec.encrypt("a token").expect("encrypted");
            assert_eq!(codec.decrypt(&protected).expect("decrypted"), "a token");
        } else {
            assert!(
                matches!(codec.encrypt("a token"), Err(CodecError::Locked)),
                "no keystore means no key is invented"
            );
        }
    }

    /// Point `store_path()` at a directory of its own.
    ///
    /// The same two-lock split the Skillshare tests use: the path override is held
    /// only for the install and the restore, and a *separate* mutex serialises the
    /// tests. One lock for both deadlocks — `store_path()` locks it, and holding it
    /// across the call means the test locks against itself.
    fn with_store_path<T>(tag: &str, body: impl FnOnce(&Path) -> T) -> T {
        let dir = std::env::temp_dir().join(format!("svcdef-path-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("make a store directory");
        let _serialise = STORE_PATH_SERIALISE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *STORE_PATH_OVERRIDE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(dir.clone());
        let outcome = body(&dir);
        *STORE_PATH_OVERRIDE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    /// Writes the format sidecar the way a future build would.
    fn write_format_version(dir: &Path, version: u64) {
        std::fs::write(dir.join("service-definitions.enc.version"), version.to_string())
            .expect("write the sidecar");
    }

    #[test]
    fn a_store_with_no_format_version_is_refused_rather_than_read() {
        // M5.5 的第三种纪律。无版本号的 schema 不是旧的，是认不出来的——猜就会读错。
        // 这里做成拒绝该能力而不是拒绝启动：打不开的应用没法让用户去修配置文件，
        // 而壳的其余部分与凭据存储无关。方案要的是「不静默读错」，这条给同一保证。
        with_store_path("unversioned", |_| {
            let verdict = format_verdict();
            assert_eq!(verdict, step_studio_host::migrate::Outcome::Unversioned);
            assert!(!verdict.is_usable(), "an unversioned store must not be read");
            assert!(
                verdict.reason().contains("no format version"),
                "and the reason says which: {}",
                verdict.reason()
            );
        });
    }

    #[test]
    fn a_store_from_a_newer_format_is_refused() {
        with_store_path("toonew", |dir| {
            write_format_version(dir, step_studio_host::migrate::CURRENT_FORMAT_VERSION + 1);
            let verdict = format_verdict();
            assert!(!verdict.is_usable(), "a newer format must not be read as this one");
            assert!(verdict.reason().contains("newer"));
        });
    }

    #[test]
    fn a_store_at_the_current_format_is_read() {
        with_store_path("current", |dir| {
            write_format_version(dir, step_studio_host::migrate::CURRENT_FORMAT_VERSION);
            let verdict = format_verdict();
            assert!(verdict.is_usable(), "the current format is readable");
            assert_eq!(verdict, step_studio_host::migrate::Outcome::Current);
        });
    }

    #[test]
    fn the_three_commands_refuse_an_unreadable_format_before_touching_the_store() {
        // The guard runs first, so an unreadable store is never opened. Without it
        // the read would happen and either fail confusingly or — worse — succeed on
        // a file this build misparses.
        with_store_path("refuse", |_| {
            for command in [
                list_service_definitions(json!({ "workspaceId": "ws-1" })),
                save_service_definition(json!({ "workspaceId": "ws-1", "spec": { "name": "web", "command": "x" } })),
                remove_service_definition(json!({ "workspaceId": "ws-1", "id": "3f2504e0-4f89-11d3-9a0c-0305e82c3301", "revision": 1 })),
            ] {
                let error = command.expect_err("an unreadable format must be refused");
                assert!(
                    error.message.contains("format"),
                    "the message names the reason: {}",
                    error.message
                );
            }
        });
    }

    #[test]
    fn a_generated_id_is_a_canonical_uuid() {
        for _ in 0..50 {
            assert!(is_id(&new_id()));
        }
    }
}
