use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use forgust_storage::StorageError;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("two-factor authentication is required by this organization")]
    TwoFactorRequired,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("{0}")]
    TooMany(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        ApiError::BadRequest(msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        ApiError::Forbidden(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        ApiError::Conflict(msg.into())
    }
    fn code(&self) -> (StatusCode, &'static str) {
        match self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::TwoFactorRequired => (StatusCode::FORBIDDEN, "two_factor_required"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApiError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            ApiError::TooMany(_) => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests"),
            ApiError::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.code();
        if let ApiError::Internal(e) = &self {
            tracing::error!(error = ?e, "internal error");
        }
        (status, Json(json!({ "error": code, "message": self.to_string() }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        if forgust_db::is_unique_violation(&e) {
            return ApiError::Conflict("already exists".into());
        }
        ApiError::Internal(e.into())
    }
}

impl From<StorageError> for ApiError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::RepoNotFound(_) | StorageError::RevisionNotFound(_) | StorageError::PathNotFound(_) => ApiError::NotFound,
            StorageError::Invalid(m) => ApiError::BadRequest(m),
            StorageError::RepoExists(_) => ApiError::Conflict("repository already exists".into()),
            StorageError::Busy => ApiError::Unavailable(StorageError::Busy.to_string()),
            other => ApiError::Internal(other.into()),
        }
    }
}
