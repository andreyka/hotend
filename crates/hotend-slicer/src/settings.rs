//! Slicer settings profiles, looked up by id.
//!
//! A slice names its machine, process and filament settings by id. The files come from two
//! places only: a profile folder the application owns, and the demo profiles compiled into
//! this crate. A job cannot carry setting values, so nothing an AI wrote can become start or
//! end G-code. The chosen files are copied into the job folder for the slicer to read.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::SlicerError;
use crate::job::valid_profile_id;
use crate::version::SlicerKind;

/// Largest settings file that is accepted.
const MAX_PROFILE_BYTES: u64 = 4 * 1024 * 1024;

/// Which of the three settings files a profile is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileRole {
    Machine,
    Process,
    Filament,
}

impl ProfileRole {
    /// Folder name of this role in a profile folder.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Process => "process",
            Self::Filament => "filament",
        }
    }
}

/// Where a profile was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileOrigin {
    /// One of the generic, untuned profiles compiled into this crate.
    Demo,
    /// A file in the application's profile folder.
    Installed(PathBuf),
}

/// One settings file, ready to be written into a job folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsProfile {
    pub id: String,
    pub role: ProfileRole,
    pub origin: ProfileOrigin,
    pub content: Vec<u8>,
}

/// The file extension of settings files for a slicer.
pub fn settings_extension(kind: SlicerKind) -> &'static str {
    match kind {
        SlicerKind::BambuStudio => "json",
        SlicerKind::PrusaSlicer => "ini",
    }
}

macro_rules! demo {
    ($kind:expr, $role:expr, $id:literal, $path:literal) => {
        (
            $kind,
            $role,
            $id,
            include_str!(concat!("../profiles/demo/", $path)),
        )
    };
}

/// The demo profiles: `(slicer, role, id, content)`.
const DEMO_PROFILES: &[(SlicerKind, ProfileRole, &str, &str)] = &[
    demo!(
        SlicerKind::BambuStudio,
        ProfileRole::Machine,
        "bambu-x2d",
        "bambu-studio/machine/bambu-x2d.json"
    ),
    demo!(
        SlicerKind::BambuStudio,
        ProfileRole::Process,
        "demo-0.20mm",
        "bambu-studio/process/demo-0.20mm.json"
    ),
    demo!(
        SlicerKind::BambuStudio,
        ProfileRole::Process,
        "demo-0.12mm",
        "bambu-studio/process/demo-0.12mm.json"
    ),
    demo!(
        SlicerKind::BambuStudio,
        ProfileRole::Filament,
        "pla-basic",
        "bambu-studio/filament/pla-basic.json"
    ),
    demo!(
        SlicerKind::BambuStudio,
        ProfileRole::Filament,
        "pla-generic",
        "bambu-studio/filament/pla-generic.json"
    ),
    demo!(
        SlicerKind::BambuStudio,
        ProfileRole::Filament,
        "petg-basic",
        "bambu-studio/filament/petg-basic.json"
    ),
    demo!(
        SlicerKind::BambuStudio,
        ProfileRole::Filament,
        "tpu-95a",
        "bambu-studio/filament/tpu-95a.json"
    ),
    demo!(
        SlicerKind::PrusaSlicer,
        ProfileRole::Machine,
        "prusa-mk4s",
        "prusa-slicer/machine/prusa-mk4s.ini"
    ),
    demo!(
        SlicerKind::PrusaSlicer,
        ProfileRole::Process,
        "demo-0.20mm",
        "prusa-slicer/process/demo-0.20mm.ini"
    ),
    demo!(
        SlicerKind::PrusaSlicer,
        ProfileRole::Filament,
        "pla-basic",
        "prusa-slicer/filament/pla-basic.ini"
    ),
    // The default material of the settings, so a fresh PrusaSlicer setup slices with it.
    demo!(
        SlicerKind::PrusaSlicer,
        ProfileRole::Filament,
        "pla-generic",
        "prusa-slicer/filament/pla-generic.ini"
    ),
];

/// Looks settings profiles up by id: first in the application's profile folder
/// (`<dir>/<slicer>/<machine|process|filament>/<id>.<json|ini>`), then among the demo
/// profiles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsStore {
    dir: Option<PathBuf>,
}

impl SettingsStore {
    /// A store with the demo profiles only.
    pub fn demo_only() -> Self {
        Self::default()
    }

    /// A store that prefers the files in `dir`.
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    /// The ids available for one slicer and role, installed ones first, without duplicates.
    pub fn list(&self, kind: SlicerKind, role: ProfileRole) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(dir) = &self.dir {
            let folder = dir.join(kind.as_str()).join(role.as_str());
            let mut found: Vec<String> = std::fs::read_dir(folder)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let matches = path
                        .extension()
                        .is_some_and(|ext| ext == settings_extension(kind));
                    let id = path.file_stem()?.to_str()?.to_owned();
                    (matches && valid_profile_id(&id)).then_some(id)
                })
                .collect();
            found.sort();
            ids.extend(found);
        }
        for (demo_kind, demo_role, id, _) in DEMO_PROFILES {
            if *demo_kind == kind && *demo_role == role && !ids.iter().any(|known| known == id) {
                ids.push((*id).to_owned());
            }
        }
        ids
    }

    /// Loads one profile. The id is checked before it is used in a path.
    pub fn load(
        &self,
        kind: SlicerKind,
        role: ProfileRole,
        id: &str,
    ) -> Result<SettingsProfile, SlicerError> {
        if !valid_profile_id(id) {
            return Err(SlicerError::InvalidJob(format!(
                "`{}` is not a valid {} profile id",
                crate::job::untrusted_text(id, 60),
                role.as_str()
            )));
        }
        let profile = |origin, content| SettingsProfile {
            id: id.to_owned(),
            role,
            origin,
            content,
        };
        if let Some(dir) = &self.dir {
            let path = dir
                .join(kind.as_str())
                .join(role.as_str())
                .join(format!("{id}.{}", settings_extension(kind)));
            match read_profile(&path) {
                Ok(Some(content)) => {
                    check_content(kind, id, &content)?;
                    return Ok(profile(ProfileOrigin::Installed(path), content));
                }
                Ok(None) => {}
                Err(reason) => {
                    return Err(SlicerError::ProfileUnusable {
                        id: id.to_owned(),
                        reason,
                    });
                }
            }
        }
        DEMO_PROFILES
            .iter()
            .find(|(demo_kind, demo_role, demo_id, _)| {
                *demo_kind == kind && *demo_role == role && *demo_id == id
            })
            .map(|(_, _, _, content)| profile(ProfileOrigin::Demo, content.as_bytes().to_vec()))
            .ok_or_else(|| SlicerError::ProfileNotFound {
                kind: kind.display_name(),
                role: role.as_str(),
                id: id.to_owned(),
                searched: match &self.dir {
                    Some(dir) => format!(
                        "{} and the built-in demo profiles",
                        dir.join(kind.as_str()).join(role.as_str()).display()
                    ),
                    None => "the built-in demo profiles (no profile folder is configured)".into(),
                },
            })
    }
}

/// Reads a profile file; `Ok(None)` when there is none.
fn read_profile(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} cannot be read: {e}", path.display())),
    };
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_PROFILE_BYTES {
        return Err(format!("{} is larger than 4 MB", path.display()));
    }
    let mut content = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_PROFILE_BYTES + 1).read_to_end(&mut content))
        .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    if content.len() as u64 > MAX_PROFILE_BYTES {
        return Err(format!("{} is larger than 4 MB", path.display()));
    }
    Ok(Some(content))
}

/// A settings file must at least have the right shape: a JSON object for Bambu Studio,
/// UTF-8 text for PrusaSlicer.
fn check_content(kind: SlicerKind, id: &str, content: &[u8]) -> Result<(), SlicerError> {
    let problem = match kind {
        SlicerKind::BambuStudio => match serde_json::from_slice::<serde_json::Value>(content) {
            Ok(serde_json::Value::Object(_)) => return Ok(()),
            Ok(_) => "it is not a JSON object".to_owned(),
            Err(e) => format!("it is not valid JSON: {e}"),
        },
        SlicerKind::PrusaSlicer => match std::str::from_utf8(content) {
            Ok(_) => return Ok(()),
            Err(_) => "it is not UTF-8 text".to_owned(),
        },
    };
    Err(SlicerError::ProfileUnusable {
        id: id.to_owned(),
        reason: problem,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn demo_profiles_are_well_formed_and_found_by_id() {
        let store = SettingsStore::demo_only();
        for (kind, role, id, content) in DEMO_PROFILES {
            check_content(*kind, id, content.as_bytes()).unwrap();
            let profile = store.load(*kind, *role, id).unwrap();
            assert_eq!(profile.origin, ProfileOrigin::Demo);
            assert_eq!(profile.content, content.as_bytes());
            // No demo profile sets G-code of any kind: the only settings with "gcode" in
            // their name are the output format switches.
            let keys: Vec<String> = match kind {
                SlicerKind::BambuStudio => serde_json::from_str::<serde_json::Value>(content)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect(),
                SlicerKind::PrusaSlicer => content
                    .lines()
                    .filter_map(|line| line.split_once('='))
                    .map(|(key, _)| key.trim().to_owned())
                    .collect(),
            };
            for key in keys {
                let harmless = matches!(key.as_str(), "binary_gcode" | "gcode_flavor");
                assert!(
                    harmless || !key.to_ascii_lowercase().contains("gcode"),
                    "{id}: {key}"
                );
            }
        }
        assert_eq!(
            store.list(SlicerKind::BambuStudio, ProfileRole::Process),
            ["demo-0.20mm", "demo-0.12mm"]
        );
        let error = store
            .load(SlicerKind::BambuStudio, ProfileRole::Process, "nope")
            .unwrap_err();
        assert_eq!(error.code(), "profile_not_found");
    }

    #[test]
    fn installed_profiles_win_and_ids_cannot_escape_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("bambu-studio").join("process");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("demo-0.20mm.json"),
            br#"{"layer_height": "0.28"}"#,
        )
        .unwrap();
        std::fs::write(folder.join("tuned.json"), br#"{"layer_height": "0.16"}"#).unwrap();
        std::fs::write(folder.join("broken.json"), b"[1, 2").unwrap();
        std::fs::write(dir.path().join("secret.json"), b"{}").unwrap();

        let store = SettingsStore::with_dir(dir.path());
        let kind = SlicerKind::BambuStudio;
        let shadowed = store
            .load(kind, ProfileRole::Process, "demo-0.20mm")
            .unwrap();
        assert!(matches!(shadowed.origin, ProfileOrigin::Installed(_)));
        assert_eq!(shadowed.content, br#"{"layer_height": "0.28"}"#);
        assert!(store.load(kind, ProfileRole::Process, "tuned").is_ok());
        assert_eq!(
            store.list(kind, ProfileRole::Process),
            ["broken", "demo-0.20mm", "tuned", "demo-0.12mm"]
        );
        let broken = store
            .load(kind, ProfileRole::Process, "broken")
            .unwrap_err();
        assert_eq!(broken.code(), "profile_unusable");
        // Falls back to the demo profile of another role.
        assert!(store.load(kind, ProfileRole::Filament, "pla-basic").is_ok());
        for id in ["../../secret", "..", "/etc/passwd", "a/b", ""] {
            let error = store.load(kind, ProfileRole::Process, id).unwrap_err();
            assert_eq!(error.code(), "invalid_job", "{id:?}");
        }
    }
}
