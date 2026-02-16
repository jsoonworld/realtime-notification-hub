use std::future::Future;
use std::pin::Pin;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::error::AppError;
use crate::state::AppState;

use super::jwt::{decode_access_token, Claims};

pub struct AuthUser(pub Claims);

impl AuthUser {
    pub fn user_id(&self) -> Result<i64, AppError> {
        self.0
            .sub
            .parse::<i64>()
            .map_err(|_| AppError::Unauthorized("유효하지 않은 user_id".to_string()))
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    fn from_request_parts<'life0, 'life1, 'async_trait>(
        parts: &'life0 mut Parts,
        state: &'life1 AppState,
    ) -> Pin<Box<dyn Future<Output = Result<Self, Self::Rejection>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let secret = &state.config.jwt_secret;

            // 1) Try Authorization: Bearer <token>
            if let Some(auth_header) = parts.headers.get(axum::http::header::AUTHORIZATION) {
                if let Ok(header_value) = auth_header.to_str() {
                    if let Some(token) = header_value.strip_prefix("Bearer ") {
                        let claims = decode_access_token(token, secret)?;
                        return Ok(AuthUser(claims));
                    }
                }
            }

            // 2) Fallback: access_token cookie
            if let Some(cookie_header) = parts.headers.get(axum::http::header::COOKIE) {
                if let Ok(cookie_str) = cookie_header.to_str() {
                    if let Some(token) = extract_token_from_cookie(cookie_str) {
                        let claims = decode_access_token(token, secret)?;
                        return Ok(AuthUser(claims));
                    }
                }
            }

            Err(AppError::Unauthorized(
                "인증 토큰이 없습니다".to_string(),
            ))
        })
    }
}

fn extract_token_from_cookie(cookie_str: &str) -> Option<&str> {
    for pair in cookie_str.split(';') {
        let trimmed = pair.trim();
        if let Some(value) = trimmed.strip_prefix("access_token=") {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_token_from_cookie_found() {
        let cookie = "session=abc; access_token=my_jwt_token; other=value";
        assert_eq!(extract_token_from_cookie(cookie), Some("my_jwt_token"));
    }

    #[test]
    fn test_extract_token_from_cookie_not_found() {
        let cookie = "session=abc; other=value";
        assert_eq!(extract_token_from_cookie(cookie), None);
    }

    #[test]
    fn test_extract_token_from_cookie_empty_value() {
        let cookie = "access_token=; other=value";
        assert_eq!(extract_token_from_cookie(cookie), None);
    }

    #[test]
    fn test_extract_token_from_cookie_only_token() {
        let cookie = "access_token=token123";
        assert_eq!(extract_token_from_cookie(cookie), Some("token123"));
    }
}
