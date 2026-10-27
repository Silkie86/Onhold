//! The API's error type. Every error is `{"error": {"code", "message", "field"?}}`
//! with a stable machine-readable `code`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use core_domain::AuthzError;
use core_domain::workspace::ValidationError;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{message}")]
    BadRequest {
        code: &'static str,
        message: String,
        field: Option<&'static str>,
    },
    #[error("authentication required")]
    Unauthenticated,
    #[error("{message}")]
    Unauthorized { code: &'static str, message: String },
    #[error("{message}")]
    Forbidden { code: &'static str, message: String },
    #[error("not found")]
    NotFound,
    #[error("{message}")]
    Conflict { code: &'static str, message: String },
    #[error("{message}")]
    PreconditionFailed { code: &'static str, message: String },
    #[error("{message}")]
    Unprocessable { code: &'static str, message: String },
    #[error("internal error")]
    Internal(String),
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorDetail {
    /// Stable identifier for programs, e.g. `invalid_credentials`.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// The request field at fault, for validation errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl ApiError {
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::BadRequest {
            code,
            message: message.into(),
            field: None,
        }
    }

    pub fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        Self::Forbidden {
            code,
            message: message.into(),
        }
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            code,
            message: message.into(),
        }
    }

    pub fn unauthorized(code: &'static str, message: impl Into<String>) -> Self {
        Self::Unauthorized {
            code,
            message: message.into(),
        }
    }

    pub fn version_conflict() -> Self {
        Self::PreconditionFailed {
            code: "version_conflict",
            message: "this record changed since you loaded it; reload and try again".into(),
        }
    }

    pub fn internal(err: impl std::fmt::Display) -> Self {
        Self::Internal(err.to_string())
    }

    fn parts(&self) -> (StatusCode, &str, String, Option<&str>) {
        match self {
            ApiError::BadRequest {
                code,
                message,
                field,
            } => (StatusCode::BAD_REQUEST, code, message.clone(), *field),
            ApiError::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "sign in or provide an API key".into(),
                None,
            ),
            ApiError::Unauthorized { code, message } => {
                (StatusCode::UNAUTHORIZED, code, message.clone(), None)
            }
            ApiError::Forbidden { code, message } => {
                (StatusCode::FORBIDDEN, code, message.clone(), None)
            }
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found", "not found".into(), None),
            ApiError::Conflict { code, message } => {
                (StatusCode::CONFLICT, code, message.clone(), None)
            }
            ApiError::PreconditionFailed { code, message } => {
                (StatusCode::PRECONDITION_FAILED, code, message.clone(), None)
            }
            ApiError::Unprocessable { code, message } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                code,
                message.clone(),
                None,
            ),
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "something went wrong; the error has been logged".into(),
                None,
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::Internal(detail) = &self {
            tracing::error!(error = %detail, "internal error");
        }
        let (status, code, message, field) = self.parts();
        let body = ErrorBody {
            error: ErrorDetail {
                code: code.to_owned(),
                message,
                field: field.map(str::to_owned),
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<core_db::DbError> for ApiError {
    fn from(err: core_db::DbError) -> Self {
        ApiError::internal(err)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        ApiError::internal(err)
    }
}

impl From<AuthzError> for ApiError {
    fn from(err: AuthzError) -> Self {
        ApiError::forbidden("permission_denied", err.to_string())
    }
}

impl From<ValidationError> for ApiError {
    fn from(err: ValidationError) -> Self {
        ApiError::BadRequest {
            code: "invalid_input",
            message: err.message,
            field: Some(err.field),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
