//! Links that leave the window.
//!
//! Guarantee: the system browser is opened only for an address in the fixed table below.
//! The window names a link by id and never supplies an address; every address of the table
//! must also pass [`allowed`] (https, a host of the allowlist, no credentials, the default
//! port), which is the last check before anything is opened.

use hotend_core::ApiError;
use url::Url;

use crate::wire::LinkId;

/// The hosts a link may point to: the key pages of the model providers and Ollama's
/// download page.
pub const ALLOWED_HOSTS: [&str; 5] = [
    "console.anthropic.com",
    "platform.openai.com",
    "aistudio.google.com",
    "openrouter.ai",
    "ollama.com",
];

/// Every link the window can ask for.
pub const LINKS: [(LinkId, &str); 5] = [
    (
        LinkId::AnthropicKeys,
        "https://console.anthropic.com/settings/keys",
    ),
    (LinkId::OpenaiKeys, "https://platform.openai.com/api-keys"),
    (LinkId::GoogleKeys, "https://aistudio.google.com/apikey"),
    (
        LinkId::OpenrouterKeys,
        "https://openrouter.ai/settings/keys",
    ),
    (LinkId::OllamaDownload, "https://ollama.com/download"),
];

fn refused(reason: &str) -> ApiError {
    ApiError::InvalidInput {
        message: format!("Hotend does not open this address: {reason}"),
        next_step: "Open it in your browser yourself if you trust it.".to_owned(),
    }
}

/// The address, if Hotend may open it in the system browser.
pub fn allowed(address: &str) -> Result<Url, ApiError> {
    let url = Url::parse(address).map_err(|_| refused("it is not a web address"))?;
    if url.scheme() != "https" {
        return Err(refused("only https addresses are opened"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refused("it carries a user name or a password"));
    }
    if url.port().is_some() {
        return Err(refused("it names a port"));
    }
    match url.host_str() {
        Some(host) if ALLOWED_HOSTS.contains(&host) => Ok(url),
        _ => Err(refused("its host is not one Hotend links to")),
    }
}

/// The address of a link id, checked against the allowlist.
pub fn resolve(link: LinkId) -> Result<Url, ApiError> {
    let address = LINKS
        .iter()
        .find(|(id, _)| *id == link)
        .map(|(_, address)| *address)
        .ok_or_else(|| refused("it is not in Hotend's list of links"))?;
    allowed(address)
}

/// Something that can show an address to the person in their own browser.
pub trait LinkOpener: Send + Sync {
    fn open(&self, url: &Url) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn every_link_of_the_table_is_allowed() {
        for (id, address) in LINKS {
            assert_eq!(resolve(id).unwrap().as_str(), address, "{id:?}");
        }
    }

    #[test]
    fn nothing_but_https_to_a_listed_host_is_opened() {
        for address in [
            "http://console.anthropic.com/settings/keys",
            "https://console.anthropic.com.evil.example/keys",
            "https://evil.example/?next=https://openrouter.ai",
            "https://user:pw@openrouter.ai/keys",
            "https://user@openrouter.ai/keys",
            "https://openrouter.ai:8443/keys",
            "https://127.0.0.1/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "bambu-connect://import-file?path=/tmp/x",
            "openrouter.ai/keys",
            "",
        ] {
            assert!(allowed(address).is_err(), "{address}");
        }
        assert!(allowed("https://openrouter.ai/settings/keys").is_ok());
        // The parser normalises the host before it is compared.
        assert!(allowed("https://OPENROUTER.AI/keys").is_ok());
        assert!(allowed("https://openrouter.ai./keys").is_err());
    }
}
