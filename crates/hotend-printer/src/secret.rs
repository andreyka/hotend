//! The printer's LAN access code, as a secret.
//!
//! Guarantees:
//! - an [`AccessCode`] cannot be printed or serialised: it has no `Display`, `Serialize`,
//!   `Deserialize` or `Clone`, and its `Debug` output is a fixed placeholder;
//! - the code is wiped from memory when the value is dropped;
//! - only this crate's LAN transport can read it (`expose` is crate-private).
//!
//! The application reads the code from the OS keychain and hands it to the transport; it
//! never needs the text back.
//!
//! ```compile_fail
//! let code = hotend_printer::AccessCode::new("12345678".to_owned()).unwrap();
//! println!("{code}"); // no Display
//! ```
//!
//! ```compile_fail
//! let code = hotend_printer::AccessCode::new("12345678".to_owned()).unwrap();
//! let _ = serde_json::to_string(&code); // no Serialize
//! ```
//!
//! ```compile_fail
//! let code = hotend_printer::AccessCode::new("12345678".to_owned()).unwrap();
//! let _copy = code.clone(); // no Clone
//! ```

use std::fmt;
use std::process::Command;

use zeroize::Zeroizing;

/// The environment variable the application reads the access code from first. It is one of
/// [`SECRET_ENV`].
pub const ACCESS_CODE_ENV: &str = "BAMBU_LAN_ACCESS_CODE";

/// The environment variables that may hold a secret Hotend reads (the access code, the keys
/// of the AI providers and search services) or name the file that does. A program Hotend
/// starts outside the sandbox for a link or a file gets none of them
/// ([`without_secret_env`]); the sandbox starts its children with an empty environment.
/// `hotend-core` has a test that holds this list to the variables it reads.
pub const SECRET_ENV: [&str; 9] = [
    ACCESS_CODE_ENV,
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
    "KEENABLE_API_KEY",
    "TAVILY_API_KEY",
    "HOTEND_SECRETS_FILE",
];

/// Removes every variable of [`SECRET_ENV`] from the environment `command` will start with.
pub fn without_secret_env(command: &mut Command) -> &mut Command {
    for name in SECRET_ENV {
        command.env_remove(name);
    }
    command
}

/// The access code shown on the printer's screen. It is the password of every LAN channel.
pub struct AccessCode(Zeroizing<String>);

/// Why text is not an access code. Never contains the text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("an access code is 6 to 32 letters and digits, as shown on the printer's screen")]
pub struct InvalidAccessCode;

impl AccessCode {
    /// Takes ownership of the code. The text is wiped if it is refused.
    pub fn new(code: String) -> Result<Self, InvalidAccessCode> {
        let code = Zeroizing::new(code);
        if !(6..=32).contains(&code.len()) || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(InvalidAccessCode);
        }
        Ok(Self(code))
    }

    /// The code itself, for the transport's sign-in only.
    #[cfg_attr(not(feature = "lan-status"), allow(dead_code))]
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessCode(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_code_never_appears_in_debug_output_or_errors() {
        let code = AccessCode::new("s3cr3tc0".to_owned()).unwrap();
        assert_eq!(format!("{code:?}"), "AccessCode(<redacted>)");
        assert_eq!(
            format!("{:#?}", Some(&code)),
            "Some(\n    AccessCode(<redacted>),\n)"
        );
        assert_eq!(code.expose(), "s3cr3tc0");
        let error = AccessCode::new("not a code!".to_owned()).unwrap_err();
        assert!(!error.to_string().contains("not a code"));
        assert!(AccessCode::new("12345".to_owned()).is_err());
    }
}
