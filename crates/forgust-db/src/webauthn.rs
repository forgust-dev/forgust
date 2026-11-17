//! WebAuthn credentials (security keys and passkeys) and the ceremonies that
//! make and use them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgTypeInfo, PgValueRef, Postgres};
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use crate::Result;

/// What a credential is for: a second factor after the password, or a
/// sign-in of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    SecurityKey,
    Passkey,
}

impl CredentialKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialKind::SecurityKey => "security_key",
            CredentialKind::Passkey => "passkey",
        }
    }

    /// How people call it ("a security key was added").
    pub fn label(self) -> &'static str {
        match self {
            CredentialKind::SecurityKey => "security key",
            CredentialKind::Passkey => "passkey",
        }
    }

    /// The ceremony that registers one.
    pub fn registration(self) -> &'static str {
        match self {
            CredentialKind::SecurityKey => "register_security_key",
            CredentialKind::Passkey => "register_passkey",
        }
    }

    pub fn of_registration(ceremony: &str) -> Option<CredentialKind> {
        [CredentialKind::SecurityKey, CredentialKind::Passkey].into_iter().find(|k| k.registration() == ceremony)
    }
}

// stored as text: the column's CHECK lists the same values
impl sqlx::Type<Postgres> for CredentialKind {
    fn type_info() -> PgTypeInfo {
        <&str as sqlx::Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <&str as sqlx::Type<Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, Postgres> for CredentialKind {
    fn decode(value: PgValueRef<'r>) -> std::result::Result<Self, sqlx::error::BoxDynError> {
        match <&str as sqlx::Decode<Postgres>>::decode(value)? {
            "security_key" => Ok(CredentialKind::SecurityKey),
            "passkey" => Ok(CredentialKind::Passkey),
            other => Err(format!("unknown WebAuthn credential kind {other:?}").into()),
        }
    }
}

/// The ceremony of a passkey sign-in.
pub const PASSKEY_LOGIN: &str = "passkey_login";

/// A credential as its owner sees it (no key material).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CredentialInfo {
    pub id: i64,
    pub kind: CredentialKind,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A stored credential, for a ceremony.
pub struct Stored {
    pub id: i64,
    pub user_id: i64,
    pub kind: CredentialKind,
    pub credential: serde_json::Value,
    /// Its account's handle for authenticators.
    pub user_handle: Option<Uuid>,
}

pub async fn list(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<CredentialInfo>> {
    sqlx::query_as!(
        CredentialInfo,
        r#"SELECT id, kind AS "kind: CredentialKind", name, created_at, last_used_at FROM webauthn_credentials WHERE user_id = $1 ORDER BY kind, created_at"#,
        user_id
    )
    .fetch_all(e)
    .await
}

/// The account's credentials of one kind, with their key material.
pub async fn credentials(e: impl PgExecutor<'_>, user_id: i64, kind: CredentialKind) -> Result<Vec<Stored>> {
    sqlx::query_as!(
        Stored,
        r#"SELECT c.id, c.user_id, c.kind AS "kind: CredentialKind", c.credential, u.webauthn_handle AS user_handle
             FROM webauthn_credentials c JOIN users u ON u.id = c.user_id
            WHERE c.user_id = $1 AND c.kind = $2 ORDER BY c.id"#,
        user_id,
        kind.as_str()
    )
    .fetch_all(e)
    .await
}

/// The ids of all the account's credentials (what an authenticator must not register again).
pub async fn credential_ids(e: impl PgExecutor<'_>, user_id: i64) -> Result<Vec<Vec<u8>>> {
    sqlx::query_scalar!("SELECT credential_id FROM webauthn_credentials WHERE user_id = $1", user_id).fetch_all(e).await
}

pub async fn find_by_credential_id(e: impl PgExecutor<'_>, credential_id: &[u8]) -> Result<Option<Stored>> {
    sqlx::query_as!(
        Stored,
        r#"SELECT c.id, c.user_id, c.kind AS "kind: CredentialKind", c.credential, u.webauthn_handle AS user_handle
             FROM webauthn_credentials c JOIN users u ON u.id = c.user_id
            WHERE c.credential_id = $1"#,
        credential_id
    )
    .fetch_optional(e)
    .await
}

/// After any change to the account's credentials: `users.security_keys_enabled`
/// follows them, and the recovery codes go with the last second factor.
async fn refresh(c: &mut PgConnection, user_id: i64) -> Result<()> {
    sqlx::query!(
        "UPDATE users SET security_keys_enabled = EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id = $1 AND kind = 'security_key'),
                          updated_at = now()
          WHERE id = $1",
        user_id
    )
    .execute(&mut *c)
    .await?;
    crate::recovery_codes::delete_unless_needed(c, user_id).await
}

pub async fn insert(
    c: &mut PgConnection,
    user_id: i64,
    kind: CredentialKind,
    credential_id: &[u8],
    credential: &serde_json::Value,
    name: &str,
    rp_id: &str,
) -> Result<CredentialInfo> {
    let row = sqlx::query_as!(
        CredentialInfo,
        r#"INSERT INTO webauthn_credentials (user_id, kind, credential_id, credential, name, rp_id)
           VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, kind AS "kind: CredentialKind", name, created_at, last_used_at"#,
        user_id,
        kind.as_str(),
        credential_id,
        credential,
        name,
        rp_id
    )
    .fetch_one(&mut *c)
    .await?;
    refresh(c, user_id).await?;
    Ok(row)
}

pub async fn rename(e: impl PgExecutor<'_>, user_id: i64, id: i64, name: &str) -> Result<bool> {
    Ok(sqlx::query!("UPDATE webauthn_credentials SET name = $3 WHERE id = $2 AND user_id = $1", user_id, id, name)
        .execute(e)
        .await?
        .rows_affected()
        == 1)
}

/// Remove one of the account's credentials: what it was (kind, name), if it was there.
pub async fn delete(c: &mut PgConnection, user_id: i64, id: i64) -> Result<Option<(CredentialKind, String)>> {
    let r = sqlx::query!(
        r#"DELETE FROM webauthn_credentials WHERE id = $2 AND user_id = $1 RETURNING kind AS "kind: CredentialKind", name"#,
        user_id,
        id
    )
    .fetch_optional(&mut *c)
    .await?;
    if r.is_some() {
        refresh(c, user_id).await?;
    }
    Ok(r.map(|r| (r.kind, r.name)))
}

/// Remove all the account's credentials (an administrator's reset, an address
/// that turned out to be someone else's).
pub async fn delete_for_user(c: &mut PgConnection, user_id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM webauthn_credentials WHERE user_id = $1", user_id).execute(&mut *c).await?;
    refresh(c, user_id).await
}

/// After a successful ceremony: when, and the counter and flags it moved.
pub async fn used(e: impl PgExecutor<'_>, id: i64, credential: Option<&serde_json::Value>) -> Result<()> {
    sqlx::query!(
        "UPDATE webauthn_credentials SET last_used_at = now(), credential = COALESCE($2, credential) WHERE id = $1",
        id,
        credential
    )
    .execute(e)
    .await?;
    Ok(())
}

/// The account's handle for authenticators, made on first use from `fresh`.
pub async fn handle(e: impl PgExecutor<'_>, user_id: i64, fresh: Uuid) -> Result<Uuid> {
    let h = sqlx::query_scalar!(
        "UPDATE users SET webauthn_handle = COALESCE(webauthn_handle, $2) WHERE id = $1 RETURNING webauthn_handle",
        user_id,
        fresh
    )
    .fetch_one(e)
    .await?;
    Ok(h.unwrap_or(fresh))
}

pub async fn save_ceremony(
    e: impl PgExecutor<'_>,
    token_hash: &[u8],
    kind: &str,
    user_id: Option<i64>,
    session_id: Option<i64>,
    state: &serde_json::Value,
    expires_at: DateTime<Utc>,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO webauthn_ceremonies (token_hash, kind, user_id, session_id, state, expires_at) VALUES ($1, $2, $3, $4, $5, $6)",
        token_hash,
        kind,
        user_id,
        session_id,
        state,
        expires_at
    )
    .execute(e)
    .await?;
    Ok(())
}

pub struct Ceremony {
    pub kind: String,
    pub user_id: Option<i64>,
    pub session_id: Option<i64>,
    pub state: serde_json::Value,
}

/// Use up a ceremony (whatever the outcome of what follows: an answer is
/// checked once). The caller checks its kind.
pub async fn take_ceremony(e: impl PgExecutor<'_>, token_hash: &[u8]) -> Result<Option<Ceremony>> {
    sqlx::query_as!(
        Ceremony,
        "DELETE FROM webauthn_ceremonies WHERE token_hash = $1 AND expires_at > now() RETURNING kind, user_id, session_id, state",
        token_hash
    )
    .fetch_optional(e)
    .await
}

/// What a pending sign-in's security key is asked to sign: false when the
/// challenge is gone.
pub async fn set_challenge_state(e: impl PgExecutor<'_>, token_hash: &[u8], state: &serde_json::Value) -> Result<bool> {
    Ok(sqlx::query!("UPDATE login_challenges SET webauthn_state = $2 WHERE token_hash = $1 AND expires_at > now()", token_hash, state)
        .execute(e)
        .await?
        .rows_affected()
        == 1)
}
