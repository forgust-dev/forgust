//! OpenID Connect login (authorization code + PKCE).

use std::collections::BTreeMap;

use axum_extra::extract::cookie::{Cookie, SameSite};
use chrono::{Duration, Utc};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClaimsVerificationError, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};

use crate::config::OidcProvider;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Clone)]
pub struct OidcClients {
    pub providers: BTreeMap<String, OidcProvider>,
    /// Each provider's discovery document and keys, kept for an hour (and
    /// fetched again when an ID token does not verify: the keys may have rotated).
    metadata: moka::future::Cache<String, CoreProviderMetadata>,
    /// Talks to the providers (never follows redirects).
    http: openidconnect::reqwest::Client,
}

impl OidcClients {
    pub fn new(providers: BTreeMap<String, OidcProvider>) -> anyhow::Result<Self> {
        let metadata = moka::future::Cache::builder().max_capacity(64).time_to_live(std::time::Duration::from_secs(3600)).build();
        // after `tls::init`: the providers may be behind `security.ca_file`
        let mut http = openidconnect::reqwest::ClientBuilder::new().redirect(openidconnect::reqwest::redirect::Policy::none());
        for cert in crate::tls::certs() {
            http = http.add_root_certificate(openidconnect::reqwest::Certificate::from_der(cert)?);
        }
        let http = http.build()?;
        Ok(OidcClients { providers, metadata, http })
    }

    async fn discover(&self, name: &str, p: &OidcProvider) -> ApiResult<CoreProviderMetadata> {
        if let Some(meta) = self.metadata.get(name).await {
            return Ok(meta);
        }
        let issuer = IssuerUrl::new(p.issuer.clone()).map_err(|e| ApiError::Internal(e.into()))?;
        let meta = CoreProviderMetadata::discover_async(issuer, &self.http)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("OIDC discovery: {e}")))?;
        self.metadata.insert(name.to_string(), meta.clone()).await;
        Ok(meta)
    }
}

/// What the provider told us about the person.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct OidcIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub username: Option<String>,
    pub name: Option<String>,
    /// The provider's groups claim (`groups_claim`), as the ID token carried it.
    pub groups: Vec<String>,
}

impl OidcProvider {
    /// With `allowed_groups` set, only their members come in through this provider.
    pub fn admits(&self, identity: &OidcIdentity) -> ApiResult<()> {
        if self.allowed_groups.is_empty() || identity.groups.iter().any(|g| self.allowed_groups.contains(g)) {
            return Ok(());
        }
        Err(ApiError::forbidden(format!("your {} account is not in a group allowed to sign in here", self.display_name)))
    }
}

/// A claim of an ID token whose signature was just verified: the names in it,
/// whether the provider sends a list or a single one.
fn string_list_claim(verified: &openidconnect::core::CoreIdToken, claim: &str) -> Vec<String> {
    use base64::Engine;
    let jwt = verified.to_string();
    let payload = jwt.split('.').nth(1).and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok());
    let claims: serde_json::Value = payload.and_then(|p| serde_json::from_slice(&p).ok()).unwrap_or_default();
    match claims.get(claim) {
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|g| g.as_str().map(str::to_string)).collect(),
        Some(serde_json::Value::String(g)) => vec![g.clone()],
        _ => Vec::new(),
    }
}

fn redirect_url(state: &AppState, name: &str) -> String {
    state.cfg.web_url(&format!("api/v1/auth/oidc/{name}/callback"))
}

/// Holds the login's `state` in the browser that started it: the callback must
/// come back to that browser (else anyone could sign a victim into their own
/// account by sending them a callback link).
pub const STATE_COOKIE: &str = "forgust_oidc";
const STATE_COOKIE_PATH: &str = "/api/v1/auth/oidc/";

/// `STATE_COOKIE` for a sign-in just started (the state `start` returned).
pub fn state_cookie(state: &AppState, login_state: String) -> Cookie<'static> {
    Cookie::build((STATE_COOKIE, login_state))
        .path(STATE_COOKIE_PATH)
        .http_only(true)
        .secure(state.cfg.secure_cookies())
        // the provider sends the browser back with a top-level GET: Lax carries it
        .same_site(SameSite::Lax)
        .max_age(time::Duration::minutes(10))
        .build()
}

/// What `CookieJar::remove` takes to end `STATE_COOKIE`: the same cookie, emptied.
pub fn spent_state_cookie(state: &AppState) -> Cookie<'static> {
    state_cookie(state, String::new())
}

/// Build the provider's authorize URL and remember state, nonce and PKCE
/// verifier; the URL, and the state for `STATE_COOKIE`. With `link_user`, the
/// callback links the identity to that account instead of signing in.
pub async fn start(state: &AppState, name: &str, link_user: Option<i64>) -> ApiResult<(String, String)> {
    let p = state.oidc.providers.get(name).ok_or(ApiError::NotFound)?;
    let meta = state.oidc.discover(name, p).await?;
    let client =
        CoreClient::from_provider_metadata(meta, ClientId::new(p.client_id.clone()), Some(ClientSecret::new(p.client_secret.clone())))
            .set_redirect_uri(RedirectUrl::new(redirect_url(state, name)).map_err(|e| ApiError::Internal(e.into()))?);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut req = client.authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random);
    for s in &p.scopes {
        if s != "openid" {
            req = req.add_scope(Scope::new(s.clone()));
        }
    }
    let (url, csrf, nonce) = req.set_pkce_challenge(challenge).url();
    forgust_db::sessions::save_oidc_state(
        &state.db,
        csrf.secret(),
        name,
        nonce.secret(),
        verifier.secret(),
        link_user,
        Utc::now() + Duration::minutes(10),
    )
    .await?;
    Ok((url.to_string(), csrf.secret().clone()))
}

/// Consume what `start` remembered for this `state` value.
pub async fn pending(state: &AppState, csrf_state: &str) -> ApiResult<forgust_db::sessions::OidcState> {
    forgust_db::sessions::take_oidc_state(&state.db, csrf_state).await?.ok_or_else(|| ApiError::bad("unknown or expired login state"))
}

/// Exchange the code and verify the ID token.
pub async fn finish(state: &AppState, name: &str, code: &str, pending: forgust_db::sessions::OidcState) -> ApiResult<OidcIdentity> {
    if pending.provider != name {
        return Err(ApiError::bad("login state does not match provider"));
    }
    let (nonce, verifier) = (pending.nonce, pending.pkce_verifier);
    let p = state.oidc.providers.get(name).ok_or(ApiError::NotFound)?;
    let redirect = RedirectUrl::new(redirect_url(state, name)).map_err(|e| ApiError::Internal(e.into()))?;
    let client_for = |meta| {
        CoreClient::from_provider_metadata(meta, ClientId::new(p.client_id.clone()), Some(ClientSecret::new(p.client_secret.clone())))
            .set_redirect_uri(redirect.clone())
    };
    let client = client_for(state.oidc.discover(name, p).await?);
    let token = client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?
        .set_pkce_verifier(PkceCodeVerifier::new(verifier))
        .request_async(&state.oidc.http)
        .await
        .map_err(|e| ApiError::forbidden(format!("OIDC token exchange failed: {e}")))?;
    let id_token = token.id_token().ok_or_else(|| ApiError::forbidden("provider returned no ID token"))?;
    let nonce = Nonce::new(nonce);
    let invalid = |e| ApiError::forbidden(format!("invalid ID token: {e}"));
    let claims = match id_token.claims(&client.id_token_verifier(), &nonce) {
        Ok(claims) => claims,
        // the provider may have rotated its keys since we last asked
        Err(ClaimsVerificationError::SignatureVerification(_)) => {
            state.oidc.metadata.invalidate(name).await;
            let fresh = client_for(state.oidc.discover(name, p).await?);
            id_token.claims(&fresh.id_token_verifier(), &nonce).map_err(invalid)?
        }
        Err(e) => return Err(invalid(e)),
    };
    let verified_email = claims.email_verified().unwrap_or(false);
    Ok(OidcIdentity {
        subject: claims.subject().to_string(),
        email: claims.email().filter(|_| verified_email).map(|e| e.to_string()),
        username: claims.preferred_username().map(|u| u.to_string()),
        name: claims.name().and_then(|n| n.get(None)).map(|n| n.to_string()),
        groups: string_list_claim(id_token, &p.groups_claim),
    })
}
