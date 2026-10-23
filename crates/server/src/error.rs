use axum::Json;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("{0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Forbidden(String),
    /// Too many requests of a kind, or one already running.
    #[error("{0}")]
    Busy(String),
    /// A service we depend on failed (a provider, the LLM endpoint).
    #[error("{0}")]
    Upstream(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn bad(msg: impl Into<String>) -> Self {
        ApiError::BadRequest(msg.into())
    }

    fn parts(&self) -> (StatusCode, String) {
        match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m.clone()),
            ApiError::Busy(m) => (StatusCode::TOO_MANY_REQUESTS, m.clone()),
            ApiError::Upstream(m) => (StatusCode::BAD_GATEWAY, m.clone()),
            ApiError::Db(e) => {
                tracing::error!(error = %e, "database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
            ApiError::Internal(e) => {
                tracing::error!(error = %e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        }
    }
}

/// JSON for `/api/v1`.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = self.parts();
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

/// The same errors for browser pages: an HTML snippet. HTMX swaps it into the
/// page's `#flash` box (HX-Retarget) instead of the request's own target, so
/// a failed action never replaces a form with an error message.
#[derive(Debug)]
pub struct WebError(pub ApiError);

impl<E: Into<ApiError>> From<E> for WebError {
    fn from(e: E) -> Self {
        WebError(e.into())
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, msg) = self.0.parts();
        let body = format!("<p class=\"error\" role=\"alert\">{}</p>", escape(&msg));
        let mut r = (status, axum::response::Html(body)).into_response();
        let h = r.headers_mut();
        h.insert("hx-retarget", HeaderValue::from_static("#flash"));
        h.insert("hx-reswap", HeaderValue::from_static("innerHTML"));
        r
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
