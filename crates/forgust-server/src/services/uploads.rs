//! Files attached to a project (M7): images and files in comments and
//! descriptions, release files. GitLab's markdown uploads: `POST
//! /projects/:id/uploads`, served at `/-/project/<id>/uploads/<secret>/<name>`
//! to whoever can see the project.
//!
//! These are files anyone signed in who can see the project may add, served from
//! this origin: only a few image and video types are shown inline, everything else
//! downloads as `application/octet-stream`, always with `nosniff` and a sandboxing
//! CSP. They are streamed, never presigned (an S3 URL would lose the name and the
//! headers). Deleting the row queues the blob for deletion (`blob_deletions`).

use axum::extract::Multipart;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::auth::authz::{ProjectAction, require_project};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use forgust_db::projects::Project;
use forgust_db::releases::{NewUpload, Upload};

/// Shown inline; anything else downloads.
const INLINE: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp", "image/avif", "video/mp4", "video/webm"];

#[derive(Debug, Serialize, ToSchema)]
pub struct Uploaded {
    pub id: i64,
    /// The file name, as alt text.
    pub alt: String,
    /// `/uploads/<secret>/<name>` (GitLab's project-relative form).
    pub url: String,
    /// `/-/project/<id>/uploads/<secret>/<name>`: where it is served.
    pub full_path: String,
    /// Markdown that shows (images, videos) or links to it.
    pub markdown: String,
    pub size: i64,
}

pub fn full_path(u: &Upload) -> String {
    format!("/-/project/{}/uploads/{}/{}", u.project_id, u.secret, encode(&u.filename))
}

use crate::blob::encode_segment as encode;

pub fn describe(u: &Upload) -> Uploaded {
    let full = full_path(u);
    let alt: String = u.filename.chars().filter(|c| !"[]()".contains(*c)).collect();
    let bang = if INLINE.contains(&u.content_type.as_str()) { "!" } else { "" };
    Uploaded {
        id: u.id,
        markdown: format!("{bang}[{alt}]({full})"),
        alt,
        url: format!("/uploads/{}/{}", u.secret, encode(&u.filename)),
        full_path: full,
        size: u.size,
    }
}

/// A name safe to store and show: no directories, no control characters.
fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let n: String = base.chars().filter(|c| !c.is_control()).collect::<String>().trim().trim_start_matches('.').to_string();
    let n = if n.is_empty() { "file".to_string() } else { n };
    n.chars().take(200).collect()
}

/// Store the multipart field `file`. Signed-in people who can see the project
/// (they can comment there).
pub async fn store(state: &AppState, principal: &Principal, p: &Project, mut multipart: Multipart) -> ApiResult<Upload> {
    require_project(state, principal, ProjectAction::Read, p).await?;
    crate::auth::authz::writable(p)?;
    // a person, or a CI job acting for one
    let who = principal.user().ok_or(ApiError::Unauthorized)?;
    let user = Some(who.id);
    // members get the full size; visitors of a public project a smaller one
    let member = who.is_admin || forgust_db::authz::project_level(&state.db, who.id, p.id).await?.unwrap_or(0) > 0;
    let file_limit = if member { state.cfg.limits.max_upload_bytes } else { state.cfg.limits.max_upload_bytes_non_member };
    // no more than the storage quota leaves
    let room = crate::services::quotas::room(state, p.id).await?;
    if let Some(r) = room.as_ref().filter(|r| r.full()) {
        return Err(ApiError::TooLarge(r.message()));
    }
    let limit = room.as_ref().map_or(file_limit, |r| file_limit.min(r.left));
    let per_hour = state.cfg.limits.uploads_per_hour;
    if per_hour > 0 && !who.is_admin && forgust_db::releases::recent_uploads_by(&state.db, who.id).await? >= per_hour as i64 {
        return Err(ApiError::TooMany(format!("at most {per_hour} files an hour: try again later")));
    }
    while let Some(field) = multipart.next_field().await.map_err(|e| ApiError::bad(format!("bad multipart body: {e}")))? {
        if field.name() != Some("file") {
            continue;
        }
        let filename = clean_name(field.file_name().unwrap_or("file"));
        let content_type = mime_guess::from_path(&filename).first_or_octet_stream().essence_str().to_string();
        let secret = hex::encode(rand::random::<[u8; 16]>());
        let key = format!("uploads/{}/{secret}", p.id);
        let size = match crate::blob::stream_field(&state.blobs, field, &key, limit).await {
            Ok(n) => n,
            Err(crate::blob::StreamError::TooLarge) => {
                return Err(ApiError::TooLarge(match room.as_ref().filter(|r| r.left < file_limit) {
                    Some(r) => r.message(),
                    None => format!("files are limited to {} MB", file_limit >> 20),
                }));
            }
            Err(crate::blob::StreamError::Interrupted(e)) => return Err(ApiError::bad(format!("upload interrupted: {e}"))),
            Err(crate::blob::StreamError::Store(e)) => return Err(ApiError::Internal(e)),
        };
        let u = forgust_db::releases::add_upload(
            &state.db,
            NewUpload {
                project_id: p.id,
                secret: &secret,
                filename: &filename,
                size: size as i64,
                content_type: &content_type,
                blob_key: &key,
                uploaded_by: user,
            },
        )
        .await?;
        metrics::counter!("forgust_uploaded_bytes_total").increment(size);
        return Ok(u);
    }
    Err(ApiError::bad("the `file` part is missing"))
}

/// The file at `/-/project/<id>/uploads/<secret>/<name>`; the name must match.
pub async fn serve(state: &AppState, principal: &Principal, p: &Project, secret: &str, name: &str) -> ApiResult<Response> {
    require_project(state, principal, ProjectAction::Read, p).await?;
    let u = forgust_db::releases::upload(&state.db, p.id, secret).await?.filter(|u| u.filename == name).ok_or(ApiError::NotFound)?;
    if let Some(r) = presigned(state, &u).await? {
        return Ok(r);
    }
    let Some((size, stream)) = state.blobs.get(&u.blob_key).await.map_err(ApiError::Internal)? else {
        return Err(ApiError::NotFound);
    };
    Ok(safe_file(axum::body::Body::from_stream(stream), &u.filename, Some(size), false))
}

/// Downloads (not shown inline) may come straight from S3 through a short-lived
/// presigned link that sets the name and an octet-stream type; access was
/// checked before. Inline files stay with Forgust (S3 cannot add `nosniff` or the CSP).
async fn presigned(state: &AppState, u: &Upload) -> ApiResult<Option<Response>> {
    if INLINE.contains(&u.content_type.as_str()) {
        return Ok(None);
    }
    let disposition = crate::blob::disposition(&u.filename, false);
    let url = state
        .blobs
        .presigned_download(&u.blob_key, std::time::Duration::from_secs(300), "application/octet-stream", &disposition)
        .await
        .map_err(ApiError::Internal)?;
    Ok(url.map(|url| {
        let mut r = Response::new(axum::body::Body::empty());
        *r.status_mut() = axum::http::StatusCode::FOUND;
        if let Ok(v) = HeaderValue::from_str(url.as_str()) {
            r.headers_mut().insert(header::LOCATION, v);
        }
        r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        r
    }))
}

/// A file people put on this origin, served so it cannot run here: a few image
/// and video types (and, with `text`, plain text) inline, anything else as an
/// `application/octet-stream` download; always `nosniff` and a sandboxing CSP.
pub fn safe_file(body: axum::body::Body, filename: &str, size: Option<u64>, text: bool) -> Response {
    let guessed = mime_guess::from_path(filename).first_or_octet_stream();
    let essence = guessed.essence_str();
    let is_text = text
        && (guessed.type_() == "text"
            || matches!(essence, "application/json" | "application/xml" | "application/x-sh" | "application/toml"));
    let ct = if INLINE.contains(&essence) {
        essence.to_string()
    } else if is_text {
        "text/plain; charset=utf-8".to_string()
    } else {
        "application/octet-stream".to_string()
    };
    let inline = INLINE.contains(&essence) || is_text;
    let mut r = Response::new(body);
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&ct) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Some(n) = size {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(n));
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    let disposition = crate::blob::disposition(filename, inline);
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    r
}

/// Delete blobs whose rows are gone (run from the CI tick).
pub async fn drain_deletions(state: &AppState) -> anyhow::Result<()> {
    loop {
        let keys = forgust_db::releases::take_blob_deletions(&state.db, 500).await?;
        if keys.is_empty() {
            return Ok(());
        }
        for k in keys {
            if let Err(e) = state.blobs.delete(&k).await {
                tracing::warn!(key = %k, error = %e, "blob of a deleted row not deleted");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\x\\shot.png"), "shot.png");
        assert_eq!(clean_name(".htaccess"), "htaccess");
        assert_eq!(clean_name("  "), "file");
        assert_eq!(encode("a b(1).tar.gz"), "a%20b%281%29.tar.gz");
    }
}
