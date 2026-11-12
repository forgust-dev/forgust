//! CI `id_tokens`: OIDC tokens a job hands to a cloud provider (AWS,
//! GCP, Vault...) to prove which project, ref and environment it runs for.
//! Signed with the instance's P-256 key (ES256; GitLab uses RS256, which the
//! usual consumers accept alike); the issuer is `server.external_url`, its
//! keys at `/oauth/discovery/keys`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};
use sha2::Digest;

use crate::state::AppState;

const KEY_NAME: &str = "ci_id_tokens_es256";

pub struct SigningKey {
    pair: EcdsaKeyPair,
    pub kid: String,
}

impl SigningKey {
    fn from_pkcs8(der: &[u8]) -> anyhow::Result<Self> {
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der, &SystemRandom::new())
            .map_err(|_| anyhow::anyhow!("the CI signing key is damaged"))?;
        let kid = URL_SAFE_NO_PAD.encode(&sha2::Sha256::digest(pair.public_key().as_ref())[..16]);
        Ok(SigningKey { pair, kid })
    }

    /// The public key as a JWK (x and y of the uncompressed point).
    pub fn jwk(&self) -> Value {
        let p = self.pair.public_key().as_ref();
        json!({
            "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": self.kid,
            "x": URL_SAFE_NO_PAD.encode(&p[1..33]), "y": URL_SAFE_NO_PAD.encode(&p[33..65]),
        })
    }

    pub fn sign(&self, claims: &Value) -> anyhow::Result<String> {
        let head = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({ "alg": "ES256", "typ": "JWT", "kid": self.kid }))?);
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
        let input = format!("{head}.{body}");
        let sig = self.pair.sign(&SystemRandom::new(), input.as_bytes()).map_err(|_| anyhow::anyhow!("signing failed"))?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref())))
    }
}

/// The instance's key: made on first use and kept (encrypted) in the database.
pub async fn key(state: &AppState) -> anyhow::Result<Arc<SigningKey>> {
    state
        .oidc_key
        .get_or_try_init(|| async {
            let enc = match forgust_db::ci::instance_key(&state.db, KEY_NAME).await? {
                Some(enc) => enc,
                None => {
                    let der = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                        .map_err(|_| anyhow::anyhow!("key generation failed"))?;
                    let enc = state.secrets.encrypt(der.as_ref())?;
                    // two workers at once: whoever wrote first wins
                    forgust_db::ci::put_instance_key(&state.db, KEY_NAME, &enc).await?
                }
            };
            Ok::<_, anyhow::Error>(Arc::new(SigningKey::from_pkcs8(&state.secrets.decrypt(&enc)?)?))
        })
        .await
        .cloned()
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/oauth/discovery/keys", get(jwks))
        .route("/-/jwks", get(jwks))
}

async fn discovery(State(state): State<AppState>) -> Response {
    let iss = state.cfg.server.external_url.clone();
    axum::Json(json!({
        "issuer": iss,
        "jwks_uri": format!("{iss}/oauth/discovery/keys"),
        "response_types_supported": ["id_token"],
        "subject_types_supported": ["public", "pairwise"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "claims_supported": [
            "sub", "aud", "exp", "iat", "iss", "jti", "nbf", "namespace_id", "namespace_path", "project_id",
            "project_path", "user_id", "user_login", "user_email", "pipeline_id", "pipeline_source", "job_id",
            "ref", "ref_type", "ref_path", "ref_protected", "environment", "environment_protected",
            "deployment_tier", "runner_id", "runner_environment", "sha", "project_visibility",
        ],
    }))
    .into_response()
}

async fn jwks(State(state): State<AppState>) -> Response {
    match key(&state).await {
        Ok(k) => axum::Json(json!({ "keys": [k.jwk()] })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "CI signing key");
            axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// What a job's token says about it (GitLab's claim names).
pub struct JobFacts<'a> {
    pub project: &'a forgust_db::projects::Project,
    pub namespace_path: &'a str,
    pub user: Option<&'a forgust_db::users::User>,
    pub pipeline: &'a forgust_db::ci::Pipeline,
    pub job_id: i64,
    pub runner_id: i64,
    pub environment: Option<(&'a str, &'a str, bool)>,
    pub timeout_secs: i64,
}

/// One token per `id_tokens` entry: (variable name, JWT).
pub async fn tokens(
    state: &AppState,
    f: &JobFacts<'_>,
    wanted: &forgust_ci_config::IndexMap<String, forgust_ci_config::job::IdToken>,
) -> anyhow::Result<Vec<(String, String)>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let k = key(state).await?;
    let now = chrono::Utc::now().timestamp();
    let ref_type = if f.pipeline.tag { "tag" } else { "branch" };
    let ref_path = format!("refs/{}/{}", if f.pipeline.tag { "tags" } else { "heads" }, f.pipeline.git_ref);
    let visibility = match f.project.visibility {
        forgust_db::visibility::PUBLIC => "public",
        forgust_db::visibility::INTERNAL => "internal",
        _ => "private",
    };
    let mut out = Vec::new();
    for (name, t) in wanted {
        let mut c = json!({
            "iss": state.cfg.server.external_url,
            "sub": format!("project_path:{}:ref_type:{ref_type}:ref:{}", f.project.full_path, f.pipeline.git_ref),
            "aud": if t.aud.len() == 1 { Value::String(t.aud[0].clone()) } else { json!(t.aud) },
            "iat": now, "nbf": now - 5, "exp": now + f.timeout_secs.clamp(300, 86400),
            "jti": crate::crypto::random_urlsafe(16),
            "namespace_id": f.project.namespace_id.to_string(),
            "namespace_path": f.namespace_path,
            "project_id": f.project.id.to_string(),
            "project_path": f.project.full_path,
            "project_visibility": visibility,
            "pipeline_id": f.pipeline.id.to_string(),
            "pipeline_source": f.pipeline.source,
            "job_id": f.job_id.to_string(),
            "ref": f.pipeline.git_ref,
            "ref_type": ref_type,
            "ref_path": ref_path,
            "ref_protected": f.pipeline.protected.to_string(),
            "runner_id": f.runner_id,
            "runner_environment": "self-hosted",
            "sha": f.pipeline.sha,
        });
        if let Some(u) = f.user {
            c["user_id"] = Value::String(u.id.to_string());
            c["user_login"] = Value::String(u.username.clone());
            c["user_email"] = Value::String(u.email.clone());
        }
        if let Some((env, tier, protected)) = f.environment {
            c["environment"] = Value::String(env.into());
            c["deployment_tier"] = Value::String(tier.into());
            c["environment_protected"] = Value::String(protected.to_string());
        }
        out.push((name.clone(), k.sign(&c)?));
    }
    Ok(out)
}
