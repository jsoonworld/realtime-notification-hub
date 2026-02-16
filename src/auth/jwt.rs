use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    pub iat: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

pub fn decode_access_token(token: &str, secret: &str) -> Result<Claims, AppError> {
    let key = DecodingKey::from_secret(secret.as_bytes());
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    let token_data = decode::<Claims>(token, &key, &validation)
        .map_err(|e| AppError::Unauthorized(format!("JWT 검증 실패: {}", e)))?;

    Ok(token_data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    const TEST_SECRET: &str = "test_secret_key_for_unit_tests";

    fn create_test_token(claims: &Claims) -> String {
        let header = Header::new(Algorithm::HS256);
        let key = EncodingKey::from_secret(TEST_SECRET.as_bytes());
        encode(&header, claims, &key).expect("failed to encode test token")
    }

    fn valid_claims() -> Claims {
        Claims {
            sub: "42".to_string(),
            exp: (chrono::Utc::now().timestamp() + 3600) as usize,
            iat: chrono::Utc::now().timestamp() as usize,
            jti: None,
            email: Some("test@example.com".to_string()),
            token_type: Some("access".to_string()),
            provider: None,
        }
    }

    #[test]
    fn test_decode_valid_token() {
        let claims = valid_claims();
        let token = create_test_token(&claims);

        let decoded = decode_access_token(&token, TEST_SECRET).unwrap();
        assert_eq!(decoded.sub, "42");
        assert_eq!(decoded.email, Some("test@example.com".to_string()));
        assert_eq!(decoded.token_type, Some("access".to_string()));
    }

    #[test]
    fn test_decode_expired_token() {
        let claims = Claims {
            exp: (chrono::Utc::now().timestamp() - 3600) as usize,
            ..valid_claims()
        };
        let token = create_test_token(&claims);

        let result = decode_access_token(&token, TEST_SECRET);
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::Unauthorized(msg) => assert!(msg.contains("JWT")),
            other => panic!("Expected Unauthorized, got: {:?}", other),
        }
    }

    #[test]
    fn test_decode_invalid_signature() {
        let claims = valid_claims();
        let token = create_test_token(&claims);

        let result = decode_access_token(&token, "wrong_secret");
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::Unauthorized(msg) => assert!(msg.contains("JWT")),
            other => panic!("Expected Unauthorized, got: {:?}", other),
        }
    }

    #[test]
    fn test_decode_empty_string() {
        let result = decode_access_token("", TEST_SECRET);
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::Unauthorized(_) => {}
            other => panic!("Expected Unauthorized, got: {:?}", other),
        }
    }

    #[test]
    fn test_decode_malformed_token() {
        let result = decode_access_token("not.a.valid.jwt.token", TEST_SECRET);
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::Unauthorized(_) => {}
            other => panic!("Expected Unauthorized, got: {:?}", other),
        }
    }
}
