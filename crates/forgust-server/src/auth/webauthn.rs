//! WebAuthn: security keys (a second factor) and passkeys (a sign-in of their
//! own). The relying party is exactly the host of `server.external_url`, and
//! the only origin accepted is that URL's: no subdomains, no other port.
//! Browsers allow WebAuthn over https, and over http on `localhost` only.

use serde::Serialize;
use serde::de::DeserializeOwned;
use webauthn_rs::prelude::{CreationChallengeResponse, Url, Webauthn, WebauthnBuilder};
use webauthn_rs_proto::ResidentKeyRequirement;

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// The instance's relying party: the library's, and the id credentials made
/// now are bound to.
pub struct RelyingParty {
    pub webauthn: Webauthn,
    pub id: String,
}

impl std::ops::Deref for RelyingParty {
    type Target = Webauthn;
    fn deref(&self) -> &Webauthn {
        &self.webauthn
    }
}

/// The relying party for this instance, or why there is none.
pub fn build(cfg: &Config) -> Result<RelyingParty, String> {
    let url = Url::parse(&cfg.server.external_url).map_err(|e| format!("server.external_url is not a URL: {e}"))?;
    let Some(host) = url.domain().map(str::to_ascii_lowercase) else {
        return Err(format!("server.external_url ({url}) names an IP address: a domain name is needed"));
    };
    if url.scheme() != "https" && host != "localhost" {
        return Err(format!("server.external_url ({url}) is not https"));
    }
    let origin = Url::parse(&url.origin().ascii_serialization()).map_err(|e| e.to_string())?;
    let webauthn = WebauthnBuilder::new(&host, &origin)
        .and_then(|b| b.rp_name("Forgust").build())
        .map_err(|e| format!("WebAuthn relying party {host}: {e}"))?;
    Ok(RelyingParty { webauthn, id: host })
}

/// The instance's relying party, or a clear refusal when it has none.
pub fn of(state: &AppState) -> ApiResult<&RelyingParty> {
    state
        .webauthn
        .as_deref()
        .ok_or_else(|| ApiError::conflict("security keys and passkeys need Forgust served over https (server.external_url)"))
}

/// A WebAuthn or (de)serialisation failure that is ours, not the user's.
pub fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(anyhow::anyhow!("webauthn: {e}"))
}

/// Ceremony state and credentials, as stored.
pub fn to_json(v: &impl Serialize) -> ApiResult<serde_json::Value> {
    serde_json::to_value(v).map_err(internal)
}

pub fn from_json<T: DeserializeOwned>(v: serde_json::Value) -> ApiResult<T> {
    serde_json::from_value(v).map_err(internal)
}

/// A passkey must be one the authenticator keeps (usernameless sign-in offers
/// only those); webauthn-rs leaves that to the authenticator.
pub fn require_resident_key(options: &mut CreationChallengeResponse) {
    if let Some(sel) = options.public_key.authenticator_selection.as_mut() {
        sel.require_resident_key = true;
        sel.resident_key = Some(ResidentKeyRequirement::Required);
    }
}

/// A security key is asked for credProtect "user verification required"
/// while it need not keep the credential nor verify its user: Chrome refuses
/// the whole request (NotSupportedError). A second factor needs the key's
/// presence only; whether it verified its user then is kept, and asked again.
pub fn without_cred_protect(options: &mut CreationChallengeResponse) {
    if let Some(ext) = options.public_key.extensions.as_mut() {
        ext.cred_protect = None;
    }
}

/// After a successful ceremony: when, and the credential (counter, backup
/// flags) when the answer moved it.
pub async fn record_use(c: &mut sqlx::PgConnection, id: i64, credential: &impl Serialize, moved: Option<bool>) -> ApiResult<()> {
    let credential = if moved.unwrap_or(false) { Some(to_json(credential)?) } else { None };
    forgust_db::webauthn::used(c, id, credential.as_ref()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The relying party id, or why there is none.
    fn with(url: &str) -> Result<String, String> {
        let mut cfg = Config::default();
        cfg.server.external_url = url.into();
        build(&cfg).map(|rp| rp.id)
    }

    #[test]
    fn a_domain_over_https_or_localhost() {
        assert_eq!(with("https://Git.Example.com").as_deref(), Ok("git.example.com"));
        assert!(with("https://git.example.com:8443").is_ok());
        assert!(with("http://localhost:8080").is_ok());
        assert!(with("http://git.example.com").unwrap_err().contains("https"));
        assert!(with("http://127.0.0.1:8080").unwrap_err().contains("IP address"));
        assert!(with("https://[::1]").unwrap_err().contains("IP address"));
    }
}
