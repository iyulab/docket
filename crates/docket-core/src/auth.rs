//! Shared-token gate for the HTTP API (ADR-0026).
//!
//! Lives in the binary, not the library: authentication is a property of the
//! transport edge, and the library crate's vocabulary (worker, topic, item,
//! claim, ...) stays free of it.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};

use crate::ErrorBody;

/// Shortest token the server accepts. A shorter secret is a typo or a
/// placeholder, and an open door that looks closed is worse than a refusal to
/// start.
pub const MIN_TOKEN_LEN: usize = 16;

/// The configured shared token, or none when auth is disabled.
#[derive(Clone, Default)]
pub struct Auth(Option<Vec<u8>>);

impl Auth {
    /// Builds the gate from the raw `DOCKET_TOKEN` value.
    ///
    /// Unset means auth is disabled. A value that is set but blank or too
    /// short is an error: failing closed at startup beats silently serving an
    /// API the operator believes is protected.
    pub fn from_env_value(value: Option<String>) -> Result<Self, String> {
        match value {
            None => Ok(Self(None)),
            Some(v) => {
                let v = v.trim();
                if v.len() < MIN_TOKEN_LEN {
                    return Err(format!(
                        "DOCKET_TOKEN is set but shorter than {MIN_TOKEN_LEN} characters; \
                         unset it to run without auth, or use a longer secret \
                         (for example `openssl rand -hex 32`)"
                    ));
                }
                Ok(Self(Some(v.as_bytes().to_vec())))
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Whether `authorization` (a request's `Authorization` header) carries
    /// the configured token. Always true when auth is disabled.
    fn permits(&self, authorization: Option<&HeaderValue>) -> bool {
        let Some(expected) = &self.0 else {
            return true;
        };
        authorization
            .and_then(|v| v.to_str().ok())
            .and_then(bearer_token)
            .is_some_and(|presented| constant_time_eq(presented.as_bytes(), expected))
    }
}

/// The credential in a `Bearer <token>` value. The scheme is matched
/// case-insensitively (RFC 9110 section 11.1).
fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

/// Compares without an early exit on the first differing byte, so response
/// time does not reveal how much of a guessed token was right. A length
/// mismatch still returns early; the length of the token is not a secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Rejects a request that does not carry the token with `401`.
///
/// Layered onto the API routes only. The console's static shell, its assets
/// and `/status` carry no item data and stay reachable, which is what lets a
/// browser load the console before it has a token to send.
pub async fn require_token(
    Extension(auth): Extension<Auth>,
    request: Request,
    next: Next,
) -> Response {
    if auth.permits(request.headers().get(header::AUTHORIZATION)) {
        return next.run(request).await;
    }
    unauthorized().into_response()
}

fn unauthorized() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    (
        StatusCode::UNAUTHORIZED,
        headers,
        Json(ErrorBody {
            error: "unauthorized: send `Authorization: Bearer <DOCKET_TOKEN>`".to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123";

    fn auth() -> Auth {
        Auth::from_env_value(Some(TOKEN.to_string())).unwrap()
    }

    fn header(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).unwrap()
    }

    #[test]
    fn unset_token_disables_auth_and_permits_everything() {
        let auth = Auth::from_env_value(None).unwrap();
        assert!(!auth.is_enabled());
        assert!(auth.permits(None));
    }

    #[test]
    fn blank_or_short_token_is_refused_rather_than_treated_as_unset() {
        for bad in ["", "   ", "short", "0123456789abcde"] {
            assert!(
                Auth::from_env_value(Some(bad.to_string())).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn surrounding_whitespace_in_the_configured_token_is_ignored() {
        let auth = Auth::from_env_value(Some(format!("  {TOKEN}\r\n"))).unwrap();
        assert!(auth.permits(Some(&header(&format!("Bearer {TOKEN}")))));
    }

    #[test]
    fn exact_bearer_token_is_permitted_and_scheme_is_case_insensitive() {
        let auth = auth();
        assert!(auth.permits(Some(&header(&format!("Bearer {TOKEN}")))));
        assert!(auth.permits(Some(&header(&format!("bearer {TOKEN}")))));
    }

    #[test]
    fn missing_wrong_or_malformed_credentials_are_rejected() {
        let auth = auth();
        assert!(!auth.permits(None));
        assert!(!auth.permits(Some(&header("Bearer wrong-token-value-0000"))));
        assert!(!auth.permits(Some(&header(&format!("Bearer {TOKEN}x")))));
        assert!(!auth.permits(Some(&header(TOKEN))));
        assert!(!auth.permits(Some(&header(&format!("Basic {TOKEN}")))));
        assert!(!auth.permits(Some(&header("Bearer "))));
    }

    #[test]
    fn constant_time_eq_agrees_with_plain_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }
}
