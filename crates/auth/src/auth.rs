//! Authentication: JWT issue/verify (HS256) and bcrypt password hashing.
//!
//! Compatibility note: existing database rows store **bcrypt** password hashes
//! (written by the legacy Python `seed_admin.py` and `users` routes), so this
//! module uses bcrypt to remain able to verify those hashes.

use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{Duration as ChronoDuration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Claims {
    pub sub: String,
    pub username: String,
    pub role: String,
    pub iat: i64,
    pub exp: i64,
    pub scope: String,
}

/// Create a new JWT for the given principal.
pub fn create_token(
    user_id: &str,
    username: &str,
    role: &str,
    scope: &str,
    secret: &str,
    expire_minutes: i64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = Utc::now();
    let claims = Claims {
        sub: user_id.to_string(),
        username: username.to_string(),
        role: role.to_string(),
        iat: now.timestamp(),
        exp: (now + ChronoDuration::minutes(expire_minutes)).timestamp(),
        scope: scope.to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

/// Decode and validate a JWT. Returns the claims on success.
pub fn decode_token(token: &str, secret: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let data = decode(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )?;
    Ok(data.claims)
}

/// Hash a plaintext password with bcrypt.
pub fn hash_password(password: &str) -> Result<String, bcrypt::BcryptError> {
    Ok(hash(password, DEFAULT_COST)?.to_string())
}

/// Verify a plaintext password against a stored bcrypt hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    verify(password, hash).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_token_round_trips_its_claims() {
        let token = create_token(
            "2d809714-2f0a-4d20-8dc7-1daab697ce8d",
            "billing.user",
            "billing_specialist",
            "full",
            "test-signing-secret-with-sufficient-length",
            30,
        )
        .unwrap();

        let claims = decode_token(&token, "test-signing-secret-with-sufficient-length").unwrap();
        assert_eq!(claims.username, "billing.user");
        assert_eq!(claims.role, "billing_specialist");
    }

    #[test]
    fn password_hashes_do_not_accept_a_different_password() {
        let password_hash = hash_password("correct horse battery staple").unwrap();

        assert!(verify_password(
            "correct horse battery staple",
            &password_hash
        ));
        assert!(!verify_password("incorrect", &password_hash));
    }
}
