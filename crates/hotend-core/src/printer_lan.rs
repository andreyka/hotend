//! Read-only printer status over the local network (PROTOCOL.md "Printer status
//! (read-only)").
//!
//! Guarantees:
//! - The only connection made is the printer layer's [`StatusListener`]: MQTT over TLS to
//!   the one address the person configured (a private-range IPv4 address, never a name),
//!   subscribed to that printer's report topic. It has no way to publish, so no command, no
//!   `pushall` and no file ever reaches the printer from here. Developer Mode is not needed
//!   and stays off.
//! - The printer's certificate is always checked, one of two ways: against the person's
//!   own file of Bambu Lab's certificate authorities, `<config>/bambu-ca.pem` (resolved and
//!   read here; Hotend does not ship that file: it is Bambu Lab's, and OpenBambuAPI, the
//!   collection that publishes it, is under the GFDL), or against a pin: the SHA-256 of the
//!   certificate the person confirmed after a first look ([`LanService::probe`] shows it; only
//!   [`LanService::confirm`] stores it, and only the fingerprint that look returned). Later
//!   connections with another certificate are refused. There is no unchecked mode.
//! - The LAN access code comes from the environment variable [`ACCESS_CODE_ENV`] (kept in a
//!   file of the person's own, mode 600, and loaded in their shell before Hotend starts) or
//!   from the secret store, where it is
//!   write-only like a provider key. It travels as an [`AccessCode`], which cannot be
//!   printed, serialised or cloned, and is never part of a setting, an audit row, an event,
//!   an error or a log line. The text read from the environment is moved into the
//!   [`AccessCode`] (which wipes it on drop) without a copy, and no program Hotend starts
//!   inherits the variable (see [`ACCESS_CODE_ENV`]).
//! - The address and serial are not secrets and are stored on their own key (not in
//!   [`crate::Settings`]); the pin is stored on another key that only `confirm` writes.
//! - The core enables the printer layer's `lan-status` feature only, never `lan`: the
//!   commanding transport (`LanPrinter`, command publishing, the FTPS upload and its
//!   `suppaftp` client) is not compiled into the application. These two examples check it
//!   in every workspace test run: the listener is there, the commanding printer is not.
//!
//! ```
//! use hotend_printer::lan::StatusListener;
//! ```
//!
//! ```compile_fail
//! use hotend_printer::lan::StatusListener;
//! use hotend_printer::lan::LanPrinter; // only with the `lan` feature
//! ```

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use hotend_printer::lan::tls::Issuers;
use hotend_printer::lan::{
    LanError, ListenOptions, ListenTarget, StatusListener, Trust, parse_listen_address,
    probe_certificate,
};
use hotend_printer::{AccessCode, PrinterProfile, PrinterStatus, Serial};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::CoreError;
use crate::printing::{Follower, LanPrinterStatus};
use crate::secrets::{RevealToken, Secret, SecretStore, SecretStoreKind};
use crate::store::{NewAudit, Store};
use crate::util::{now_ms, truncate};

/// The environment variable the access code is read from first. The sandbox starts children
/// with an empty environment, and a program started outside it for a link or a file has it
/// removed with the API keys ([`crate::secrets::without_secret_env`]). It cannot be removed
/// from this process's own environment (`std::env::remove_var` is unsound while other threads
/// run), which the web view's own helper processes inherit.
pub const ACCESS_CODE_ENV: &str = hotend_printer::secret::ACCESS_CODE_ENV;
/// The secret-store name of the access code.
pub const ACCESS_CODE_SECRET: &str = "printer-lan.access-code";
/// The file of Bambu Lab's certificate authorities, in Hotend's configuration folder.
pub const CA_FILE_NAME: &str = "bambu-ca.pem";
/// Environment variable that overrides Hotend's configuration folder.
pub const CONFIG_DIR_ENV: &str = "HOTEND_CONFIG_DIR";
/// Store key of the address and serial.
const ADDRESS_KEY: &str = "printer_lan";
/// Store key of the confirmed certificate.
const PIN_KEY: &str = "printer_lan_pin";
/// What a certificate's subject can and cannot prove, said wherever one is shown.
pub const SUBJECT_NOTE: &str = "The subject names the printer's serial number, which the \
    printer announces on the network, so any device can make a certificate that names it: \
    the subject alone proves nothing. Confirming pins this exact certificate (its SHA-256): \
    later connections must present the same one or are refused. Confirm only on a network \
    you trust, ideally right after checking that the printer is on and its serial matches \
    its screen. With Bambu Lab's certificate authorities in the configuration folder, the \
    certificate is checked against them instead and needs no confirmation.";

/// Hotend's configuration folder: `explicit`, else `HOTEND_CONFIG_DIR`, else the user's
/// configuration folder plus `hotend` (`~/.config/hotend` on Linux).
pub fn config_dir(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = explicit {
        return Some(dir.to_path_buf());
    }
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    directories::BaseDirs::new().map(|dirs| dirs.config_dir().join("hotend"))
}

/// The printer on the local network whose status Hotend reads. Not a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrinterLanAddress {
    /// A private-range IPv4 address, as four numbers.
    pub ip: String,
    /// The printer's serial number, upper case.
    pub serial: String,
}

impl PrinterLanAddress {
    /// Checks and normalises what the person typed. Errors never repeat the text.
    pub fn new(ip: &str, serial: &str) -> Result<Self, String> {
        let ip = parse_listen_address(ip).map_err(|error| error.to_string())?;
        let serial = Serial::new(serial).map_err(|_| {
            "the printer's serial number is 8 to 32 letters and digits, as shown on the \
             printer's screen"
                .to_owned()
        })?;
        Ok(Self {
            ip: ip.to_string(),
            serial: serial.as_str().to_owned(),
        })
    }

    fn parsed(&self) -> Result<(Ipv4Addr, Serial), CoreError> {
        let checked = Self::new(&self.ip, &self.serial).map_err(CoreError::InvalidInput)?;
        let ip = parse_listen_address(&checked.ip)
            .map_err(|error| CoreError::InvalidInput(error.to_string()))?;
        let serial = Serial::new(&checked.serial)
            .map_err(|error| CoreError::InvalidInput(error.to_string()))?;
        Ok((ip, serial))
    }
}

/// The certificate the person confirmed. Not a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificatePin {
    pub ip: String,
    pub serial: String,
    /// SHA-256 of the DER certificate, lower-case hex.
    pub sha256: String,
    pub subject: String,
    pub confirmed_at_ms: i64,
}

/// What a first look at the printer's certificate found, for the person to confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterCertificate {
    pub ip: String,
    pub serial: String,
    /// SHA-256 of the DER certificate, lower-case hex.
    pub sha256: String,
    pub subject: String,
    pub issuer: String,
    /// What the subject can and cannot prove ([`SUBJECT_NOTE`]).
    pub note: String,
}

/// How status reading is set up now. Never the access code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterLanSetup {
    pub address: Option<PrinterLanAddress>,
    /// `ca_file` (Bambu Lab's authorities from the configuration folder), `pinned` (a
    /// confirmed certificate for this address and serial) or `unconfirmed`.
    pub trust: String,
    /// Where Hotend looks for Bambu Lab's certificate authorities.
    pub ca_file: Option<PathBuf>,
    pub pin: Option<CertificatePin>,
    /// `environment` (BAMBU_LAN_ACCESS_CODE), `secret_store`, `missing` or `unreadable`.
    pub access_code: String,
    /// Where a code typed in Hotend would be kept: `keychain`, `file`, `memory`, or
    /// `environment`, which stores nothing (the code must then be in BAMBU_LAN_ACCESS_CODE).
    pub access_code_store: SecretStoreKind,
    /// Whether the core is following the printer now.
    pub watching: bool,
}

/// Where the status reports come from: the LAN listener, or a scripted source in tests.
pub(crate) trait StatusSource: Send {
    /// Waits until a report newer than `after` was merged, the session ended, or `timeout`
    /// passed.
    fn wait(&mut self, after: u64, timeout: Duration) -> SourceUpdate;
}

/// One look at a [`StatusSource`].
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceUpdate {
    pub(crate) status: PrinterStatus,
    pub(crate) connected: bool,
    pub(crate) ended: Option<String>,
}

struct ListenerSource(StatusListener);

impl StatusSource for ListenerSource {
    fn wait(&mut self, after: u64, timeout: Duration) -> SourceUpdate {
        let state = self.0.wait_for_update(after, timeout);
        SourceUpdate {
            status: state.status,
            connected: state.connected,
            ended: state.ended,
        }
    }
}

struct Watch {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Watch {
    /// Whether the session still runs (it ends by itself when the printer goes away).
    fn running(&self) -> bool {
        !self.thread.is_finished()
    }
}

/// The core's side of printer status: setup, certificate, access code and the session.
pub(crate) struct LanService {
    store: Arc<Store>,
    secrets: Arc<dyn SecretStore>,
    config_dir: Option<PathBuf>,
    /// The last first look, for `confirm`.
    probed: Mutex<Option<PrinterCertificate>>,
    watch: Mutex<Option<Watch>>,
    latest: Arc<Mutex<Option<LanPrinterStatus>>>,
    /// Reads one environment variable (replaced in tests).
    env: EnvLookup,
}

/// How [`LanService`] reads one environment variable.
type EnvLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

impl Drop for LanService {
    fn drop(&mut self) {
        if let Some(watch) = lock(&self.watch).take() {
            watch.stop.store(true, Ordering::SeqCst);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Refuses while a session runs; a session that ended by itself is cleared (its thread
/// joined) so that a new one can start. Called with the watch lock held.
fn clear_ended(watch: &mut Option<Watch>) -> Result<(), CoreError> {
    if watch.as_ref().is_some_and(Watch::running) {
        return Err(CoreError::InvalidInput(
            "already running: Hotend is already following the printer's status; stop it \
             first to start again"
                .to_owned(),
        ));
    }
    if let Some(ended) = watch.take() {
        let _ = ended.thread.join();
    }
    Ok(())
}

/// A printer-layer error as the person reads it; it never holds the access code.
fn lan_error(error: &LanError) -> CoreError {
    CoreError::NotConfigured(truncate(&error.to_string(), 600))
}

fn not_set_up() -> CoreError {
    CoreError::NotConfigured(
        "no printer address is set for reading its status: give the printer's IP address on \
         your network and its serial number (shown on the printer's screen) first"
            .to_owned(),
    )
}

fn no_access_code() -> CoreError {
    CoreError::NotConfigured(format!(
        "no LAN access code is available: set {ACCESS_CODE_ENV} in Hotend's environment \
         (keep it in a file of your own with mode 600, for example \
         ~/.config/hotend/bambu-lan.env, and load it in your shell before starting Hotend) or \
         store it through the write-only access-code setting. The code is shown on the \
         printer's screen"
    ))
}

impl LanService {
    pub(crate) fn new(
        store: Arc<Store>,
        secrets: Arc<dyn SecretStore>,
        config_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            store,
            secrets,
            config_dir,
            probed: Mutex::new(None),
            watch: Mutex::new(None),
            latest: Arc::new(Mutex::new(None)),
            env: Box::new(|name| std::env::var(name).ok()),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_env_for_tests(
        &mut self,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) {
        self.env = Box::new(env);
    }

    fn audit(&self, name: &str, outcome: Result<(), &str>, detail: Value) {
        let mut detail = detail;
        if let (Err(why), Some(object)) = (outcome, detail.as_object_mut()) {
            object.insert("error".to_owned(), Value::from(truncate(why, 600)));
        }
        let _ = self.store.audit(NewAudit {
            kind: "printer_lan",
            name: name.to_owned(),
            outcome: if outcome.is_ok() { "ok" } else { "failed" }.to_owned(),
            detail,
            ..NewAudit::default()
        });
    }

    fn ca_file(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|dir| dir.join(CA_FILE_NAME))
    }

    pub(crate) fn address(&self) -> Result<Option<PrinterLanAddress>, CoreError> {
        Ok(self
            .store
            .setting(ADDRESS_KEY)?
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value(value).ok()))
    }

    fn pin(&self) -> Result<Option<CertificatePin>, CoreError> {
        Ok(self
            .store
            .setting(PIN_KEY)?
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::from_value(value).ok()))
    }

    /// The pin, when it is for this address and serial.
    fn pin_for(&self, address: &PrinterLanAddress) -> Result<Option<CertificatePin>, CoreError> {
        Ok(self
            .pin()?
            .filter(|pin| pin.ip == address.ip && pin.serial == address.serial))
    }

    /// Saves the address and serial. A pin for another address or serial no longer applies
    /// (it is kept, and used again if they are set back).
    pub(crate) fn set_address(
        &self,
        ip: &str,
        serial: &str,
    ) -> Result<PrinterLanAddress, CoreError> {
        let address = PrinterLanAddress::new(ip, serial).map_err(CoreError::InvalidInput)?;
        self.store.set_setting(ADDRESS_KEY, &json!(address))?;
        self.audit(
            "address_set",
            Ok(()),
            json!({"ip": address.ip, "serial": address.serial}),
        );
        Ok(address)
    }

    pub(crate) fn clear_address(&self) -> Result<(), CoreError> {
        self.store.set_setting(ADDRESS_KEY, &Value::Null)?;
        self.audit("address_cleared", Ok(()), json!({}));
        Ok(())
    }

    /// Where the access code would come from now. Never the code.
    pub(crate) fn access_code_source(&self) -> &'static str {
        if (self.env)(ACCESS_CODE_ENV).is_some_and(|code| AccessCode::new(code).is_ok()) {
            return "environment";
        }
        match self.secrets.is_set(ACCESS_CODE_SECRET) {
            Ok(true) => "secret_store",
            Ok(false) => "missing",
            Err(_) => "unreadable",
        }
    }

    /// The access code: the environment first, then the secret store.
    fn access_code(&self) -> Result<AccessCode, CoreError> {
        if let Some(code) = (self.env)(ACCESS_CODE_ENV) {
            return AccessCode::new(code).map_err(|_| {
                CoreError::NotConfigured(format!(
                    "{ACCESS_CODE_ENV} is set but does not hold an access code (6 to 32 \
                     letters and digits, as shown on the printer's screen)"
                ))
            });
        }
        let secret = self
            .secrets
            .reveal(ACCESS_CODE_SECRET, &RevealToken::new())?
            .ok_or_else(no_access_code)?;
        AccessCode::new(secret.expose().to_owned()).map_err(|_| no_access_code())
    }

    /// Stores the access code. Write-only: nothing returns it.
    pub(crate) fn set_access_code(&self, code: String) -> Result<(), CoreError> {
        let outcome = store_access_code(&*self.secrets, code);
        self.audit(
            "access_code",
            outcome.as_ref().map(|_| ()).map_err(|_| "refused"),
            json!({"action": "set"}),
        );
        outcome
    }

    pub(crate) fn delete_access_code(&self) -> Result<(), CoreError> {
        let outcome = self
            .secrets
            .delete(ACCESS_CODE_SECRET)
            .map_err(CoreError::from);
        self.audit(
            "access_code",
            outcome
                .as_ref()
                .map(|_| ())
                .map_err(|_| "the secret store refused"),
            json!({"action": "delete"}),
        );
        outcome
    }

    /// How the certificate is checked for `address`, without connecting.
    fn trust(&self, address: &PrinterLanAddress) -> Result<(Trust, &'static str), CoreError> {
        if let Some(file) = self.ca_file()
            && std::fs::symlink_metadata(&file).is_ok()
        {
            // A file that is there must be usable: no quiet fall back to a pin.
            let issuers = Issuers::bambu_lab_from_file(&file)
                .ok_or_else(|| lan_error(&LanError::NoIssuers(file.display().to_string())))?;
            return Ok((Trust::Authorities(issuers), "ca_file"));
        }
        match self.pin_for(address)? {
            Some(pin) => Ok((Trust::Pinned(pin.sha256), "pinned")),
            None => Err(CoreError::NotConfigured(format!(
                "the printer's certificate has not been confirmed yet for {} (serial {}): take \
                 a first look at it, check it and confirm it; or put Bambu Lab's certificate \
                 authorities in {}. Nothing was connected to",
                address.ip,
                address.serial,
                self.ca_file()
                    .map_or_else(|| CA_FILE_NAME.to_owned(), |f| f.display().to_string())
            ))),
        }
    }

    pub(crate) fn setup(&self) -> Result<PrinterLanSetup, CoreError> {
        let address = self.address()?;
        let ca_present = self
            .ca_file()
            .is_some_and(|file| std::fs::symlink_metadata(file).is_ok());
        let pin = match &address {
            Some(address) => self.pin_for(address)?,
            None => None,
        };
        let trust = if ca_present {
            "ca_file"
        } else if pin.is_some() {
            "pinned"
        } else {
            "unconfirmed"
        };
        Ok(PrinterLanSetup {
            address,
            trust: trust.to_owned(),
            ca_file: self.ca_file(),
            pin,
            access_code: self.access_code_source().to_owned(),
            access_code_store: self.secrets.kind(),
            watching: lock(&self.watch).as_ref().is_some_and(Watch::running),
        })
    }

    /// A first look at the certificate of the configured printer: a TLS handshake, closed
    /// at once; nothing is sent. The result is kept for [`LanService::confirm`].
    pub(crate) fn probe(&self, profile: &PrinterProfile) -> Result<PrinterCertificate, CoreError> {
        let address = self.address()?.ok_or_else(not_set_up)?;
        let (ip, serial) = address.parsed()?;
        let target = ListenTarget::new(ip, serial, profile).map_err(|e| lan_error(&e))?;
        let seen = probe_certificate(&target);
        let detail = json!({"ip": address.ip, "serial": address.serial});
        let seen = match seen {
            Ok(seen) => seen,
            Err(error) => {
                self.audit("certificate_seen", Err(&error.to_string()), detail);
                return Err(lan_error(&error));
            }
        };
        let certificate = PrinterCertificate {
            ip: address.ip.clone(),
            serial: address.serial.clone(),
            sha256: seen.sha256,
            subject: seen.subject,
            issuer: seen.issuer,
            note: SUBJECT_NOTE.to_owned(),
        };
        self.audit(
            "certificate_seen",
            Ok(()),
            json!({"ip": address.ip, "serial": address.serial, "sha256": certificate.sha256}),
        );
        *lock(&self.probed) = Some(certificate.clone());
        Ok(certificate)
    }

    /// Stores the pin for the certificate the last first look returned, when `sha256` is
    /// its fingerprint and the address and serial are still the ones it was taken for. The
    /// only way a pin is stored.
    pub(crate) fn confirm(&self, sha256: &str) -> Result<CertificatePin, CoreError> {
        let address = self.address()?.ok_or_else(not_set_up)?;
        let wanted = sha256.trim().to_ascii_lowercase();
        let probed = lock(&self.probed).clone();
        let Some(probed) = probed.filter(|seen| {
            seen.ip == address.ip && seen.serial == address.serial && seen.sha256 == wanted
        }) else {
            self.audit(
                "certificate_confirmed",
                Err("not the certificate of the last first look"),
                json!({"ip": address.ip, "serial": address.serial}),
            );
            return Err(CoreError::InvalidInput(
                "this is not the fingerprint of the certificate Hotend last showed for this \
                 printer; take a first look again and confirm what it shows. Nothing was \
                 stored"
                    .to_owned(),
            ));
        };
        let pin = CertificatePin {
            ip: probed.ip,
            serial: probed.serial,
            sha256: probed.sha256,
            subject: probed.subject,
            confirmed_at_ms: now_ms(),
        };
        self.store.set_setting(PIN_KEY, &json!(pin))?;
        *lock(&self.probed) = None;
        self.audit(
            "certificate_confirmed",
            Ok(()),
            json!({"ip": pin.ip, "serial": pin.serial, "sha256": pin.sha256}),
        );
        Ok(pin)
    }

    pub(crate) fn forget_pin(&self) -> Result<(), CoreError> {
        self.store.set_setting(PIN_KEY, &Value::Null)?;
        self.audit("certificate_forgotten", Ok(()), json!({}));
        Ok(())
    }

    /// Opens a session with the configured printer: address, certificate check and access
    /// code are settled before anything is connected to.
    fn listen(&self, profile: &PrinterProfile) -> Result<StatusListener, CoreError> {
        let address = self.address()?.ok_or_else(not_set_up)?;
        let (ip, serial) = address.parsed()?;
        let (trust, mode) = self.trust(&address)?;
        let code = self.access_code()?;
        let target = ListenTarget::new(ip, serial, profile).map_err(|e| lan_error(&e))?;
        let detail = json!({"ip": address.ip, "serial": address.serial, "trust": mode});
        match StatusListener::connect(
            target,
            trust,
            code,
            profile.clone(),
            ListenOptions::default(),
        ) {
            Ok(listener) => {
                self.audit("session_open", Ok(()), detail);
                Ok(listener)
            }
            Err(error) => {
                self.audit("session_open", Err(&error.to_string()), detail);
                Err(lan_error(&error))
            }
        }
    }

    /// Reads the status once: connects, waits up to `wait` for a report, and ends the
    /// session. While the core follows the printer, the latest status is returned instead
    /// (a printer serves few clients at once).
    pub(crate) fn read(
        &self,
        profile: &PrinterProfile,
        mut follower: Follower,
        wait: Duration,
    ) -> Result<LanPrinterStatus, CoreError> {
        if lock(&self.watch).as_ref().is_some_and(Watch::running)
            && let Some(latest) = lock(&self.latest).clone()
        {
            return Ok(latest);
        }
        let listener = self.listen(profile)?;
        let state = listener.wait_for_update(0, wait.min(Duration::from_secs(120)));
        let view = follower.observe(&state.status, state.connected, state.ended.clone());
        drop(listener);
        Ok(view)
    }

    /// Follows the printer until [`LanService::stop`]: every report is matched and reported.
    ///
    /// One lock is held from the check to the store, the connection included: of two
    /// starts at the same time one connects and the other is refused as already running
    /// (a `read` or `stop` meanwhile waits for the connection attempt, at most
    /// [`hotend_printer::lan::CONNECT_TIMEOUT`] and the report wait).
    pub(crate) fn start(
        &self,
        profile: &PrinterProfile,
        follower: Follower,
    ) -> Result<(), CoreError> {
        let mut watch = lock(&self.watch);
        clear_ended(&mut watch)?;
        let listener = self.listen(profile)?;
        *watch = Some(self.spawn_watch(Box::new(ListenerSource(listener)), follower));
        Ok(())
    }

    /// Follows `source` until [`LanService::stop`] or until it ends; refused as already
    /// running while a session runs, under the same single lock as [`LanService::start`].
    /// Test builds only: scripted sources stand in for the printer.
    #[cfg(test)]
    pub(crate) fn start_with(
        &self,
        source: Box<dyn StatusSource>,
        follower: Follower,
    ) -> Result<(), CoreError> {
        let mut watch = lock(&self.watch);
        clear_ended(&mut watch)?;
        *watch = Some(self.spawn_watch(source, follower));
        Ok(())
    }

    /// Starts the thread that follows `source`. The caller holds the watch lock and stores
    /// the result under it.
    fn spawn_watch(&self, mut source: Box<dyn StatusSource>, mut follower: Follower) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let latest = Arc::clone(&self.latest);
        let store = Arc::clone(&self.store);
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut after = 0;
                let ended = loop {
                    if stop.load(Ordering::SeqCst) {
                        break None;
                    }
                    let update = source.wait(after, Duration::from_millis(250));
                    if update.status.reports > after || update.ended.is_some() {
                        after = update.status.reports;
                        let view = follower.observe(
                            &update.status,
                            update.connected,
                            update.ended.clone(),
                        );
                        *lock(&latest) = Some(view);
                        if update.ended.is_some() {
                            break update.ended;
                        }
                    }
                };
                // Dropping the source ends the session (DISCONNECT).
                drop(source);
                let _ = store.audit(NewAudit {
                    kind: "printer_lan",
                    name: "session_end".to_owned(),
                    outcome: "ok".to_owned(),
                    detail: json!({"why": ended.as_deref().map_or("stopped", |why| why)}),
                    ..NewAudit::default()
                });
            })
        };
        Watch { stop, thread }
    }

    /// Stops following; returns whether a session was running. Waits for it to end.
    pub(crate) fn stop(&self) -> bool {
        let Some(watch) = lock(&self.watch).take() else {
            return false;
        };
        watch.stop.store(true, Ordering::SeqCst);
        let _ = watch.thread.join();
        true
    }

    /// The latest status of the session, while one runs or after it ended.
    pub(crate) fn latest(&self) -> Option<LanPrinterStatus> {
        lock(&self.latest).clone()
    }
}

/// Runs blocking work (sockets, the keychain, a thread join) off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> Result<T, CoreError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| CoreError::Setup(format!("a printer status task failed: {error}")))?
}

/// Stores `code` in `secrets` as the access code, after checking it is one. The text is
/// never repeated in an error.
pub(crate) fn store_access_code(secrets: &dyn SecretStore, code: String) -> Result<(), CoreError> {
    let secret = Secret::new(code);
    if AccessCode::new(secret.expose().to_owned()).is_err() {
        return Err(CoreError::InvalidInput(
            "an access code is 6 to 32 letters and digits, as shown on the printer's screen; \
             nothing was stored"
                .to_owned(),
        ));
    }
    Ok(secrets.set(ACCESS_CODE_SECRET, secret)?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::collections::VecDeque;

    use super::*;
    use crate::core::{Core, CoreConfig, SecretBackend};
    use crate::printing::{CoreEvent, PrintEvent, PrintJobDetail};
    use crate::runtime::PythonRuntime;
    use crate::secrets::MemorySecretStore;
    use crate::store::{NewVersion, SliceRecord, VersionStatus};

    /// Stands in for an access code in these tests only.
    const CODE: &str = "s3cr3tc0de42";
    const SERIAL: &str = "20P0TEST00000003";

    fn profile() -> PrinterProfile {
        crate::profiles::printer_layer_profile(crate::profiles::DEFAULT_PRINTER_ID).unwrap()
    }

    fn service(dir: &Path) -> LanService {
        let data = crate::paths::DataDir::resolve(Some(&dir.join("data"))).unwrap();
        let store = Arc::new(Store::open(&data.db_path()).unwrap());
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::default());
        let mut lan = LanService::new(store, secrets, Some(dir.join("config")));
        // The process's own environment is never read by these tests.
        lan.set_env_for_tests(|_| None);
        lan
    }

    #[test]
    fn the_address_is_a_private_ipv4_address_and_the_serial_is_checked() {
        let address = PrinterLanAddress::new(" 192.168.1.50 ", "20p0test00000003").unwrap();
        assert_eq!(
            address,
            PrinterLanAddress {
                ip: "192.168.1.50".into(),
                serial: SERIAL.into()
            }
        );
        for (ip, serial) in [
            ("8.8.8.8", SERIAL),
            ("printer.local", SERIAL),
            ("127.0.0.1", SERIAL),
            ("10.0.0.2:8883", SERIAL),
            ("::1", SERIAL),
            ("192.168.1.50", "short"),
            ("192.168.1.50", "has space 1234"),
        ] {
            let error = PrinterLanAddress::new(ip, serial).unwrap_err();
            assert!(!error.contains("printer.local") && !error.contains("has space"));
        }
        // Not part of the settings document, which the window writes whole.
        let settings = serde_json::to_string(&crate::Settings::default()).unwrap();
        assert!(!settings.contains("printer_lan"));
    }

    #[test]
    fn the_access_code_comes_from_the_environment_or_the_store_and_is_never_shown() {
        let dir = tempfile::tempdir().unwrap();
        let mut lan = service(dir.path());
        assert_eq!(lan.access_code_source(), "missing");
        let missing = lan.access_code().unwrap_err().to_string();
        assert!(missing.contains(ACCESS_CODE_ENV), "{missing}");
        let refused = lan.set_access_code("not a code!".into()).unwrap_err();
        assert!(!refused.to_string().contains("not a code"));
        lan.set_access_code(CODE.into()).unwrap();
        assert_eq!(lan.access_code_source(), "secret_store");
        let code = lan.access_code().unwrap();
        assert!(!format!("{code:?}").contains(CODE));

        lan.set_env_for_tests(|name| (name == ACCESS_CODE_ENV).then(|| "envc0de123".to_owned()));
        assert_eq!(lan.access_code_source(), "environment");
        lan.set_env_for_tests(|name| (name == ACCESS_CODE_ENV).then(|| "bad code!".to_owned()));
        let error = lan.access_code().unwrap_err().to_string();
        assert!(!error.contains("bad code"), "{error}");

        lan.set_address("192.168.1.50", SERIAL).unwrap();
        // Nothing the core keeps or shows holds the code.
        let audit = serde_json::to_string(&lan.store.list_audit(None, 100).unwrap()).unwrap();
        assert!(
            audit.contains("access_code") && !audit.contains(CODE),
            "{audit}"
        );
        let setup = serde_json::to_string(&lan.setup().unwrap()).unwrap();
        assert!(!setup.contains(CODE), "{setup}");
        for key in [ADDRESS_KEY, PIN_KEY, crate::settings::SETTINGS_KEY] {
            let value = lan.store.setting(key).unwrap();
            assert!(!format!("{value:?}").contains(CODE));
        }
        lan.set_env_for_tests(|_| None);
        lan.delete_access_code().unwrap();
        assert_eq!(lan.access_code_source(), "missing");
    }

    #[cfg(unix)]
    #[test]
    fn a_code_stored_in_the_file_store_reaches_the_listener_and_says_where_it_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let data = crate::paths::DataDir::resolve(Some(&dir.path().join("data"))).unwrap();
        let store = Arc::new(Store::open(&data.db_path()).unwrap());
        let file = dir.path().join("config/hotend/secrets.json");
        let secrets: Arc<dyn SecretStore> =
            Arc::new(crate::secrets::FileSecretStore::at_with_env(&file, |_| {
                None
            }));
        let mut lan = LanService::new(store, secrets, Some(dir.path().join("config")));
        lan.set_env_for_tests(|_| None);
        assert_eq!(
            lan.setup().unwrap().access_code_store,
            SecretStoreKind::File
        );
        lan.set_access_code(CODE.into()).unwrap();
        assert_eq!(lan.access_code_source(), "secret_store");
        // What the listener would be given comes from the file, not the environment.
        let code = lan.access_code().unwrap();
        assert!(!format!("{code:?}").contains(CODE));
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("printer-lan.access-code")
        );
        // The variable still wins when it is set.
        lan.set_env_for_tests(|name| (name == ACCESS_CODE_ENV).then(|| "envc0de123".to_owned()));
        assert_eq!(lan.access_code_source(), "environment");
        lan.set_env_for_tests(|_| None);
        lan.delete_access_code().unwrap();
        assert_eq!(lan.access_code_source(), "missing");
        // The read-only store says so before anything is typed.
        let data = crate::paths::DataDir::resolve(Some(&dir.path().join("data2"))).unwrap();
        let store = Arc::new(Store::open(&data.db_path()).unwrap());
        let env: Arc<dyn SecretStore> = Arc::new(crate::secrets::EnvSecretStore::default());
        let lan = LanService::new(store, env, Some(dir.path().join("config")));
        let setup = lan.setup().unwrap();
        assert_eq!(setup.access_code_store, SecretStoreKind::Environment);
        assert!(!setup.access_code_store.writable());
    }

    #[test]
    fn the_certificate_is_checked_by_the_ca_file_or_by_a_pin_the_person_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let lan = service(dir.path());
        // Nothing set up: refused before anything is connected to.
        assert!(matches!(
            lan.probe(&profile()),
            Err(CoreError::NotConfigured(_))
        ));
        let address = lan.set_address("192.168.1.50", SERIAL).unwrap();
        let error = lan.trust(&address).err().unwrap().to_string();
        assert!(
            error.contains("not been confirmed") && error.contains("Nothing was connected to"),
            "{error}"
        );
        assert_eq!(lan.setup().unwrap().trust, "unconfirmed");

        // A confirmation needs a first look, and exactly its fingerprint.
        assert!(matches!(
            lan.confirm(&"ab".repeat(32)),
            Err(CoreError::InvalidInput(_))
        ));
        let sha = "0f".repeat(32);
        *lock(&lan.probed) = Some(PrinterCertificate {
            ip: address.ip.clone(),
            serial: address.serial.clone(),
            sha256: sha.clone(),
            subject: format!("CN={SERIAL}"),
            issuer: "CN=Some CA".into(),
            note: SUBJECT_NOTE.into(),
        });
        assert!(lan.confirm(&"ab".repeat(32)).is_err());
        let pin = lan.confirm(&sha.to_uppercase()).unwrap();
        assert_eq!(pin.sha256, sha);
        assert!(matches!(
            lan.trust(&address).unwrap(),
            (Trust::Pinned(pinned), "pinned") if pinned == sha
        ));
        assert!(lan.confirm(&sha).is_err(), "a first look confirms once");

        // The pin holds for its address and serial only.
        let other = lan.set_address("192.168.1.51", SERIAL).unwrap();
        assert!(lan.trust(&other).is_err());
        assert_eq!(lan.setup().unwrap().trust, "unconfirmed");
        lan.set_address("192.168.1.50", SERIAL).unwrap();
        assert_eq!(lan.setup().unwrap().trust, "pinned");

        // A CA file that is there but holds no Bambu Lab authority is refused, not ignored.
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join(CA_FILE_NAME), "no certificate here").unwrap();
        let error = lan.trust(&address).err().unwrap().to_string();
        assert!(error.contains(CA_FILE_NAME), "{error}");
        assert_eq!(lan.setup().unwrap().trust, "ca_file");
        std::fs::remove_file(config.join(CA_FILE_NAME)).unwrap();

        // Pinned, but no access code: refused before connecting.
        let error = lan.listen(&profile()).unwrap_err().to_string();
        assert!(error.contains("access code"), "{error}");
        lan.forget_pin().unwrap();
        assert_eq!(lan.setup().unwrap().trust, "unconfirmed");
        let audit = lan.store.list_audit(None, 100).unwrap();
        assert!(
            audit
                .iter()
                .any(|row| row.name == "certificate_confirmed" && row.outcome == "ok")
        );
    }

    /// A printer that reports these statuses, one per wait, and then goes away.
    struct Scripted(VecDeque<PrinterStatus>, PrinterStatus);

    impl Scripted {
        /// Hand-written reports with the short field names of the printer's report topic,
        /// merged one after another as the listener merges them.
        fn new(reports: &[&str]) -> Self {
            let profile = profile();
            let mut status = PrinterStatus::default();
            let mut statuses = VecDeque::new();
            for report in reports {
                let message = hotend_printer::protocol::report::parse(report.as_bytes()).unwrap();
                status.apply(&message.print.unwrap(), &profile);
                statuses.push_back(status.clone());
            }
            Self(statuses, status)
        }
    }

    impl StatusSource for Scripted {
        fn wait(&mut self, _after: u64, _timeout: Duration) -> SourceUpdate {
            match self.0.pop_front() {
                Some(status) => SourceUpdate {
                    status,
                    connected: true,
                    ended: None,
                },
                None => SourceUpdate {
                    status: self.1.clone(),
                    connected: false,
                    ended: Some("the script ended".into()),
                },
            }
        }
    }

    /// A printer that stays connected and silent until the session is stopped.
    struct Silent;

    impl StatusSource for Silent {
        fn wait(&mut self, _after: u64, timeout: Duration) -> SourceUpdate {
            std::thread::sleep(timeout.min(Duration::from_millis(20)));
            SourceUpdate {
                connected: true,
                ..SourceUpdate::default()
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn of_starts_at_the_same_time_one_runs_and_the_others_are_refused_as_already_running() {
        let dir = tempfile::tempdir().unwrap();
        let core = core(dir.path()).await;
        let outcomes: Vec<Result<(), CoreError>> = std::thread::scope(|scope| {
            let starts: Vec<_> = (0..8)
                .map(|_| {
                    let (lan, follower) = core.printer_watch_parts_for_tests();
                    scope.spawn(move || lan.start_with(Box::new(Silent), follower))
                })
                .collect();
            starts
                .into_iter()
                .map(|start| start.join().unwrap())
                .collect()
        });
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        for refused in outcomes.iter().filter_map(|outcome| outcome.as_ref().err()) {
            assert!(
                refused.to_string().starts_with("already running"),
                "{refused}"
            );
        }
        let (lan, follower) = core.printer_watch_parts_for_tests();
        assert!(lan.start_with(Box::new(Silent), follower).is_err());
        assert!(lan.stop(), "the one session was running");
        assert!(!lan.stop());
        let (lan, follower) = core.printer_watch_parts_for_tests();
        lan.start_with(Box::new(Silent), follower).unwrap();
        assert!(lan.stop());
    }

    /// A job handed off to Bambu Connect whose placed file is `path`.
    fn handed_off_job(core: &Core, path: &str, version: u32) -> String {
        let store = core.store_for_tests();
        let data = crate::paths::DataDir::resolve(Some(core.data_dir())).unwrap();
        let project = match store.list_projects().unwrap().into_iter().next() {
            Some(project) => project,
            None => store.create_project("Box").unwrap(),
        };
        let staged = data.jobs_dir().join(format!("staged-{version}"));
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("model.py"), "x").unwrap();
        store
            .create_version(
                &data,
                &project.id,
                &staged,
                NewVersion {
                    parent: None,
                    origin: "write_model_code".into(),
                    note: "v".into(),
                    status: VersionStatus::Ok,
                    code_sha256: "00".into(),
                    params: std::collections::BTreeMap::new(),
                },
            )
            .unwrap();
        let record = SliceRecord {
            project_id: project.id.clone(),
            version,
            profile_hash: "abc".into(),
            profile: "0.20mm".into(),
            summary: json!({"time_s": 60}),
            job: json!({"plate": 1}),
            created_at_ms: 1,
        };
        store.put_slice(&record).unwrap();
        let job = store.create_pending_print(&record, "ff", "note").unwrap();
        let detail = PrintJobDetail {
            message: Some("saved to the hand-off folder".into()),
            path: Some(path.into()),
            link: Some("bambu-connect://import-file?path=x".into()),
            ..PrintJobDetail::default()
        };
        store
            .update_print_job(
                &job.id,
                &["pending"],
                "handed_off",
                Some("bambu_connect"),
                &json!(detail),
            )
            .unwrap();
        job.id
    }

    /// Every event until a `Printer` event says the session ended.
    async fn until_ended(
        events: &mut tokio::sync::broadcast::Receiver<CoreEvent>,
    ) -> Vec<CoreEvent> {
        let mut seen = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .expect("the session ends")
                .unwrap();
            let last = matches!(&event, CoreEvent::Printer { status } if status.ended.is_some());
            seen.push(event);
            if last {
                return seen;
            }
        }
    }

    async fn core(dir: &Path) -> Core {
        Core::open(CoreConfig {
            data_dir: Some(dir.join("data")),
            secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
            runtime: Some(PythonRuntime {
                python: dir.join("no-python"),
                read_only: Vec::new(),
            }),
            config_dir: Some(dir.join("config")),
            ..CoreConfig::default()
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_handed_off_job_is_followed_by_its_name_and_other_prints_are_not_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let core = core(dir.path()).await;
        core.printer_lan_key_set(CODE.into()).await.unwrap();
        core.printer_lan_set("192.168.1.50", SERIAL).await.unwrap();
        let path = "/mnt/d/Models/Hotend/Box-v3-0123abcd.gcode.3mf";
        let job_id = handed_off_job(&core, path, 1);
        let mut events = core.events();

        core.printer_watch_source_for_tests(Box::new(Scripted::new(&[
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING", "stg_cur": 0,
                "subtask_name": "Box-v3-0123abcd", "mc_percent": 40, "mc_remaining_time": 25,
                "layer_num": 10, "total_layer_num": 50, "a_new_field": [1, 2]}}"#,
            r#"{"print": {"command": "push_status", "bed_temper": 55, "chamber_temper": 31}}"#,
            r#"{"print": {"command": "push_status", "gcode_state": "FINISH", "mc_percent": 100,
                "mc_remaining_time": 0}}"#,
        ])))
        .unwrap();
        let seen = until_ended(&mut events).await;

        let printer: Vec<&LanPrinterStatus> = seen
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Printer { status } => Some(&**status),
                _ => None,
            })
            .collect();
        assert_eq!(printer.len(), 4, "three reports and the end");
        assert!(
            printer
                .iter()
                .all(|s| s.job_id.as_deref() == Some(job_id.as_str()))
        );
        let running = printer[0];
        assert_eq!(running.state.phase, "printing");
        assert_eq!(
            (running.state.layer, running.state.total_layers),
            (Some(10), Some(50))
        );
        assert_eq!(running.state.progress_percent, Some(40));
        assert_eq!(running.state.remaining_minutes, Some(25));
        assert_eq!(running.matched_name.as_deref(), Some("Box-v3-0123abcd"));
        assert_eq!(printer[1].bed.current_c, Some(55.0));
        assert_eq!(printer[1].chamber.current_c, Some(31.0));
        assert_eq!(
            printer[1].state.progress_percent,
            Some(40),
            "a partial report keeps the rest"
        );
        assert_eq!(printer[2].state.phase, "finished");
        assert!(!printer[3].connected);

        let job_events: Vec<&PrintEvent> = seen
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Print {
                    job_id: id,
                    simulated,
                    event,
                    ..
                } if *id == job_id => {
                    assert!(!simulated);
                    Some(event)
                }
                _ => None,
            })
            .collect();
        assert!(
            job_events
                .iter()
                .any(|e| matches!(e, PrintEvent::Status { job } if job.status == "handed_off"))
        );
        assert!(
            job_events
                .iter()
                .any(|e| matches!(e, PrintEvent::Phase { phase, .. } if phase == "printing"))
        );
        assert!(
            job_events
                .iter()
                .any(|e| matches!(e, PrintEvent::Progress { percent: 40, .. }))
        );
        assert!(
            job_events
                .iter()
                .any(|e| matches!(e, PrintEvent::Phase { phase, .. } if phase == "finished"))
        );

        let job = core.print_status(&job_id).await.unwrap();
        assert_eq!(
            job.status, "handed_off",
            "Hotend did not start it: the status stays"
        );
        assert_eq!(job.detail.phase.as_deref(), Some("finished"));
        let message = job.detail.message.clone().unwrap();
        assert!(
            message.contains("the printer reports this job finished")
                && message.contains("followed from the printer's own status reports"),
            "{message}"
        );
        assert_eq!(
            job.detail.path.as_deref(),
            Some(Path::new(path)),
            "the hand-off detail stays"
        );
        assert!(job.detail.link.is_some());
        assert!(core.printer_watch_latest().unwrap().ended.is_some());

        // Another print on the printer: shown as the printer's status, claimed by nobody.
        let before = core.print_status(&job_id).await.unwrap();
        core.printer_watch_source_for_tests(Box::new(Scripted::new(&[
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING",
                "subtask_name": "someone-elses-print", "mc_percent": 5}}"#,
        ])))
        .unwrap();
        let seen = until_ended(&mut events).await;
        for event in &seen {
            match event {
                CoreEvent::Printer { status } => assert_eq!(status.job_id, None),
                CoreEvent::Print { .. } => panic!("no job event for a print that is not ours"),
                CoreEvent::Run { .. } => {}
            }
        }
        assert_eq!(core.print_status(&job_id).await.unwrap(), before);

        // The name is compared exactly, case included, and `subtask_name` when given is the
        // only name looked at (the file it prints is no second chance).
        for report in [
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING",
                "subtask_name": "BOX-V3-0123ABCD", "mc_percent": 6}}"#,
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING",
                "subtask_name": "someone-elses-print",
                "gcode_file": "/data/Box-v3-0123abcd.gcode.3mf", "mc_percent": 6}}"#,
        ] {
            core.printer_watch_source_for_tests(Box::new(Scripted::new(&[report])))
                .unwrap();
            for event in until_ended(&mut events).await {
                match event {
                    CoreEvent::Printer { status } => assert_eq!(status.job_id, None),
                    CoreEvent::Print { .. } => panic!("no job event for another name"),
                    CoreEvent::Run { .. } => {}
                }
            }
            assert_eq!(core.print_status(&job_id).await.unwrap(), before);
        }

        // Once matched, a report naming another job neither updates the job nor moves the
        // match: it is the printer's status only.
        core.printer_watch_source_for_tests(Box::new(Scripted::new(&[
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING",
                "subtask_name": "Box-v3-0123abcd", "mc_percent": 7}}"#,
            r#"{"print": {"command": "push_status", "subtask_name": "someone-elses-print",
                "mc_percent": 60}}"#,
        ])))
        .unwrap();
        let switched: Vec<Option<String>> = until_ended(&mut events)
            .await
            .into_iter()
            .filter_map(|event| match event {
                CoreEvent::Printer { status } => Some(status.job_id),
                _ => None,
            })
            .collect();
        assert_eq!(switched, [Some(job_id.clone()), None, None]);
        let job = core.print_status(&job_id).await.unwrap();
        assert_eq!(job.status, "handed_off");
        assert_eq!(job.detail.progress_percent, Some(7));

        // Two handed-off jobs with the same file: neither is claimed.
        let twin = handed_off_job(&core, path, 2);
        core.printer_watch_source_for_tests(Box::new(Scripted::new(&[
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING",
                "gcode_file": "/data/Box-v3-0123abcd.gcode.3mf", "mc_percent": 1}}"#,
        ])))
        .unwrap();
        for event in until_ended(&mut events).await {
            if let CoreEvent::Printer { status } = event {
                assert_eq!(status.job_id, None);
            }
        }
        assert_eq!(core.print_status(&twin).await.unwrap().detail.phase, None);

        // The access code is in no event, no audit row, no setting and no setup.
        let shown = format!(
            "{} {} {} {}",
            serde_json::to_string(&seen).unwrap(),
            serde_json::to_string(&core.audit_log(None, 1000).await.unwrap()).unwrap(),
            serde_json::to_string(&core.settings_get().await.unwrap()).unwrap(),
            serde_json::to_string(&core.printer_lan_get().await.unwrap()).unwrap(),
        );
        assert!(!shown.contains(CODE));
        assert_eq!(core.printer_lan_key_status().await.unwrap(), "secret_store");
    }
}
