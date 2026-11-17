//! Recovery codes: one-time stand-ins for the second factor.

use sqlx::{PgConnection, PgExecutor};

use crate::Result;

/// Replace the account's codes with these (hashes).
pub async fn replace(c: &mut PgConnection, user_id: i64, hashes: &[Vec<u8>]) -> Result<()> {
    delete_for_user(&mut *c, user_id).await?;
    sqlx::query!("INSERT INTO recovery_codes (user_id, code_hash) SELECT $1, unnest($2::bytea[])", user_id, hashes)
        .execute(&mut *c)
        .await?;
    Ok(())
}

/// Use a code: true when it was one of the account's, now gone (a code works once).
pub async fn consume(e: impl PgExecutor<'_>, user_id: i64, hash: &[u8]) -> Result<bool> {
    Ok(sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1 AND code_hash = $2", user_id, hash).execute(e).await?.rows_affected()
        == 1)
}

pub async fn remaining(e: impl PgExecutor<'_>, user_id: i64) -> Result<i64> {
    sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM recovery_codes WHERE user_id = $1"#, user_id).fetch_one(e).await
}

pub async fn delete_for_user(e: impl PgExecutor<'_>, user_id: i64) -> Result<()> {
    sqlx::query!("DELETE FROM recovery_codes WHERE user_id = $1", user_id).execute(e).await?;
    Ok(())
}
