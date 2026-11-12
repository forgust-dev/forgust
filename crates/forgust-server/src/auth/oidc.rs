//! OpenID Connect login (authorization code + PKCE).

use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
};

use crate::config::OidcProvider;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Clone, Default)]
pub struct OidcClients {
    pub providers: BTreeMap<String, OidcProvider>,
}

/// What the provider told us about the person.
pub struct OidcIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub username: Option<String>,
    pub name: Option<String>,
}

fn http_client() -> ApiResult<openidconnect::reqwest::Client> {
    openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| ApiError::Internal(e.into()))
}

fn redirect_url(state: &AppState, name: &str) -> String {
    state.cfg.web_url(&format!("api/v1/auth/oidc/{name}/callback"))
}

/// Holds the login's `state` in the browser that started it: the callback must
/// come back to that browser (else anyone could sign a victim into their own
/// account by sending them a callback link).
pub const STATE_COOKIE: &str = "forgust_oidc";

/// Build the provider's authorize URL and remember state, nonce and PKCE
/// verifier; the URL, and the state for `STATE_COOKIE`.
pub async fn start(state: &AppState, name: &str) -> ApiResult<(String, String)> {
    let p = state.oidc.providers.get(name).ok_or(ApiError::NotFound)?;
    let http = http_client()?;
    let issuer = IssuerUrl::new(p.issuer.clone()).map_err(|e| ApiError::Internal(e.into()))?;
    let meta = CoreProviderMetadata::discover_async(issuer, &http)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("OIDC discovery: {e}")))?;
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
        Utc::now() + Duration::minutes(10),
    )
    .await?;
    Ok((url.to_string(), csrf.secret().clone()))
}

/// Exchange the code and verify the ID token.
pub async fn finish(state: &AppState, name: &str, code: &str, csrf_state: &str) -> ApiResult<OidcIdentity> {
    let (provider, nonce, verifier) = forgust_db::sessions::take_oidc_state(&state.db, csrf_state)
        .await?
        .ok_or_else(|| ApiError::bad("unknown or expired login state"))?;
    if provider != name {
        return Err(ApiError::bad("login state does not match provider"));
    }
    let p = state.oidc.providers.get(name).ok_or(ApiError::NotFound)?;
    let http = http_client()?;
    let issuer = IssuerUrl::new(p.issuer.clone()).map_err(|e| ApiError::Internal(e.into()))?;
    let meta = CoreProviderMetadata::discover_async(issuer, &http)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("OIDC discovery: {e}")))?;
    let client =
        CoreClient::from_provider_metadata(meta, ClientId::new(p.client_id.clone()), Some(ClientSecret::new(p.client_secret.clone())))
            .set_redirect_uri(RedirectUrl::new(redirect_url(state, name)).map_err(|e| ApiError::Internal(e.into()))?);
    let token = client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?
        .set_pkce_verifier(PkceCodeVerifier::new(verifier))
        .request_async(&http)
        .await
        .map_err(|e| ApiError::forbidden(format!("OIDC token exchange failed: {e}")))?;
    let id_token = token.id_token().ok_or_else(|| ApiError::forbidden("provider returned no ID token"))?;
    let claims = id_token
        .claims(&client.id_token_verifier(), &Nonce::new(nonce))
        .map_err(|e| ApiError::forbidden(format!("invalid ID token: {e}")))?;
    let verified_email = claims.email_verified().unwrap_or(false);
    Ok(OidcIdentity {
        subject: claims.subject().to_string(),
        email: claims.email().filter(|_| verified_email).map(|e| e.to_string()),
        username: claims.preferred_username().map(|u| u.to_string()),
        name: claims.name().and_then(|n| n.get(None)).map(|n| n.to_string()),
    })
}
