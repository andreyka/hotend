//! Secrets: provider API keys (and later printer access codes).
//!
//! Guarantees:
//! - From outside this crate a secret is write-only: it can be set, deleted and asked
//!   "is it set", and nothing returns its value. Reading needs a [`RevealToken`], which only
//!   this crate can create (the egress module does, to put the key on an outbound request).
//! - A [`Secret`] never prints its value: `Debug` is redacted, there is no `Display` and no
//!   serialisation, and the memory is wiped when it is dropped.
//! - Error messages never contain a secret.
//!
//! Backends: the system keychain (the default), the environment (read-only, opt-in with
//! `HOTEND_SECRETS=env`), one file of the person's own (opt-in with `HOTEND_SECRETS=file`,
//! see [`FileSecretStore`]) and memory (tests).

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::egress::ProviderKind;
use crate::search::{SearchService, search_key_name};

/// Keychain service name under which Hotend's entries are stored.
pub const KEYRING_SERVICE: &str = "hotend";
/// Longest accepted secret, in bytes.
pub const MAX_SECRET_BYTES: usize = 4096;

/// A secret value. Construct it from user input; only this crate can read it back.
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that must send it: the egress module.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Proof that the caller is this crate. Required to read a secret.
pub struct RevealToken(());

impl RevealToken {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

/// Why a secret operation failed. Never contains a secret value.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The operating system's keychain cannot be used in this session.
    #[error(
        "the system keychain is not available ({detail}). On a headless machine or in WSL, \
         start Hotend with HOTEND_SECRETS=file to keep keys in a file of your own, or with \
         the environment backend (HOTEND_SECRETS=env) and the key in the provider's usual \
         variable (for example ANTHROPIC_API_KEY)"
    )]
    Unavailable { detail: String },
    /// This backend cannot store or delete secrets.
    #[error(
        "this secret store is read-only: set the key in the environment instead, or start \
         Hotend with HOTEND_SECRETS=file to keep keys in a file of your own"
    )]
    ReadOnly,
    /// The name or value is not acceptable.
    #[error("{0}")]
    Invalid(&'static str),
    /// The keychain reported an error.
    #[error("the system keychain reported an error: {0}")]
    Backend(String),
    /// The secrets file or its folder could be read or changed by someone else (other
    /// users, a link to elsewhere). Nothing was read or written; the message says how to
    /// fix it.
    #[error("{0}")]
    Unsafe(String),
    /// The secrets file does not hold what Hotend writes. Its contents are never shown.
    #[error(
        "the secrets file {path} is damaged ({detail}); Hotend does not show what is in it. \
         Move it aside (for example `mv {} {}`) and store the keys again",
        shell_quote(.path),
        shell_quote(&broken_name(.path))
    )]
    Corrupt { path: String, detail: String },
    /// The secrets file could not be read or written; the message says what to do.
    #[error("{0}")]
    File(String),
    /// This backend does not work on this operating system.
    #[error("{0}")]
    Unsupported(&'static str),
}

/// `text` quoted for a POSIX shell, for the commands a refusal suggests.
pub(crate) fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn broken_name(path: &str) -> String {
    format!("{path}.broken")
}

/// Which kind of place a store keeps secrets in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStoreKind {
    /// The operating system keychain.
    Keychain,
    /// One file of the person's own ([`FileSecretStore`]).
    File,
    /// The environment: read-only.
    Environment,
    /// Memory, or a store a caller supplied.
    Memory,
}

impl SecretStoreKind {
    /// Whether a secret can be stored here (everything but the environment).
    pub fn writable(self) -> bool {
        self != Self::Environment
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::File => "file",
            Self::Environment => "environment",
            Self::Memory => "memory",
        }
    }
}

/// A place secrets are kept. Backends live in this crate.
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// What kind of store this is; a store supplied by a caller counts as memory.
    fn kind(&self) -> SecretStoreKind {
        SecretStoreKind::Memory
    }
    /// Stores or replaces a secret.
    fn set(&self, name: &str, secret: Secret) -> Result<(), SecretError>;
    /// Removes a secret; removing one that is not set succeeds.
    fn delete(&self, name: &str) -> Result<(), SecretError>;
    /// Whether a secret is set.
    fn is_set(&self, name: &str) -> Result<bool, SecretError>;
    /// Reads a secret. Callable only from inside this crate.
    fn reveal(&self, name: &str, token: &RevealToken) -> Result<Option<Secret>, SecretError>;
}

/// The secret name of a provider's API key.
pub fn provider_key_name(kind: ProviderKind) -> String {
    format!("provider-key.{}", kind.as_str())
}

fn check_name(name: &str) -> Result<(), SecretError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(SecretError::Invalid(
            "a secret name is 1 to 64 lower-case letters, digits, dots or dashes",
        ))
    }
}

fn check_value(secret: &Secret) -> Result<(), SecretError> {
    let value = secret.expose();
    if value.is_empty() || value.len() > MAX_SECRET_BYTES {
        return Err(SecretError::Invalid(
            "a key must have between 1 and 4096 characters",
        ));
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(SecretError::Invalid(
            "a key must not contain spaces or line breaks; paste only the key itself",
        ));
    }
    Ok(())
}

// ---- keychain ---------------------------------------------------------------------------

/// The operating system keychain (Keychain Services, Windows Credential Manager, the
/// Secret Service on Linux), through the `keyring` crate. This is the default backend.
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self {
            service: KEYRING_SERVICE.to_owned(),
        }
    }
}

impl KeyringSecretStore {
    fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
        check_name(name)?;
        keyring::Entry::new(&self.service, name).map_err(keyring_error)
    }
}

/// Maps a keychain error to ours without ever formatting a payload that could hold a secret.
fn keyring_error(error: keyring::Error) -> SecretError {
    match error {
        keyring::Error::NoDefaultStore => SecretError::Unavailable {
            detail: "no credential store could be opened".to_owned(),
        },
        keyring::Error::NoStorageAccess(inner) => SecretError::Unavailable {
            detail: crate::util::truncate(&inner.to_string(), 200),
        },
        keyring::Error::PlatformFailure(inner) => {
            SecretError::Backend(crate::util::truncate(&inner.to_string(), 200))
        }
        keyring::Error::BadEncoding(_) | keyring::Error::BadDataFormat(..) => {
            SecretError::Backend("the stored entry is not text".to_owned())
        }
        keyring::Error::TooLong(..) => SecretError::Invalid("the key is too long for the keychain"),
        _ => SecretError::Backend("unexpected keychain error".to_owned()),
    }
}

impl SecretStore for KeyringSecretStore {
    fn kind(&self) -> SecretStoreKind {
        SecretStoreKind::Keychain
    }

    fn set(&self, name: &str, secret: Secret) -> Result<(), SecretError> {
        check_value(&secret)?;
        self.entry(name)?
            .set_password(secret.expose())
            .map_err(keyring_error)
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keyring_error(error)),
        }
    }

    fn is_set(&self, name: &str) -> Result<bool, SecretError> {
        Ok(self.reveal(name, &RevealToken::new())?.is_some())
    }

    fn reveal(&self, name: &str, _token: &RevealToken) -> Result<Option<Secret>, SecretError> {
        match self.entry(name)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keyring_error(error)),
        }
    }
}

// ---- environment ------------------------------------------------------------------------

/// How [`EnvSecretStore`] reads one variable (replaced in tests).
type EnvLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Reads provider keys from the providers' usual environment variables. Read-only and
/// opt-in: for headless machines and WSL sessions without a keychain.
pub struct EnvSecretStore {
    lookup: EnvLookup,
}

impl fmt::Debug for EnvSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvSecretStore")
    }
}

impl Default for EnvSecretStore {
    fn default() -> Self {
        Self {
            lookup: Box::new(|name| std::env::var(name).ok()),
        }
    }
}

impl EnvSecretStore {
    #[cfg(test)]
    pub(crate) fn with_lookup(
        lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            lookup: Box::new(lookup),
        }
    }

    fn find(&self, name: &str) -> Option<Secret> {
        // A search service's key: its one variable (KEENABLE_API_KEY, TAVILY_API_KEY).
        if let Some(service) = SearchService::ALL
            .into_iter()
            .find(|service| search_key_name(*service) == name)
        {
            return (self.lookup)(service.env_var())
                .map(Secret::new)
                .filter(|secret| check_value(secret).is_ok());
        }
        let kind = ProviderKind::ALL
            .into_iter()
            .find(|kind| provider_key_name(*kind) == name)?;
        kind.rule()
            .env_vars
            .iter()
            .filter_map(|variable| (self.lookup)(variable))
            .map(Secret::new)
            .find(|secret| check_value(secret).is_ok())
    }
}

/// Whether the environment holds a usable value for the secret `name` (a provider's or a
/// search service's key in its usual variable). Never the value.
pub fn environment_has(name: &str) -> bool {
    EnvSecretStore::default().find(name).is_some()
}

impl SecretStore for EnvSecretStore {
    fn kind(&self) -> SecretStoreKind {
        SecretStoreKind::Environment
    }

    fn set(&self, _name: &str, _secret: Secret) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }

    fn delete(&self, _name: &str) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly)
    }

    fn is_set(&self, name: &str) -> Result<bool, SecretError> {
        Ok(self.find(name).is_some())
    }

    fn reveal(&self, name: &str, _token: &RevealToken) -> Result<Option<Secret>, SecretError> {
        Ok(self.find(name))
    }
}

// ---- file -------------------------------------------------------------------------------

/// What a program started outside the sandbox must not inherit: the variables that may hold
/// a secret, and the function that takes them out of a command's environment.
pub use hotend_printer::secret::{SECRET_ENV, without_secret_env};

/// Names the secrets file instead of the default (an absolute path only).
pub const SECRETS_FILE_ENV: &str = "HOTEND_SECRETS_FILE";
/// The secrets file's name in Hotend's configuration folder.
pub const SECRETS_FILE_NAME: &str = "secrets.json";
/// Largest secrets file Hotend reads.
#[cfg_attr(not(unix), allow(dead_code))]
const MAX_SECRETS_FILE_BYTES: u64 = 1024 * 1024;
/// The format version written in the file.
const SECRETS_FILE_FORMAT: u32 = 1;

/// Where the secrets file is: [`SECRETS_FILE_ENV`] when set (it must be absolute), else
/// `$XDG_CONFIG_HOME/hotend/secrets.json`, else `~/.config/hotend/secrets.json`.
pub fn default_secrets_file() -> Result<PathBuf, SecretError> {
    secrets_file_from(|name| std::env::var_os(name))
}

fn secrets_file_from(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, SecretError> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(path) = set(SECRETS_FILE_ENV) {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(SecretError::Invalid(
                "HOTEND_SECRETS_FILE must be an absolute path to a file, for example \
                 /home/you/.config/hotend/secrets.json",
            ));
        }
        return Ok(path);
    }
    let base = match set("XDG_CONFIG_HOME").filter(|path| path.is_absolute()) {
        Some(base) => base,
        None => set("HOME")
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                SecretError::File(
                    "the secrets file has no place: neither XDG_CONFIG_HOME nor HOME is an \
                     absolute path. Set HOTEND_SECRETS_FILE to an absolute path"
                        .to_owned(),
                )
            })?
            .join(".config"),
    };
    Ok(base.join("hotend").join(SECRETS_FILE_NAME))
}

/// What the secrets file holds. The values are wiped when it is dropped.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileContents {
    version: u32,
    secrets: BTreeMap<String, String>,
}

impl FileContents {
    #[cfg_attr(not(unix), allow(dead_code))]
    fn empty() -> Self {
        Self {
            version: SECRETS_FILE_FORMAT,
            secrets: BTreeMap::new(),
        }
    }
}

impl Drop for FileContents {
    fn drop(&mut self) {
        for value in self.secrets.values_mut() {
            value.zeroize();
        }
    }
}

/// Keeps secrets in one file of the person's own: opt-in with `HOTEND_SECRETS=file`, for
/// machines without a keychain (WSL, headless Linux). Weaker than a keychain: any program
/// running as the same user can read the file, as it can read an environment file.
///
/// Rules:
/// - The folder is made with mode 700 and the file with mode 600; an existing folder or
///   file that other users could read or change, that belongs to another user, or that is
///   a symbolic link is refused (never silently changed), with the command that fixes it.
/// - Only this store writes the file, by atomic replacement: a new file next to it (created
///   exclusively, mode 600, never through a link), flushed to disk, renamed over the old
///   one, and the folder flushed. A reader sees the old file or the new one, never half.
/// - Writers take a lock (an in-process mutex, and an advisory `flock` on a lock file in
///   the folder for other processes) and read the file again under it, so two writers
///   never lose each other's changes.
/// - A provider's or search service's key in its usual environment variable wins over the
///   file when reading (so existing environment files keep working); storing writes the
///   file.
/// - Linux and macOS only; elsewhere every call is [`SecretError::Unsupported`].
pub struct FileSecretStore {
    /// The file, or why there is none (shown on every call).
    path: Result<PathBuf, String>,
    env: EnvSecretStore,
    writer: Mutex<()>,
    #[cfg(test)]
    fail_before_rename: std::sync::atomic::AtomicBool,
}

impl fmt::Debug for FileSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileSecretStore")
    }
}

impl FileSecretStore {
    /// The store at [`default_secrets_file`], reading the real environment first.
    pub fn from_env() -> Self {
        Self::with_parts(
            default_secrets_file().map_err(|error| error.to_string()),
            EnvSecretStore::default(),
        )
    }

    /// The store at `path` (absolute), reading the real environment first.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self::with_parts(Ok(path.into()), EnvSecretStore::default())
    }

    fn with_parts(path: Result<PathBuf, String>, env: EnvSecretStore) -> Self {
        let path = path.and_then(|path| {
            if path.is_absolute() && path.file_name().is_some() {
                Ok(path)
            } else {
                Err(format!(
                    "the secrets file must be an absolute path to a file, not {}",
                    path.display()
                ))
            }
        });
        Self {
            path,
            env,
            writer: Mutex::new(()),
            #[cfg(test)]
            fail_before_rename: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn at_with_env(
        path: impl Into<PathBuf>,
        lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self::with_parts(Ok(path.into()), EnvSecretStore::with_lookup(lookup))
    }

    /// The file this store uses, when it has one.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref().ok()
    }

    /// Whether `name` is read from the environment rather than the file.
    pub fn from_environment(&self, name: &str) -> bool {
        self.env.find(name).is_some()
    }

    fn file(&self) -> Result<&Path, SecretError> {
        self.path
            .as_deref()
            .map_err(|why| SecretError::File(why.clone()))
    }

    fn writer(&self) -> std::sync::MutexGuard<'_, ()> {
        self.writer.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Changes the file under both locks; `change` says whether it changed anything.
    fn update(
        &self,
        create: bool,
        change: impl FnOnce(&mut FileContents) -> bool,
    ) -> Result<(), SecretError> {
        let path = self.file()?;
        let _guard = self.writer();
        #[cfg(test)]
        let fail = self
            .fail_before_rename
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let fail = false;
        file_store::update(path, create, fail, change)
    }
}

impl SecretStore for FileSecretStore {
    fn kind(&self) -> SecretStoreKind {
        SecretStoreKind::File
    }

    fn set(&self, name: &str, secret: Secret) -> Result<(), SecretError> {
        check_name(name)?;
        check_value(&secret)?;
        self.update(true, |contents| {
            if let Some(mut old) = contents
                .secrets
                .insert(name.to_owned(), secret.expose().to_owned())
            {
                old.zeroize();
            }
            true
        })
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        check_name(name)?;
        self.update(false, |contents| match contents.secrets.remove(name) {
            Some(mut old) => {
                old.zeroize();
                true
            }
            None => false,
        })
    }

    fn is_set(&self, name: &str) -> Result<bool, SecretError> {
        check_name(name)?;
        if self.env.find(name).is_some() {
            return Ok(true);
        }
        Ok(file_store::read(self.file()?)?.secrets.contains_key(name))
    }

    fn reveal(&self, name: &str, _token: &RevealToken) -> Result<Option<Secret>, SecretError> {
        check_name(name)?;
        if let Some(secret) = self.env.find(name) {
            return Ok(Some(secret));
        }
        let contents = file_store::read(self.file()?)?;
        Ok(contents.secrets.get(name).cloned().map(Secret::new))
    }
}

/// Parses the file's bytes; a refusal never quotes them.
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_contents(path: &Path, bytes: &[u8]) -> Result<FileContents, SecretError> {
    let corrupt = |detail: String| SecretError::Corrupt {
        path: path.display().to_string(),
        detail,
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(corrupt("it is empty".to_owned()));
    }
    // serde_json's messages can quote the text they stumbled on: only its position is used.
    let contents: FileContents = serde_json::from_slice(bytes).map_err(|error| {
        corrupt(format!(
            "not what Hotend writes, at line {} column {}",
            error.line(),
            error.column()
        ))
    })?;
    if contents.version != SECRETS_FILE_FORMAT {
        return Err(corrupt(format!(
            "format {} is not one this Hotend reads",
            contents.version
        )));
    }
    for (name, value) in &contents.secrets {
        if check_name(name).is_err() {
            return Err(corrupt(
                "it holds an entry with a name Hotend never writes".to_owned(),
            ));
        }
        let value = Secret::new(value.clone());
        if check_value(&value).is_err() {
            return Err(corrupt(format!(
                "the entry {name} does not hold a usable key"
            )));
        }
    }
    Ok(contents)
}

#[cfg(unix)]
mod file_store {
    //! The secrets folder is opened once, without following a link, and checked through
    //! the open handle; the file, the lock file and the temporary file are then opened,
    //! renamed and removed relative to that handle (`openat`, `renameat`, `unlinkat`), so a
    //! folder swapped for another after the check cannot redirect a read or a write.

    use std::ffi::{CStr, CString};
    use std::fs::{self, File, OpenOptions};
    use std::io::{ErrorKind, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};

    use zeroize::Zeroizing;

    use super::{
        FileContents, MAX_SECRETS_FILE_BYTES, SECRETS_FILE_FORMAT, SecretError, shell_quote,
    };

    fn me() -> u32 {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    fn io(what: &str, path: &Path, error: &std::io::Error) -> SecretError {
        SecretError::File(format!(
            "cannot {what} {}: {}",
            path.display(),
            crate::util::truncate(&error.to_string(), 200)
        ))
    }

    fn quoted(path: &Path) -> String {
        shell_quote(&path.display().to_string())
    }

    /// The secrets folder, open (`O_DIRECTORY | O_NOFOLLOW`) and checked.
    struct Folder {
        handle: File,
        path: PathBuf,
    }

    /// The file's folder and its name, as `openat` takes it.
    fn split(path: &Path) -> Result<(&Path, CString, String), SecretError> {
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .ok_or_else(|| {
                SecretError::File(format!("the secrets file {} has no folder", path.display()))
            })?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.contains('\0'))
            .ok_or_else(|| {
                SecretError::File(format!(
                    "the secrets file's name {} is not plain text",
                    path.display()
                ))
            })?;
        let c_name = CString::new(name)
            .map_err(|_| SecretError::File("the secrets file's name is not plain text".into()))?;
        Ok((dir, c_name, name.to_owned()))
    }

    fn c_name(name: &str) -> Result<CString, SecretError> {
        CString::new(name).map_err(|_| SecretError::File("a file name is not plain text".into()))
    }

    /// `openat(folder, name, flags, mode)` with `O_CLOEXEC`.
    fn open_at(
        folder: &Folder,
        name: &CStr,
        flags: libc::c_int,
        mode: libc::c_uint,
    ) -> std::io::Result<File> {
        // SAFETY: the folder's descriptor is open for as long as `folder` lives, `name` is
        // NUL-terminated, and the descriptor returned is new and owned by the `File` alone.
        let fd = unsafe {
            libc::openat(
                folder.handle.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC,
                mode,
            )
        };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: `fd` was just returned by `openat` and nothing else owns it.
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    fn unlink_at(folder: &Folder, name: &CStr) {
        // SAFETY: as in `open_at`; a failure is ignored (the caller only tidies up).
        let _ = unsafe { libc::unlinkat(folder.handle.as_raw_fd(), name.as_ptr(), 0) };
    }

    fn rename_at(folder: &Folder, from: &CStr, to: &CStr) -> std::io::Result<()> {
        let fd = folder.handle.as_raw_fd();
        // SAFETY: both names are NUL-terminated and the descriptor is open.
        if unsafe { libc::renameat(fd, from.as_ptr(), fd, to.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Whether `name` in the folder is a plain file of this user's (not following a link).
    fn plain_file_of_mine(folder: &Folder, name: &CStr) -> bool {
        // SAFETY: `stat` is plain data that `fstatat` fills in; all-zero is a valid value.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as in `open_at`; `stat` is a valid place to write.
        let found = unsafe {
            libc::fstatat(
                folder.handle.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        found && (stat.st_mode & libc::S_IFMT) == libc::S_IFREG && stat.st_uid == me()
    }

    fn check_folder(dir: &Path, meta: &fs::Metadata) -> Result<(), SecretError> {
        let shown = dir.display();
        if !meta.is_dir() {
            return Err(SecretError::Unsafe(format!(
                "{shown} should be the folder of the secrets file but is not a folder"
            )));
        }
        if meta.uid() != me() {
            return Err(SecretError::Unsafe(format!(
                "the folder of the secrets file, {shown}, belongs to another user (uid {}); \
                 use a folder of your own (set HOTEND_SECRETS_FILE)",
                meta.uid()
            )));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(SecretError::Unsafe(format!(
                "other users can open the folder of the secrets file, {shown} (mode {:03o}); \
                 Hotend left it as it is. Run `chmod 700 {}` and try again",
                meta.mode() & 0o777,
                quoted(dir)
            )));
        }
        Ok(())
    }

    fn symlinked_folder(dir: &Path) -> SecretError {
        SecretError::Unsafe(format!(
            "the folder of the secrets file, {}, is a symbolic link; Hotend does not follow \
             a link there. Replace it with a real folder (`mkdir -m 700`), or set \
             HOTEND_SECRETS_FILE to a file in one",
            dir.display()
        ))
    }

    fn check_file(path: &Path, meta: &fs::Metadata) -> Result<(), SecretError> {
        let shown = path.display();
        if !meta.is_file() {
            return Err(SecretError::Unsafe(format!(
                "the secrets file {shown} is not a plain file"
            )));
        }
        if meta.uid() != me() {
            return Err(SecretError::Unsafe(format!(
                "the secrets file {shown} belongs to another user (uid {}); Hotend does not \
                 use it. Remove it or set HOTEND_SECRETS_FILE to a file of your own",
                meta.uid()
            )));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(SecretError::Unsafe(format!(
                "other users can read or change the secrets file {shown} (mode {:03o}); \
                 Hotend left it as it is. Run `chmod 600 {}`, and replace the keys in it \
                 if others may have read them",
                meta.mode() & 0o777,
                quoted(path)
            )));
        }
        if meta.nlink() != 1 {
            return Err(SecretError::Unsafe(format!(
                "the secrets file {shown} has {} names (hard links), so the keys may also be \
                 reachable elsewhere; Hotend does not use it. Remove the other links and try \
                 again",
                meta.nlink()
            )));
        }
        Ok(())
    }

    /// The folder that holds the secrets folder must not be changeable by others (they
    /// could swap the secrets folder), unless it is a root-owned sticky folder like /tmp.
    fn check_parent(dir: &Path) -> Result<(), SecretError> {
        let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) else {
            return Ok(());
        };
        let meta = match fs::metadata(parent) {
            Ok(meta) => meta,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io("read", parent, &error)),
        };
        if !meta.is_dir() {
            return Err(SecretError::File(format!(
                "{} should be a folder but is not",
                parent.display()
            )));
        }
        let sticky_root = meta.mode() & 0o1000 != 0 && meta.uid() == 0;
        if meta.uid() != me() && meta.uid() != 0 {
            return Err(SecretError::Unsafe(format!(
                "the folder {} that holds the secrets folder belongs to another user (uid {}); \
                 set HOTEND_SECRETS_FILE to a file in a folder of your own",
                parent.display(),
                meta.uid()
            )));
        }
        if meta.mode() & 0o022 != 0 && !sticky_root {
            return Err(SecretError::Unsafe(format!(
                "other users can change the folder {} that holds the secrets folder (mode \
                 {:03o}); Hotend left it as it is. Run `chmod go-w {}`, or set \
                 HOTEND_SECRETS_FILE to a file in a folder of your own",
                parent.display(),
                meta.mode() & 0o7777,
                quoted(parent)
            )));
        }
        Ok(())
    }

    /// Opens and checks the secrets folder; with `create`, makes it (mode 700, and every
    /// missing folder above it mode 700) first. `None` when it is missing and not made.
    fn open_folder(dir: &Path, create: bool) -> Result<Option<Folder>, SecretError> {
        if create {
            if let Some(parent) = dir.parent().filter(|parent| !parent.exists()) {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(|error| io("make the folder", parent, &error))?;
            }
            check_parent(dir)?;
            match fs::DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io("make the folder", dir, &error)),
            }
        } else {
            check_parent(dir)?;
        }
        let handle = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)
        {
            Ok(handle) => handle,
            Err(error) if error.kind() == ErrorKind::NotFound && !create => return Ok(None),
            Err(error) if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                let link =
                    fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink());
                return Err(if link {
                    symlinked_folder(dir)
                } else {
                    SecretError::Unsafe(format!(
                        "{} should be the folder of the secrets file but is not a folder",
                        dir.display()
                    ))
                });
            }
            Err(error) => return Err(io("open the folder", dir, &error)),
        };
        // What was opened is checked, not what the name pointed at a moment before.
        let meta = handle.metadata().map_err(|error| io("read", dir, &error))?;
        check_folder(dir, &meta)?;
        Ok(Some(Folder {
            handle,
            path: dir.to_path_buf(),
        }))
    }

    /// Reads the file; a missing file (or folder) holds nothing.
    pub(super) fn read(path: &Path) -> Result<FileContents, SecretError> {
        let (dir, name, _) = split(path)?;
        match open_folder(dir, false)? {
            Some(folder) => read_in(&folder, &name, path),
            None => Ok(FileContents::empty()),
        }
    }

    fn read_in(folder: &Folder, name: &CStr, path: &Path) -> Result<FileContents, SecretError> {
        let mut file = match open_at(
            folder,
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(FileContents::empty()),
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(SecretError::Unsafe(format!(
                    "the secrets file {} is a symbolic link; Hotend does not follow it. Remove \
                     the link (Hotend makes a new file), or set HOTEND_SECRETS_FILE to the \
                     real file",
                    path.display()
                )));
            }
            Err(error) => return Err(io("open", path, &error)),
        };
        let meta = file.metadata().map_err(|error| io("read", path, &error))?;
        check_file(path, &meta)?;
        if meta.len() > MAX_SECRETS_FILE_BYTES {
            return Err(SecretError::Corrupt {
                path: path.display().to_string(),
                detail: "it is far larger than a file of keys".to_owned(),
            });
        }
        // Sized up front, so that the bytes are not copied around as the buffer grows.
        let size = usize::try_from(meta.len()).unwrap_or(0).saturating_add(1);
        let mut bytes = Zeroizing::new(Vec::with_capacity(size));
        (&mut file)
            .take(MAX_SECRETS_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| io("read", path, &error))?;
        super::parse_contents(path, &bytes)
    }

    /// Opens (making it if needed) the lock file in the folder and takes it.
    fn lock_in(folder: &Folder, name: &str) -> Result<File, SecretError> {
        let lock_name = format!(".{name}.lock");
        let path = folder.path.join(&lock_name);
        let file = open_at(
            folder,
            &c_name(&lock_name)?,
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW,
            0o600,
        )
        .map_err(|error| io("open the lock file", &path, &error))?;
        let meta = file
            .metadata()
            .map_err(|error| io("read the lock file", &path, &error))?;
        if !meta.is_file() || meta.uid() != me() {
            return Err(SecretError::Unsafe(format!(
                "the lock file {} is not a plain file of yours; remove it and try again",
                path.display()
            )));
        }
        file.lock().map_err(|error| io("lock", &path, &error))?;
        Ok(file)
    }

    /// Removes temporary files an earlier writer left behind (a crash between creating and
    /// renaming one). Called under the lock, when no other writer has one open.
    fn remove_leftovers(folder: &Folder, name: &str) {
        let prefix = format!(".{name}.");
        let Ok(entries) = fs::read_dir(&folder.path) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(found) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !(found.starts_with(&prefix) && found.ends_with(".tmp")) {
                continue;
            }
            // The names come from the path; what is removed is checked and removed through
            // the open folder.
            if let Ok(found) = CString::new(found)
                && plain_file_of_mine(folder, &found)
            {
                unlink_at(folder, &found);
            }
        }
    }

    /// Reads, changes and atomically replaces the file under the lock.
    pub(super) fn update(
        path: &Path,
        create: bool,
        fail_before_rename: bool,
        change: impl FnOnce(&mut FileContents) -> bool,
    ) -> Result<(), SecretError> {
        let (dir, c_file, name) = split(path)?;
        let Some(folder) = open_folder(dir, create)? else {
            return Ok(());
        };
        let _lock = lock_in(&folder, &name)?;
        remove_leftovers(&folder, &name);
        // Read again under the lock: another process may have written since.
        let mut contents = read_in(&folder, &c_file, path)?;
        if !change(&mut contents) {
            return Ok(());
        }
        contents.version = SECRETS_FILE_FORMAT;
        // Sized up front (keys hold no characters JSON must spell out at length), so that
        // the text is not copied around as the buffer grows.
        let estimate = 64
            + contents
                .secrets
                .iter()
                .map(|(key, value)| 2 * (key.len() + value.len()) + 16)
                .sum::<usize>();
        let mut bytes = Zeroizing::new(Vec::with_capacity(estimate));
        serde_json::to_writer_pretty(&mut *bytes, &contents)
            .map_err(|_| SecretError::File("cannot write the secrets file".to_owned()))?;
        bytes.push(b'\n');
        replace_in(&folder, &name, &c_file, path, &bytes, fail_before_rename)
    }

    fn replace_in(
        folder: &Folder,
        name: &str,
        c_file: &CStr,
        path: &Path,
        bytes: &[u8],
        fail_before_rename: bool,
    ) -> Result<(), SecretError> {
        let temporary = c_name(&format!(".{name}.{}.tmp", crate::util::new_id()))?;
        let mut file = open_at(
            folder,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        )
        .map_err(|error| io("write a new secrets file in", &folder.path, &error))?;
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                if fail_before_rename {
                    Err(std::io::Error::other("stopped before the rename (test)"))
                } else {
                    Ok(())
                }
            })
            .and_then(|()| rename_at(folder, &temporary, c_file));
        drop(file);
        if let Err(error) = written {
            unlink_at(folder, &temporary);
            return Err(io("replace", path, &error));
        }
        // The rename reaches the disk with the folder. The new file is in place either way,
        // so a failure to flush the folder is not reported as a failure to store the key.
        let _ = folder.handle.sync_all();
        Ok(())
    }
}

#[cfg(not(unix))]
mod file_store {
    use std::path::Path;

    use super::{FileContents, SecretError};

    const UNSUPPORTED: &str = "the file store (HOTEND_SECRETS=file) works on Linux and macOS \
                               only; on Windows Hotend keeps keys in the Credential Manager";

    pub(super) fn read(_path: &Path) -> Result<FileContents, SecretError> {
        Err(SecretError::Unsupported(UNSUPPORTED))
    }

    pub(super) fn update(
        _path: &Path,
        _create: bool,
        _fail_before_rename: bool,
        _change: impl FnOnce(&mut FileContents) -> bool,
    ) -> Result<(), SecretError> {
        Err(SecretError::Unsupported(UNSUPPORTED))
    }
}

// ---- memory -----------------------------------------------------------------------------

/// Keeps secrets in memory. For tests and for runs that must not touch the keychain.
#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, String>>,
}

impl fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemorySecretStore")
    }
}

impl MemorySecretStore {
    fn values(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.values.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SecretStore for MemorySecretStore {
    fn set(&self, name: &str, secret: Secret) -> Result<(), SecretError> {
        check_name(name)?;
        check_value(&secret)?;
        self.values()
            .insert(name.to_owned(), secret.expose().to_owned());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        check_name(name)?;
        if let Some(mut old) = self.values().remove(name) {
            old.zeroize();
        }
        Ok(())
    }

    fn is_set(&self, name: &str) -> Result<bool, SecretError> {
        Ok(self.values().contains_key(name))
    }

    fn reveal(&self, name: &str, _token: &RevealToken) -> Result<Option<Secret>, SecretError> {
        Ok(self.values().get(name).cloned().map(Secret::new))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const KEY: &str = "sk-test-0123456789abcdef";

    #[test]
    fn a_secret_never_prints_its_value() {
        let secret = Secret::new(KEY);
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{:?}", Some(&secret)).contains(KEY));
    }

    #[test]
    fn the_memory_store_sets_reports_and_deletes() {
        let store = MemorySecretStore::default();
        let name = provider_key_name(ProviderKind::Anthropic);
        assert!(!store.is_set(&name).unwrap());
        store.set(&name, Secret::new(KEY)).unwrap();
        assert!(store.is_set(&name).unwrap());
        assert!(!format!("{store:?}").contains(KEY));
        let revealed = store.reveal(&name, &RevealToken::new()).unwrap().unwrap();
        assert_eq!(revealed.expose(), KEY);
        store.delete(&name).unwrap();
        store.delete(&name).unwrap();
        assert!(!store.is_set(&name).unwrap());
    }

    #[test]
    fn bad_names_and_values_are_refused_without_echoing_the_value() {
        let store = MemorySecretStore::default();
        for value in [
            "",
            "has space",
            "line\nbreak",
            &"x".repeat(MAX_SECRET_BYTES + 1),
        ] {
            let error = store
                .set("provider-key.openai", Secret::new(value))
                .unwrap_err();
            assert!(matches!(error, SecretError::Invalid(_)));
            assert!(value.is_empty() || !error.to_string().contains(value));
        }
        assert!(store.set("../x", Secret::new(KEY)).is_err());
        assert!(store.set("UPPER", Secret::new(KEY)).is_err());
    }

    /// Every variable a secret is read from is taken out of the environment of a program
    /// Hotend starts for a link or a file, and nothing else is.
    #[test]
    fn the_secret_variables_are_the_ones_the_core_reads() {
        let mut read: Vec<&str> = ProviderKind::ALL
            .into_iter()
            .flat_map(|kind| kind.rule().env_vars.iter().copied())
            .chain(SearchService::ALL.into_iter().map(SearchService::env_var))
            .chain([SECRETS_FILE_ENV, crate::printer_lan::ACCESS_CODE_ENV])
            .collect();
        read.sort_unstable();
        let mut removed = SECRET_ENV.to_vec();
        removed.sort_unstable();
        assert_eq!(removed, read);
    }

    #[test]
    fn the_environment_store_reads_the_usual_variables_and_is_read_only() {
        let store = EnvSecretStore::with_lookup(|name| match name {
            "ANTHROPIC_API_KEY" => Some(KEY.to_owned()),
            "GEMINI_API_KEY" => Some("g-key".to_owned()),
            "OPENAI_API_KEY" => Some("bad key with spaces".to_owned()),
            _ => None,
        });
        let token = RevealToken::new();
        let anthropic = provider_key_name(ProviderKind::Anthropic);
        assert!(store.is_set(&anthropic).unwrap());
        assert_eq!(
            store.reveal(&anthropic, &token).unwrap().unwrap().expose(),
            KEY
        );
        let google = provider_key_name(ProviderKind::Google);
        assert_eq!(
            store.reveal(&google, &token).unwrap().unwrap().expose(),
            "g-key"
        );
        assert!(
            !store
                .is_set(&provider_key_name(ProviderKind::Openai))
                .unwrap()
        );
        assert!(
            !store
                .is_set(&provider_key_name(ProviderKind::Openrouter))
                .unwrap()
        );
        assert!(!store.is_set("something-else").unwrap());
        assert!(matches!(
            store.set(&anthropic, Secret::new(KEY)),
            Err(SecretError::ReadOnly)
        ));
        assert!(matches!(
            store.delete(&anthropic),
            Err(SecretError::ReadOnly)
        ));
    }

    #[test]
    fn search_keys_are_read_from_their_own_variables_like_the_model_key() {
        let store = EnvSecretStore::with_lookup(|name| match name {
            "KEENABLE_API_KEY" => Some("keen-test-0000".to_owned()),
            "TAVILY_API_KEY" => Some("has a space".to_owned()),
            _ => None,
        });
        let token = RevealToken::new();
        let keenable = search_key_name(SearchService::Keenable);
        assert_eq!(keenable, "search-key.keenable");
        assert!(store.is_set(&keenable).unwrap());
        assert_eq!(
            store.reveal(&keenable, &token).unwrap().unwrap().expose(),
            "keen-test-0000"
        );
        // A value that cannot be a key is not taken.
        assert!(
            !store
                .is_set(&search_key_name(SearchService::Tavily))
                .unwrap()
        );
        // The model key's variable does not stand in for a search key, nor the reverse.
        let other = EnvSecretStore::with_lookup(|name| {
            (name == "ANTHROPIC_API_KEY").then(|| KEY.to_owned())
        });
        assert!(!other.is_set(&keenable).unwrap());
        let memory = MemorySecretStore::default();
        memory
            .set(&keenable, Secret::new("keen-test-0000"))
            .unwrap();
        assert!(memory.is_set(&keenable).unwrap());
        assert!(!format!("{memory:?}").contains("keen-test"));
    }

    #[test]
    fn the_secrets_file_path_comes_from_the_override_or_the_configuration_folder() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        assert_eq!(
            secrets_file_from(vars(&[
                (SECRETS_FILE_ENV, "/tmp/x/s.json"),
                ("HOME", "/home/a")
            ]))
            .unwrap(),
            PathBuf::from("/tmp/x/s.json")
        );
        assert!(matches!(
            secrets_file_from(vars(&[(SECRETS_FILE_ENV, "relative/s.json")])),
            Err(SecretError::Invalid(_))
        ));
        assert_eq!(
            secrets_file_from(vars(&[("XDG_CONFIG_HOME", "/cfg"), ("HOME", "/home/a")])).unwrap(),
            PathBuf::from("/cfg/hotend/secrets.json")
        );
        assert_eq!(
            secrets_file_from(vars(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/home/a")])).unwrap(),
            PathBuf::from("/home/a/.config/hotend/secrets.json")
        );
        assert!(matches!(
            secrets_file_from(vars(&[])),
            Err(SecretError::File(_))
        ));
        let relative = FileSecretStore::at("relative/secrets.json");
        assert!(matches!(
            relative.is_set("provider-key.anthropic"),
            Err(SecretError::File(_))
        ));
    }

    #[cfg(unix)]
    mod file {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        use std::sync::Arc;

        use super::*;

        const NAME: &str = "provider-key.anthropic";

        /// A store whose environment holds nothing, in a fresh temporary folder.
        fn fresh() -> (tempfile::TempDir, PathBuf, FileSecretStore) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config/hotend/secrets.json");
            let store = FileSecretStore::at_with_env(&path, |_| None);
            (dir, path, store)
        }

        fn mode(path: &Path) -> u32 {
            std::fs::symlink_metadata(path).unwrap().mode() & 0o777
        }

        fn chmod(path: &Path, mode: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        fn leftovers(folder: &Path) -> Vec<String> {
            std::fs::read_dir(folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".tmp"))
                .collect()
        }

        fn refusal(error: &SecretError) -> String {
            let text = error.to_string();
            assert!(!text.contains(KEY), "{text}");
            assert!(!format!("{error:?}").contains(KEY));
            text
        }

        #[test]
        fn it_stores_reads_and_deletes_with_private_modes() {
            let (_dir, path, store) = fresh();
            let folder = path.parent().unwrap().to_path_buf();
            assert_eq!(store.kind(), SecretStoreKind::File);
            assert!(store.kind().writable());
            // Nothing there yet: nothing set, and asking makes no folder.
            assert!(!store.is_set(NAME).unwrap());
            assert!(!folder.exists());
            store.delete(NAME).unwrap();
            assert!(!folder.exists());

            store.set(NAME, Secret::new(KEY)).unwrap();
            assert_eq!(mode(&folder), 0o700);
            assert_eq!(mode(&path), 0o600);
            assert!(store.is_set(NAME).unwrap());
            assert!(!store.is_set("provider-key.openai").unwrap());
            let token = RevealToken::new();
            assert_eq!(store.reveal(NAME, &token).unwrap().unwrap().expose(), KEY);
            assert!(!format!("{store:?}").contains(KEY));

            // A second store on the same file sees it; a replacement replaces it.
            let other = FileSecretStore::at_with_env(&path, |_| None);
            other.set(NAME, Secret::new("sk-second-0000")).unwrap();
            assert_eq!(
                store.reveal(NAME, &token).unwrap().unwrap().expose(),
                "sk-second-0000"
            );
            store
                .set("search-key.tavily", Secret::new("tvly-0000"))
                .unwrap();
            store.delete(NAME).unwrap();
            assert!(!store.is_set(NAME).unwrap());
            assert!(store.is_set("search-key.tavily").unwrap());
            assert_eq!(mode(&path), 0o600);
            assert!(leftovers(&folder).is_empty());
            // Bad names and values are refused before the file is touched.
            assert!(matches!(
                store.set("../x", Secret::new(KEY)),
                Err(SecretError::Invalid(_))
            ));
            assert!(matches!(
                store.set(NAME, Secret::new("has space")),
                Err(SecretError::Invalid(_))
            ));
        }

        #[test]
        fn a_file_or_folder_others_can_open_is_refused_and_left_as_it_is() {
            let (_dir, path, store) = fresh();
            let folder = path.parent().unwrap().to_path_buf();
            store.set(NAME, Secret::new(KEY)).unwrap();
            for bad in [0o640, 0o604, 0o620, 0o602] {
                chmod(&path, bad);
                let error = store.is_set(NAME).unwrap_err();
                assert!(matches!(error, SecretError::Unsafe(_)), "{error:?}");
                let text = refusal(&error);
                assert!(text.contains("chmod 600") && text.contains(&path.display().to_string()));
                assert!(matches!(
                    store.set(NAME, Secret::new(KEY)),
                    Err(SecretError::Unsafe(_))
                ));
                assert!(matches!(
                    store.reveal(NAME, &RevealToken::new()),
                    Err(SecretError::Unsafe(_))
                ));
                assert_eq!(mode(&path), bad, "the mode must not be changed silently");
            }
            chmod(&path, 0o600);
            for bad in [0o750, 0o705, 0o770, 0o755] {
                chmod(&folder, bad);
                let error = store.is_set(NAME).unwrap_err();
                assert!(matches!(error, SecretError::Unsafe(_)), "{error:?}");
                assert!(refusal(&error).contains("chmod 700"));
                assert!(store.delete(NAME).is_err());
                assert_eq!(mode(&folder), bad);
            }
            chmod(&folder, 0o700);
            assert!(store.is_set(NAME).unwrap());
        }

        #[test]
        fn a_symlinked_file_is_refused_and_its_target_untouched() {
            let (dir, path, store) = fresh();
            let folder = path.parent().unwrap().to_path_buf();
            store
                .set("search-key.tavily", Secret::new("tvly-0000"))
                .unwrap();
            // A private file elsewhere, linked in place of the secrets file.
            let elsewhere = dir.path().join("elsewhere");
            std::fs::create_dir(&elsewhere).unwrap();
            chmod(&elsewhere, 0o700);
            let target = elsewhere.join("target.json");
            std::fs::write(&target, b"{\"version\":1,\"secrets\":{}}").unwrap();
            chmod(&target, 0o600);
            std::fs::remove_file(&path).unwrap();
            symlink(&target, &path).unwrap();

            let error = store.is_set(NAME).unwrap_err();
            assert!(matches!(error, SecretError::Unsafe(_)));
            assert!(refusal(&error).contains("symbolic link"));
            assert!(matches!(
                store.set(NAME, Secret::new(KEY)),
                Err(SecretError::Unsafe(_))
            ));
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                "{\"version\":1,\"secrets\":{}}"
            );
            assert!(
                std::fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(leftovers(&folder).is_empty());
        }

        #[test]
        fn a_symlinked_folder_is_refused_and_nothing_is_written_through_it() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("real");
            std::fs::create_dir(&real).unwrap();
            chmod(&real, 0o700);
            let linked = dir.path().join("linked");
            symlink(&real, &linked).unwrap();
            let store = FileSecretStore::at_with_env(linked.join("secrets.json"), |_| None);
            let error = store.set(NAME, Secret::new(KEY)).unwrap_err();
            assert!(matches!(error, SecretError::Unsafe(_)), "{error:?}");
            assert!(refusal(&error).contains("symbolic link"));
            assert!(matches!(store.is_set(NAME), Err(SecretError::Unsafe(_))));
            assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
        }

        #[test]
        fn a_file_of_another_user_is_refused() {
            // SAFETY: `geteuid` has no preconditions and cannot fail.
            if unsafe { libc::geteuid() } != 0 {
                println!("skipped: giving a file to another user needs root");
                return;
            }
            let (_dir, path, store) = fresh();
            store.set(NAME, Secret::new(KEY)).unwrap();
            std::os::unix::fs::chown(&path, Some(1), Some(1)).unwrap();
            let error = store.is_set(NAME).unwrap_err();
            assert!(matches!(error, SecretError::Unsafe(_)));
            assert!(refusal(&error).contains("another user"));
            std::os::unix::fs::chown(&path, Some(0), Some(0)).unwrap();
            std::os::unix::fs::chown(path.parent().unwrap(), Some(1), Some(1)).unwrap();
            let error = store.is_set(NAME).unwrap_err();
            assert!(refusal(&error).contains("another user"));
        }

        #[test]
        fn a_failed_write_leaves_the_old_file_whole_and_no_temporary_file() {
            let (_dir, path, store) = fresh();
            let folder = path.parent().unwrap().to_path_buf();
            store.set(NAME, Secret::new(KEY)).unwrap();
            let before = std::fs::read(&path).unwrap();
            store
                .fail_before_rename
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let error = store
                .set(NAME, Secret::new("sk-never-written-0000"))
                .unwrap_err();
            assert!(matches!(error, SecretError::File(_)));
            assert!(!error.to_string().contains("sk-never-written"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(leftovers(&folder).is_empty(), "{:?}", leftovers(&folder));
            store
                .fail_before_rename
                .store(false, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                store
                    .reveal(NAME, &RevealToken::new())
                    .unwrap()
                    .unwrap()
                    .expose(),
                KEY
            );
        }

        #[test]
        fn missing_folders_are_made_private_and_a_parent_others_can_change_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("a/b/hotend/secrets.json");
            let store = FileSecretStore::at_with_env(&path, |_| None);
            store.set(NAME, Secret::new(KEY)).unwrap();
            for folder in ["a", "a/b", "a/b/hotend"] {
                assert_eq!(mode(&dir.path().join(folder)), 0o700, "{folder}");
            }
            // The folder holding the secrets folder: others could swap the secrets folder.
            let parent = dir.path().join("a/b");
            for bad in [0o770, 0o707, 0o1777] {
                chmod(&parent, bad);
                let error = store.is_set(NAME).unwrap_err();
                assert!(matches!(error, SecretError::Unsafe(_)), "{error:?}");
                assert!(refusal(&error).contains("chmod go-w"), "{error}");
                assert!(store.set(NAME, Secret::new(KEY)).is_err());
                // A sticky folder counts only when root owns it.
                assert_eq!(
                    std::fs::metadata(&parent).unwrap().mode() & 0o7777,
                    bad,
                    "the mode must not be changed silently"
                );
            }
            chmod(&parent, 0o755);
            assert!(store.is_set(NAME).unwrap());
        }

        #[test]
        fn leftover_temporary_files_are_removed_under_the_lock_and_links_are_not_followed() {
            let (dir, path, store) = fresh();
            let folder = path.parent().unwrap().to_path_buf();
            store.set(NAME, Secret::new(KEY)).unwrap();
            let stale = folder.join(".secrets.json.0123-crashed.tmp");
            std::fs::write(&stale, b"half a file").unwrap();
            // A link with the same shape is not followed: its target stays.
            let target = dir.path().join("keep.txt");
            std::fs::write(&target, b"keep").unwrap();
            let link = folder.join(".secrets.json.link.tmp");
            symlink(&target, &link).unwrap();
            store
                .set("search-key.tavily", Secret::new("tvly-0000"))
                .unwrap();
            assert!(!stale.exists());
            assert_eq!(std::fs::read(&target).unwrap(), b"keep");
            assert!(std::fs::symlink_metadata(&link).is_ok());
            assert!(store.is_set(NAME).unwrap());
        }

        #[test]
        fn a_secrets_file_with_other_names_is_refused() {
            let (_dir, path, store) = fresh();
            store.set(NAME, Secret::new(KEY)).unwrap();
            let other = path.with_file_name("copy.json");
            std::fs::hard_link(&path, &other).unwrap();
            let error = store.is_set(NAME).unwrap_err();
            assert!(matches!(error, SecretError::Unsafe(_)), "{error:?}");
            assert!(refusal(&error).contains("hard links"));
            assert!(store.set(NAME, Secret::new(KEY)).is_err());
            std::fs::remove_file(&other).unwrap();
            assert!(store.is_set(NAME).unwrap());
        }

        #[test]
        fn the_commands_a_refusal_suggests_quote_the_path() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("it's here/secrets.json");
            let store = FileSecretStore::at_with_env(&path, |_| None);
            store.set(NAME, Secret::new(KEY)).unwrap();
            chmod(&path, 0o644);
            let text = refusal(&store.is_set(NAME).unwrap_err());
            let quoted = shell_quote(&path.display().to_string());
            assert!(quoted.contains("'\\''"), "{quoted}");
            assert!(text.contains(&format!("chmod 600 {quoted}")), "{text}");
            chmod(&path, 0o600);
            std::fs::write(&path, b"not json").unwrap();
            let text = refusal(&store.is_set(NAME).unwrap_err());
            assert!(text.contains(&format!("mv {quoted} ")), "{text}");
        }

        #[test]
        fn concurrent_writers_never_lose_each_others_keys() {
            let (_dir, path, store) = fresh();
            let folder = path.parent().unwrap().to_path_buf();
            // Threads on one store (the mutex), and two stores on one file (the file lock:
            // each store opens the lock file on its own, as another process would).
            let one = Arc::new(store);
            let two = Arc::new(FileSecretStore::at_with_env(&path, |_| None));
            let mut threads = Vec::new();
            for writer in 0..8 {
                let store = if writer % 2 == 0 {
                    Arc::clone(&one)
                } else {
                    Arc::clone(&two)
                };
                threads.push(std::thread::spawn(move || {
                    for round in 0..12 {
                        let name = format!("test-key.w{writer}-r{round}");
                        store
                            .set(&name, Secret::new(format!("value-{writer}-{round}")))
                            .unwrap();
                        // Readers in between see a whole file, never half of one.
                        assert!(store.is_set(&name).unwrap());
                    }
                }));
            }
            for thread in threads {
                thread.join().unwrap();
            }
            let token = RevealToken::new();
            for writer in 0..8 {
                for round in 0..12 {
                    let name = format!("test-key.w{writer}-r{round}");
                    assert_eq!(
                        one.reveal(&name, &token).unwrap().unwrap().expose(),
                        format!("value-{writer}-{round}")
                    );
                }
            }
            assert!(leftovers(&folder).is_empty());
            assert_eq!(mode(&path), 0o600);
        }

        #[test]
        fn a_damaged_file_is_a_typed_error_that_never_shows_its_contents() {
            let (_dir, path, store) = fresh();
            store.set(NAME, Secret::new(KEY)).unwrap();
            for damaged in [
                format!("{KEY} not json at all"),
                format!("{{\"version\":\"{KEY}\",\"secrets\":{{}}}}"),
                format!("{{\"version\":1,\"secrets\":{{\"{NAME}\":[\"{KEY}\"]}}}}"),
                format!("{{\"version\":1,\"secrets\":{{\"{NAME}\":\"{KEY} x\"}}}}"),
                format!("{{\"version\":1,\"secrets\":{{\"Bad Name\":\"{KEY}\"}}}}"),
                format!("{{\"version\":2,\"secrets\":{{\"{NAME}\":\"{KEY}\"}}}}"),
                format!("{{\"version\":1,\"secrets\":{{}},\"extra\":\"{KEY}\"}}"),
                String::new(),
            ] {
                std::fs::write(&path, &damaged).unwrap();
                let error = store.is_set(NAME).unwrap_err();
                assert!(matches!(error, SecretError::Corrupt { .. }), "{error:?}");
                let text = refusal(&error);
                assert!(
                    text.contains("damaged") && text.contains(".broken"),
                    "{text}"
                );
                // A write refuses too, rather than replacing keys it could not read.
                assert!(matches!(
                    store.set(NAME, Secret::new(KEY)),
                    Err(SecretError::Corrupt { .. })
                ));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), damaged);
            }
        }

        #[test]
        fn a_key_in_the_environment_wins_over_the_file_for_reading() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("hotend/secrets.json");
            let store = FileSecretStore::at_with_env(&path, |name| {
                (name == "ANTHROPIC_API_KEY").then(|| "sk-from-environment".to_owned())
            });
            let token = RevealToken::new();
            assert!(store.is_set(NAME).unwrap());
            assert!(store.from_environment(NAME));
            store.set(NAME, Secret::new(KEY)).unwrap();
            assert_eq!(
                store.reveal(NAME, &token).unwrap().unwrap().expose(),
                "sk-from-environment"
            );
            // The file holds what was stored; without the variable it is what is read.
            let plain = FileSecretStore::at_with_env(&path, |_| None);
            assert!(!plain.from_environment(NAME));
            assert_eq!(plain.reveal(NAME, &token).unwrap().unwrap().expose(), KEY);
            // Deleting removes the file's entry; the variable still answers.
            store.delete(NAME).unwrap();
            assert!(!plain.is_set(NAME).unwrap());
            assert!(store.is_set(NAME).unwrap());
            // A name with no variable (the printer's access code) comes from the file only.
            store
                .set("printer-lan.access-code", Secret::new("12345678"))
                .unwrap();
            assert!(!store.from_environment("printer-lan.access-code"));
            assert!(store.is_set("printer-lan.access-code").unwrap());
        }
    }

    #[test]
    fn the_keychain_store_answers_or_fails_with_a_typed_error() {
        // No test writes to a real keychain. On a machine without one (CI, WSL) the store
        // must report that it is unavailable and say what to do instead.
        let store = KeyringSecretStore {
            service: "hotend-test-nonexistent".to_owned(),
        };
        match store.is_set("provider-key.anthropic") {
            Ok(set) => assert!(!set),
            Err(error) => {
                assert!(matches!(
                    error,
                    SecretError::Unavailable { .. } | SecretError::Backend(_)
                ));
                println!("keychain not usable here: {error}");
            }
        }
        assert!(store.is_set("Bad Name").is_err());
    }
}
