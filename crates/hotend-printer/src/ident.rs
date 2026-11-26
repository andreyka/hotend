//! Who a printer is: its serial number and model.
//!
//! A [`Serial`] is validated text (ASCII letters and digits, upper case), so it can be put
//! into an MQTT topic, compared with a certificate's common name and shown to the user
//! without further checks.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A printer's serial number, as printed on the printer's screen.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Serial(String);

/// Why text is not a serial number.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a serial number is 8 to 32 letters and digits")]
pub struct InvalidSerial;

impl Serial {
    /// Checks and normalises `text` (surrounding spaces dropped, upper case).
    pub fn new(text: &str) -> Result<Self, InvalidSerial> {
        let text = text.trim();
        if !(8..=32).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(InvalidSerial);
        }
        Ok(Self(text.to_ascii_uppercase()))
    }

    /// The serial as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Serial {
    type Error = InvalidSerial;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(&text)
    }
}

impl From<Serial> for String {
    fn from(serial: Serial) -> Self {
        serial.0
    }
}

impl fmt::Display for Serial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Serial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Serial({})", self.0)
    }
}

/// One printer: its serial, the profile id of its model and the name the user gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterIdentity {
    /// The serial number the user confirmed.
    pub serial: Serial,
    /// Id of the model's profile, for example `bambu-x2d`.
    pub model: String,
    /// Display name.
    pub name: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn serials_are_validated_and_normalised() {
        assert_eq!(
            Serial::new(" 20p0ab123456789 ").unwrap().as_str(),
            "20P0AB123456789"
        );
        for bad in [
            "",
            "short",
            "20P0/../x1234",
            "20P0 AB12345",
            "ÄÖÜ12345678",
            &"A".repeat(33),
        ] {
            assert_eq!(Serial::new(bad), Err(InvalidSerial), "{bad}");
        }
        let parsed: Result<Serial, _> = serde_json::from_str("\"device/#\"");
        assert!(parsed.is_err());
        let parsed: Serial = serde_json::from_str("\"20p0ab123456789\"").unwrap();
        assert_eq!(parsed.to_string(), "20P0AB123456789");
    }
}
