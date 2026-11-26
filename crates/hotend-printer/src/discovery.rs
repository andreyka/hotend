//! Printer discovery: parsing only.
//!
//! Bambu printers announce themselves on the local network with SSDP-style datagrams on
//! UDP 2021 (Bambu Lab's list of network ports; not confirmed on an X2D). This module turns
//! one datagram into a [`Announcement`]. It opens no socket: listening is the transport's job,
//! and nothing in a default build listens.
//!
//! An announcement is a claim by whoever sent the datagram. It is used to offer the user a
//! printer to pair with; the serial is trusted only after the user has confirmed it against
//! the printer's screen and the certificate presented at pairing names it.
//!
//! Header names follow the announcements quoted in ha-bambulab and in the community notes:
//! `NT`/`ST` (service type), `USN` (serial), `Location` (address), `DevModel.bambu.com`,
//! `DevName.bambu.com`, `DevConnect.bambu.com`, `DevBind.bambu.com`.

use std::net::IpAddr;

use serde::Serialize;

use crate::ident::Serial;
use crate::profile::PrinterProfile;
use crate::protocol::report::clean_text;

/// Largest datagram that is parsed.
pub const MAX_DATAGRAM_BYTES: usize = 4096;

/// A printer's announcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Announcement {
    /// The serial the sender claims.
    pub serial: Serial,
    /// The address the sender claims.
    pub address: IpAddr,
    /// Model code, for example `N6`.
    pub model_code: Option<String>,
    /// The name the user gave the printer.
    pub name: Option<String>,
    /// `lan` or `cloud`.
    pub connect_mode: Option<String>,
    /// `free` or `occupied` (bound to a Bambu account).
    pub bind_state: Option<String>,
}

/// Why a datagram is not an announcement of a printer of this family.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// Larger than [`MAX_DATAGRAM_BYTES`].
    #[error("the datagram is too large")]
    TooLarge,
    /// Not an SSDP `NOTIFY` or search response.
    #[error("not a discovery announcement")]
    NotAnAnnouncement,
    /// Announces another kind of device.
    #[error("the announcement is for another kind of device")]
    OtherService,
    /// A required field is missing or malformed.
    #[error("the announcement has no usable {0}")]
    Missing(&'static str),
}

/// Parses one datagram received on the discovery port.
pub fn parse_announcement(
    datagram: &[u8],
    profile: &PrinterProfile,
) -> Result<Announcement, DiscoveryError> {
    if datagram.len() > MAX_DATAGRAM_BYTES {
        return Err(DiscoveryError::TooLarge);
    }
    let text = std::str::from_utf8(datagram).map_err(|_| DiscoveryError::NotAnAnnouncement)?;
    let mut lines = text.split("\r\n").flat_map(|line| line.split('\n'));
    let first = lines.next().unwrap_or_default().trim();
    if !(first.starts_with("NOTIFY * HTTP/1.1") || first.starts_with("HTTP/1.1 200")) {
        return Err(DiscoveryError::NotAnAnnouncement);
    }
    let mut service = None;
    let mut serial = None;
    let mut address = None;
    let mut announcement_fields: [Option<String>; 4] = [None, None, None, None];
    for line in lines.take(64) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = clean_text(value.trim());
        match key.trim().to_ascii_lowercase().as_str() {
            "nt" | "st" => service = Some(value),
            "usn" => serial = Some(value),
            "location" => address = Some(value),
            "devmodel.bambu.com" => announcement_fields[0] = Some(value),
            "devname.bambu.com" => announcement_fields[1] = Some(value),
            "devconnect.bambu.com" => announcement_fields[2] = Some(value),
            "devbind.bambu.com" => announcement_fields[3] = Some(value),
            _ => {}
        }
    }
    if service.as_deref() != Some(profile.family().discovery.service.as_str()) {
        return Err(DiscoveryError::OtherService);
    }
    let serial = serial
        .and_then(|text| Serial::new(&text).ok())
        .ok_or(DiscoveryError::Missing("serial number"))?;
    let address = address
        .and_then(|text| text.parse::<IpAddr>().ok())
        .ok_or(DiscoveryError::Missing("address"))?;
    let [model_code, name, connect_mode, bind_state] = announcement_fields;
    Ok(Announcement {
        serial,
        address,
        model_code: model_code.filter(|text| !text.is_empty()),
        name: name.filter(|text| !text.is_empty()),
        connect_mode: connect_mode.filter(|text| !text.is_empty()),
        bind_state: bind_state.filter(|text| !text.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::profile::Profiles;

    fn x2d() -> PrinterProfile {
        Profiles::builtin().unwrap().printer("bambu-x2d").unwrap()
    }

    const NOTIFY: &str = "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nServer: UPnP/1.0\r\n\
        Location: 192.168.1.50\r\nNT: urn:bambulab-com:device:3dprinter:1\r\n\
        USN: 20P0AB123456789\r\nCache-Control: max-age=1800\r\nDevModel.bambu.com: N6\r\n\
        DevName.bambu.com: Workshop X2D\r\nDevSignal.bambu.com: -45\r\n\
        DevConnect.bambu.com: lan\r\nDevBind.bambu.com: free\r\n\r\n";

    #[test]
    fn an_announcement_is_read() {
        let found = parse_announcement(NOTIFY.as_bytes(), &x2d()).unwrap();
        assert_eq!(found.serial.as_str(), "20P0AB123456789");
        assert_eq!(found.address.to_string(), "192.168.1.50");
        assert_eq!(found.model_code.as_deref(), Some("N6"));
        assert_eq!(found.name.as_deref(), Some("Workshop X2D"));
        assert_eq!(found.connect_mode.as_deref(), Some("lan"));
        assert_eq!(found.bind_state.as_deref(), Some("free"));
        let profiles = Profiles::builtin().unwrap();
        let model = profiles.printer_for_model_code(found.model_code.as_deref().unwrap());
        assert_eq!(model.unwrap().id(), "bambu-x2d");
    }

    #[test]
    fn hostile_or_foreign_datagrams_are_refused() {
        let profile = x2d();
        let parse = |text: &str| parse_announcement(text.as_bytes(), &profile);
        assert_eq!(
            parse(&NOTIFY.replace("3dprinter", "mediaserver")),
            Err(DiscoveryError::OtherService)
        );
        assert_eq!(
            parse(&NOTIFY.replace("USN: 20P0AB123456789", "USN: ../../etc")),
            Err(DiscoveryError::Missing("serial number"))
        );
        assert_eq!(
            parse(&NOTIFY.replace("192.168.1.50", "printer.evil.example")),
            Err(DiscoveryError::Missing("address"))
        );
        assert_eq!(
            parse("GET / HTTP/1.1\r\n\r\n"),
            Err(DiscoveryError::NotAnAnnouncement)
        );
        assert_eq!(parse(""), Err(DiscoveryError::NotAnAnnouncement));
        assert_eq!(
            parse_announcement(&[0xFF, 0xFE, 0x00], &profile),
            Err(DiscoveryError::NotAnAnnouncement)
        );
        assert_eq!(
            parse_announcement(&vec![b'A'; MAX_DATAGRAM_BYTES + 1], &profile),
            Err(DiscoveryError::TooLarge)
        );
        // Control characters in a name do not reach the interface.
        let named = NOTIFY.replace("Workshop X2D", "Work\u{1b}[31mshop");
        assert_eq!(parse(&named).unwrap().name.as_deref(), Some("Work[31mshop"));
    }
}
