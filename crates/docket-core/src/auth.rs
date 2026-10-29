//! Shared-secret gate for the HTTP API (ADR-0026).
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

/// Shortest `DOCKET_TOKEN` the server accepts. A shorter secret is a typo or a
/// placeholder, and an open door that looks closed is worse than a refusal to
/// start.
pub const MIN_TOKEN_LEN: usize = 16;

/// Shortest `DOCKET_PASSWORD` the server accepts. Lower than the token's
/// because a person types it, which is the point of having a second secret;
/// the operator is expected to keep it random all the same.
pub const MIN_PASSWORD_LEN: usize = 8;

/// The secrets the API accepts, or none when auth is disabled.
///
/// Both kinds are bearer secrets with the same reach: the split exists so the
/// part a person types can be short and rotated apart from the one machines
/// carry, not to give either of them less power.
#[derive(Clone, Default)]
pub struct Auth(Vec<Vec<u8>>);

impl Auth {
    /// Builds the gate from the raw `DOCKET_TOKEN` and `DOCKET_PASSWORD` values.
    ///
    /// Neither set means auth is disabled. A value that is set but blank or too
    /// short is an error: failing closed at startup beats silently serving an
    /// API the operator believes is protected.
    pub fn from_env(token: Option<String>, password: Option<String>) -> Result<Self, String> {
        let mut secrets = Vec::new();
        if let Some(v) = token {
            secrets.push(accept(
                "DOCKET_TOKEN",
                &v,
                MIN_TOKEN_LEN,
                " (for example `openssl rand -hex 32`)",
            )?);
        }
        if let Some(v) = password {
            secrets.push(accept("DOCKET_PASSWORD", &v, MIN_PASSWORD_LEN, "")?);
        }
        Ok(Self(secrets))
    }

    pub fn is_enabled(&self) -> bool {
        !self.0.is_empty()
    }

    /// Whether `authorization` (a request's `Authorization` header) carries one
    /// of the configured secrets. Always true when auth is disabled.
    fn permits(&self, authorization: Option<&HeaderValue>) -> bool {
        if self.0.is_empty() {
            return true;
        }
        let Some(presented) = authorization
            .and_then(|v| v.to_str().ok())
            .and_then(bearer_token)
        else {
            return false;
        };
        // Every secret is compared, with no early exit on a match, so the time
        // taken does not say which one a guess was close to.
        self.0.iter().fold(false, |ok, expected| {
            constant_time_eq(presented.as_bytes(), expected) | ok
        })
    }
}

fn accept(name: &str, value: &str, min_len: usize, hint: &str) -> Result<Vec<u8>, String> {
    let value = value.trim();
    if value.len() < min_len {
        return Err(format!(
            "{name} is set but shorter than {min_len} characters; unset it to run without it, \
             or use a longer secret{hint}"
        ));
    }
    Ok(value.as_bytes().to_vec())
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
/// time does not reveal how much of a guessed secret was right. A length
/// mismatch still returns early; the length of a secret is not itself secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Rejects a request that does not carry a valid secret with `401`.
///
/// Layered onto the API routes only. The console's static shell, its assets
/// and `/status` carry no item data and stay reachable, which is what lets a
/// browser load the console before it has a secret to send.
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
            error: "unauthorized: send `Authorization: Bearer <DOCKET_TOKEN or DOCKET_PASSWORD>`"
                .to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123";
    const PASSWORD: &str = "k7m2xq9t";

    fn auth() -> Auth {
        Auth::from_env(Some(TOKEN.to_string()), None).unwrap()
    }

    fn header(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).unwrap()
    }

    #[test]
    fn nothing_set_disables_auth_and_permits_everything() {
        let auth = Auth::from_env(None, None).unwrap();
        assert!(!auth.is_enabled());
        assert!(auth.permits(None));
    }

    #[test]
    fn blank_or_short_token_is_refused_rather_than_treated_as_unset() {
        for bad in ["", "   ", "short", "0123456789abcde"] {
            assert!(
                Auth::from_env(Some(bad.to_string()), None).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn blank_or_short_password_is_refused_but_a_short_one_is_fine_as_long_as_it_meets_the_floor() {
        for bad in ["", "  ", "abc", "1234567"] {
            assert!(
                Auth::from_env(None, Some(bad.to_string())).is_err(),
                "{bad:?} must be refused"
            );
        }
        assert!(Auth::from_env(None, Some(PASSWORD.to_string())).is_ok());
        // The token floor is not relaxed by the password's.
        assert!(Auth::from_env(Some(PASSWORD.to_string()), None).is_err());
    }

    #[test]
    fn surrounding_whitespace_in_a_configured_secret_is_ignored() {
        let auth = Auth::from_env(Some(format!("  {TOKEN}\r\n")), None).unwrap();
        assert!(auth.permits(Some(&header(&format!("Bearer {TOKEN}")))));
    }

    #[test]
    fn exact_bearer_token_is_permitted_and_scheme_is_case_insensitive() {
        let auth = auth();
        assert!(auth.permits(Some(&header(&format!("Bearer {TOKEN}")))));
        assert!(auth.permits(Some(&header(&format!("bearer {TOKEN}")))));
    }

    #[test]
    fn either_configured_secret_is_permitted_and_neither_unlocks_the_other_alone() {
        let both = Auth::from_env(Some(TOKEN.to_string()), Some(PASSWORD.to_string())).unwrap();
        assert!(both.permits(Some(&header(&format!("Bearer {TOKEN}")))));
        assert!(both.permits(Some(&header(&format!("Bearer {PASSWORD}")))));
        assert!(!both.permits(Some(&header("Bearer neither-of-them-0000"))));

        // A password-only server does not accept the (unconfigured) token.
        let only_password = Auth::from_env(None, Some(PASSWORD.to_string())).unwrap();
        assert!(only_password.is_enabled());
        assert!(only_password.permits(Some(&header(&format!("Bearer {PASSWORD}")))));
        assert!(!only_password.permits(Some(&header(&format!("Bearer {TOKEN}")))));
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
