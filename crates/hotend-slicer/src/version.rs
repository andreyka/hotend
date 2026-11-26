//! Slicer kinds, versions and the pinned-version policy.
//!
//! The policy is data (`profiles/slicer-versions.toml`): for each slicer kind an ordered list
//! of version ranges with a verdict. Nothing here runs a program.

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::SlicerError;

const POLICY_TOML: &str = include_str!("../profiles/slicer-versions.toml");
const POLICY_FILE: &str = "profiles/slicer-versions.toml";

/// Which vendor command line an executable speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlicerKind {
    BambuStudio,
    PrusaSlicer,
}

impl SlicerKind {
    pub const ALL: [Self; 2] = [Self::BambuStudio, Self::PrusaSlicer];

    /// The id used in data files and folder names.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BambuStudio => "bambu-studio",
            Self::PrusaSlicer => "prusa-slicer",
        }
    }

    /// The vendor's product name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::BambuStudio => "Bambu Studio",
            Self::PrusaSlicer => "PrusaSlicer",
        }
    }

    /// Environment variable that names the executable explicitly.
    pub fn override_env(self) -> &'static str {
        match self {
            Self::BambuStudio => "HOTEND_BAMBU_STUDIO",
            Self::PrusaSlicer => "HOTEND_PRUSA_SLICER",
        }
    }
}

impl fmt::Display for SlicerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A slicer version: dot-separated numbers, optionally followed by a pre-release tag
/// (`-alpha1`) or build metadata (`+UNKNOWN`). A pre-release sorts before its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicerVersion {
    numbers: Vec<u64>,
    pre_release: bool,
    text: String,
}

impl SlicerVersion {
    /// Parses a version at the start of `text`; `None` when it does not start with a number.
    /// At most six components of at most nine digits are accepted.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let core_len = text
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(text.len());
        let (core, rest) = text.split_at(core_len);
        let core = core.trim_end_matches('.');
        if core.is_empty() {
            return None;
        }
        let mut numbers = Vec::new();
        for part in core.split('.') {
            if part.is_empty() || part.len() > 9 || numbers.len() == 6 {
                return None;
            }
            numbers.push(part.parse().ok()?);
        }
        let tag_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.' | '_')))
            .unwrap_or(rest.len())
            .min(40);
        let tag = &rest[..tag_len];
        Some(Self {
            numbers,
            pre_release: tag.starts_with('-'),
            text: format!("{core}{tag}"),
        })
    }

    /// The version as the program printed it.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for SlicerVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl SlicerVersion {
    /// Compares the numbers only, so `3.0.0-alpha1` equals `3.0.0`.
    fn cmp_numbers(&self, other: &Self) -> Ordering {
        let len = self.numbers.len().max(other.numbers.len());
        for i in 0..len {
            let a = self.numbers.get(i).copied().unwrap_or(0);
            let b = other.numbers.get(i).copied().unwrap_or(0);
            if a != b {
                return a.cmp(&b);
            }
        }
        Ordering::Equal
    }
}

impl Ord for SlicerVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        // `true` (a pre-release) sorts first.
        self.cmp_numbers(other)
            .then(other.pre_release.cmp(&self.pre_release))
    }
}

impl PartialOrd for SlicerVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What the policy says about one installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run it.
    Allow,
    /// Run it and attach this reason to every result as a warning.
    Warn(String),
    /// Do not run it.
    Refuse(String),
}

impl Verdict {
    pub fn is_refused(&self) -> bool {
        matches!(self, Self::Refuse(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VerdictKind {
    Allow,
    Warn,
    Refuse,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    schema: u32,
    revision: String,
    slicer: Vec<SlicerPolicyFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlicerPolicyFile {
    kind: SlicerKind,
    unknown: VerdictKind,
    #[serde(default)]
    rule: Vec<RuleFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    from: Option<String>,
    below: Option<String>,
    verdict: VerdictKind,
    reason: Option<String>,
}

#[derive(Debug, Clone)]
struct Rule {
    from: Option<SlicerVersion>,
    below: Option<SlicerVersion>,
    verdict: VerdictKind,
    reason: String,
}

#[derive(Debug, Clone)]
struct KindPolicy {
    kind: SlicerKind,
    unknown: VerdictKind,
    rules: Vec<Rule>,
}

/// The pinned-version table: which versions of each slicer are allowed, allowed with a
/// warning, or refused. A version no rule matches is refused.
#[derive(Debug, Clone)]
pub struct VersionPolicy {
    revision: String,
    kinds: Vec<KindPolicy>,
}

impl VersionPolicy {
    /// The policy compiled into this build.
    pub fn builtin() -> Result<Self, SlicerError> {
        Self::from_toml(POLICY_TOML).map_err(|message| SlicerError::Data {
            file: POLICY_FILE,
            message,
        })
    }

    /// Parses a policy in the format of `profiles/slicer-versions.toml`.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let file: PolicyFile = toml::from_str(text).map_err(|e| e.to_string())?;
        if file.schema != 1 {
            return Err(format!("unsupported schema {}", file.schema));
        }
        let mut kinds = Vec::new();
        for slicer in file.slicer {
            let mut rules = Vec::new();
            for rule in slicer.rule {
                let bound = |text: Option<String>| match text {
                    None => Ok(None),
                    Some(text) => SlicerVersion::parse(&text)
                        .map(Some)
                        .ok_or_else(|| format!("`{text}` is not a version")),
                };
                let reason = match (rule.verdict, rule.reason) {
                    (_, Some(reason)) => reason,
                    (VerdictKind::Allow, None) => String::new(),
                    (_, None) => return Err("a warn or refuse rule needs a reason".into()),
                };
                rules.push(Rule {
                    from: bound(rule.from)?,
                    below: bound(rule.below)?,
                    verdict: rule.verdict,
                    reason,
                });
            }
            if kinds.iter().any(|k: &KindPolicy| k.kind == slicer.kind) {
                return Err(format!("{} is listed twice", slicer.kind));
            }
            kinds.push(KindPolicy {
                kind: slicer.kind,
                unknown: slicer.unknown,
                rules,
            });
        }
        Ok(Self {
            revision: file.revision,
            kinds,
        })
    }

    /// Revision string of the policy data.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The verdict for one slicer kind and the version read from it (`None` when the version
    /// could not be read).
    pub fn verdict(&self, kind: SlicerKind, version: Option<&SlicerVersion>) -> Verdict {
        let name = kind.display_name();
        let Some(policy) = self.kinds.iter().find(|k| k.kind == kind) else {
            return Verdict::Refuse(format!("the version policy has no entry for {name}"));
        };
        let Some(version) = version else {
            let reason = format!("the version of this {name} could not be read");
            return make_verdict(policy.unknown, reason);
        };
        for rule in &policy.rules {
            // A pre-release of the upper bound is not "below" it: 3.0.0-alpha1 must not fall
            // into a range that ends before 3.0.0. It is not "from" its release either.
            let above = rule.from.as_ref().is_none_or(|from| version >= from);
            let under = rule
                .below
                .as_ref()
                .is_none_or(|below| version.cmp_numbers(below) == Ordering::Less);
            if above && under {
                return make_verdict(rule.verdict, rule.reason.clone());
            }
        }
        Verdict::Refuse(format!(
            "{name} {version} is not in the list of supported versions"
        ))
    }
}

fn make_verdict(kind: VerdictKind, reason: String) -> Verdict {
    match kind {
        VerdictKind::Allow => Verdict::Allow,
        VerdictKind::Warn => Verdict::Warn(reason),
        VerdictKind::Refuse => Verdict::Refuse(reason),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn v(text: &str) -> SlicerVersion {
        SlicerVersion::parse(text).unwrap()
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(v("2.7.2+UNKNOWN").as_str(), "2.7.2+UNKNOWN");
        assert_eq!(v("02.05.03.00 extra words").as_str(), "02.05.03.00");
        assert!(v("2.7.2+UNKNOWN") < v("2.9.7"));
        assert!(v("2.9.7") == v("2.9.7"));
        assert_eq!(v("2.9.7").cmp(&v("2.9.7.0")), Ordering::Equal);
        assert!(v("2.10.0") > v("2.9.7"));
        assert!(v("2.9.7-rc1") < v("2.9.7"));
        assert!(v("2.9.7-rc1") > v("2.9.6"));
        assert!(v("02.05.03.00") > v("2.5.2.99"));
        assert!(v("3.0.0-alpha14") < v("3.0.0"));
        for bad in [
            "",
            "abc",
            ".1",
            "1..2",
            "1234567890.1",
            "1.2.3.4.5.6.7",
            "-1",
        ] {
            assert!(
                SlicerVersion::parse(bad).is_none(),
                "{bad:?} must not parse"
            );
        }
    }

    #[test]
    fn builtin_policy_refuses_old_and_alpha_prusaslicer() {
        let policy = VersionPolicy::builtin().unwrap();
        let prusa = |text: &str| policy.verdict(SlicerKind::PrusaSlicer, Some(&v(text)));
        assert!(prusa("2.7.2+UNKNOWN").is_refused());
        assert!(prusa("2.9.6").is_refused());
        assert!(prusa("2.9.7-rc1").is_refused());
        assert_eq!(prusa("2.9.7"), Verdict::Allow);
        assert_eq!(prusa("2.9.9"), Verdict::Allow);
        assert!(prusa("3.0.0").is_refused());
        // A 3.0 pre-release sorts below 3.0.0 but is still the rewrite; it must not slip
        // into the 2.9 range.
        assert!(prusa("3.0.0-alpha14").is_refused());
        assert!(policy.verdict(SlicerKind::PrusaSlicer, None).is_refused());
    }

    #[test]
    fn builtin_policy_names_the_measured_bambu_studio_and_warns_for_every_version() {
        let policy = VersionPolicy::builtin().unwrap();
        let reason = |text: &str| match policy.verdict(SlicerKind::BambuStudio, Some(&v(text))) {
            Verdict::Warn(reason) => reason,
            other => panic!("{text}: {other:?}"),
        };
        // The measured release, as the sliced file states it and without leading zeros.
        for measured in ["02.09.01.70", "2.9.1.70"] {
            assert!(
                reason(measured).contains("was measured against, but"),
                "{measured}"
            );
        }
        // Its neighbours and everything else are named as not measured.
        for other in ["02.09.01.69", "02.09.01.71", "02.05.03.00", "1.0", "3.0.0"] {
            assert!(
                reason(other).contains("is not the Bambu Studio version"),
                "{other}"
            );
        }
        // The Windows program does not state its version before a slice.
        assert!(matches!(
            policy.verdict(SlicerKind::BambuStudio, None),
            Verdict::Warn(_)
        ));
    }

    #[test]
    fn first_matching_rule_wins_and_unmatched_versions_are_refused() {
        let policy = VersionPolicy::from_toml(
            r#"
            schema = 1
            revision = "t"
            [[slicer]]
            kind = "prusa-slicer"
            unknown = "warn"
            [[slicer.rule]]
            from = "2.7.0"
            below = "2.8.0"
            verdict = "warn"
            reason = "test range"
            [[slicer.rule]]
            from = "2.7.2"
            verdict = "allow"
            "#,
        )
        .unwrap();
        let verdict = |text: &str| policy.verdict(SlicerKind::PrusaSlicer, Some(&v(text)));
        assert_eq!(verdict("2.7.2"), Verdict::Warn("test range".into()));
        assert_eq!(verdict("2.8.0"), Verdict::Allow);
        assert!(verdict("2.6.9").is_refused());
        assert!(matches!(
            policy.verdict(SlicerKind::PrusaSlicer, None),
            Verdict::Warn(_)
        ));
        assert!(
            policy
                .verdict(SlicerKind::BambuStudio, Some(&v("2.0")))
                .is_refused()
        );
    }

    #[test]
    fn malformed_policies_are_rejected() {
        for text in [
            "schema = 2\nrevision = \"x\"\nslicer = []",
            "schema = 1\nrevision = \"x\"\n[[slicer]]\nkind = \"cura\"\nunknown = \"allow\"",
            "schema = 1\nrevision = \"x\"\n[[slicer]]\nkind = \"prusa-slicer\"\nunknown = \"refuse\"\n[[slicer.rule]]\nverdict = \"refuse\"",
            "schema = 1\nrevision = \"x\"\n[[slicer]]\nkind = \"prusa-slicer\"\nunknown = \"refuse\"\n[[slicer.rule]]\nfrom = \"new\"\nverdict = \"allow\"",
        ] {
            assert!(VersionPolicy::from_toml(text).is_err(), "{text}");
        }
    }
}
