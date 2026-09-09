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
