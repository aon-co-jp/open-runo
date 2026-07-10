//! `open-runo-security`: authentication, authorization, API key handling,
//! and rate limiting for the open-runo gateway (see README section 10).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod oidc;
pub mod rbac;

use chrono::{DateTime, Utc};
use open_runo_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub key: String,
    pub owner: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: bool,
}

impl ApiKey {
    /// Validates the key against the current time, returning a descriptive
    /// error for expired/revoked keys instead of a bare boolean so callers
    /// can log/return a helpful message.
    pub fn validate(&self, now: DateTime<Utc>) -> Result<()> {
        if self.revoked {
            return Err(AppError::Validation(format!("API key for '{}' has been revoked", self.owner)));
        }
        if let Some(expires_at) = self.expires_at {
            if now >= expires_at {
                return Err(AppError::Validation(format!("API key for '{}' expired at {expires_at}", self.owner)));
            }
        }
        Ok(())
    }
}

/// A simple fixed-window rate limiter: `max_requests` per `window` per key.
/// Sufficient for Phase 1; a token-bucket/leaky-bucket limiter can replace
/// this without changing the [`RateLimiter::check`] call site.
#[derive(Debug)]
pub struct RateLimiter {
    max_requests: u32,
    window: chrono::Duration,
    windows: Mutex<HashMap<String, (DateTime<Utc>, u32)>>,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window: chrono::Duration) -> Self {
        Self {
            max_requests,
            window,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `Ok(())` if `key` is still within its rate limit at `now`,
    /// otherwise `Err(AppError::Validation(..))`. Also records the request.
    pub fn check(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let entry = windows.entry(key.to_string()).or_insert((now, 0));
        if now.signed_duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }

        if entry.1 >= self.max_requests {
            return Err(AppError::Validation(format!(
                "rate limit exceeded for '{key}': {} requests per {:?}",
                self.max_requests, self.window
            )));
        }

        entry.1 += 1;
        Ok(())
    }

    /// Whole seconds until `key`'s current window resets, for a
    /// `Retry-After` response header when [`Self::check`] rejects a
    /// request. `0` if `key` has no active window (the next request would
    /// start a fresh one and succeed immediately).
    pub fn seconds_until_reset(&self, key: &str, now: DateTime<Utc>) -> i64 {
        let windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match windows.get(key) {
            Some((window_start, _)) => (self.window - now.signed_duration_since(*window_start))
                .num_seconds()
                .max(0),
            None => 0,
        }
    }
}


/// Fine-grained token-bucket rate limiter (Cosmo paid-tier parity).
///
/// Each key gets a bucket of `capacity` tokens refilled continuously at
/// `refill_per_sec`. One token is consumed per request, so short bursts up
/// to `capacity` are allowed while the sustained rate converges to
/// `refill_per_sec`. Per-key overrides support different budgets per
/// API key / operation (`with_override`).
#[derive(Debug)]
pub struct TokenBucketLimiter {
    capacity: f64,
    refill_per_sec: f64,
    overrides: HashMap<String, (f64, f64)>,
    buckets: Mutex<HashMap<String, (f64, DateTime<Utc>)>>,
}

impl TokenBucketLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            capacity: f64::from(capacity),
            refill_per_sec,
            overrides: HashMap::new(),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Give `key` its own budget, independent of the default.
    #[must_use]
    pub fn with_override(mut self, key: impl Into<String>, capacity: u32, refill_per_sec: f64) -> Self {
        self.overrides.insert(key.into(), (f64::from(capacity), refill_per_sec));
        self
    }

    fn budget(&self, key: &str) -> (f64, f64) {
        self.overrides
            .get(key)
            .copied()
            .unwrap_or((self.capacity, self.refill_per_sec))
    }

    /// Consume one token for `key` at `now`. Returns
    /// `Err(AppError::Validation(..))` when the bucket is empty.
    pub fn try_acquire(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        let (capacity, refill) = self.budget(key);
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let (tokens, last) = buckets
            .get(key)
            .copied()
            .unwrap_or((capacity, now));

        let elapsed_secs = now.signed_duration_since(last).num_milliseconds() as f64 / 1000.0;
        let tokens = (tokens + elapsed_secs.max(0.0) * refill).min(capacity);

        if tokens < 1.0 {
            return Err(AppError::Validation(format!(
                "rate limit exceeded for '{key}': bucket empty (capacity {capacity}, {refill}/s)"
            )));
        }

        buckets.insert(key.to_string(), (tokens - 1.0, now));
        Ok(())
    }
}

// ── JWT ────────────────────────────────────────────────────────────────────

/// Claims embedded in an open-runo-issued JWT.
///
/// `sub` identifies the caller (service name or user id); `exp` is a Unix
/// timestamp (seconds) enforced by `jsonwebtoken`'s default validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Encodes and verifies HS256 JWTs for the `Authorization: Bearer <token>`
/// auth path (an alternative to `X-Api-Key`; see `open-runo-router::auth`).
#[derive(Debug, Clone)]
pub struct JwtCodec {
    secret: Vec<u8>,
}

impl JwtCodec {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self { secret: secret.into() }
    }

    /// Load the signing secret from `OPEN_RUNO_JWT_SECRET`. Returns `None`
    /// (JWT auth disabled) if the variable is unset — deployments that only
    /// want `X-Api-Key` auth do not need to set it.
    pub fn from_env() -> Option<Self> {
        std::env::var("OPEN_RUNO_JWT_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .map(Self::new)
    }

    /// Issue a signed token for `subject`, valid for `ttl` from now.
    pub fn encode(&self, subject: &str, roles: Vec<String>, ttl: chrono::Duration) -> Result<String> {
        let claims = Claims {
            sub: subject.to_string(),
            exp: (Utc::now() + ttl).timestamp(),
            roles,
        };
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(&self.secret),
        )
        .map_err(|e| AppError::Validation(format!("jwt encode failed: {e}")))
    }

    /// Verify a bearer token's signature and expiry, returning its claims.
    ///
    /// `leeway` is set to 0 so an expired token is rejected immediately
    /// (jsonwebtoken defaults to 60 s of clock-skew tolerance, which made
    /// short-lived tokens outlive their `exp`).
    pub fn decode(&self, token: &str) -> Result<Claims> {
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.leeway = 0;
        jsonwebtoken::decode::<Claims>(
            token,
            &jsonwebtoken::DecodingKey::from_secret(&self.secret),
            &validation,
        )
        .map(|data| data.claims)
        .map_err(|e| AppError::Validation(format!("invalid or expired token: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn rejects_revoked_key() {
        let key = ApiKey { key: "k".into(), owner: "alice".into(), expires_at: None, revoked: true };
        assert!(key.validate(Utc::now()).is_err());
    }

    #[test]
    fn rejects_expired_key() {
        let key = ApiKey {
            key: "k".into(),
            owner: "alice".into(),
            expires_at: Some(Utc::now() - Duration::seconds(1)),
            revoked: false,
        };
        assert!(key.validate(Utc::now()).is_err());
    }

    #[test]
    fn accepts_valid_key() {
        let key = ApiKey {
            key: "k".into(),
            owner: "alice".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            revoked: false,
        };
        assert!(key.validate(Utc::now()).is_ok());
    }

    #[test]
    fn rate_limiter_blocks_after_max_requests() {
        let limiter = RateLimiter::new(2, Duration::minutes(1));
        let now = Utc::now();
        assert!(limiter.check("client-a", now).is_ok());
        assert!(limiter.check("client-a", now).is_ok());
        assert!(limiter.check("client-a", now).is_err());
    }

    #[test]
    fn rate_limiter_resets_after_window() {
        let limiter = RateLimiter::new(1, Duration::seconds(1));
        let t0 = Utc::now();
        assert!(limiter.check("client-b", t0).is_ok());
        assert!(limiter.check("client-b", t0).is_err());
        let t1 = t0 + Duration::seconds(2);
        assert!(limiter.check("client-b", t1).is_ok());
    }

    #[test]
    fn seconds_until_reset_counts_down_within_the_window() {
        let limiter = RateLimiter::new(1, Duration::seconds(10));
        let t0 = Utc::now();
        assert!(limiter.check("client-c", t0).is_ok());
        assert_eq!(limiter.seconds_until_reset("client-c", t0), 10);
        assert_eq!(limiter.seconds_until_reset("client-c", t0 + Duration::seconds(4)), 6);
    }

    #[test]
    fn seconds_until_reset_is_zero_for_an_unseen_key() {
        let limiter = RateLimiter::new(1, Duration::seconds(10));
        assert_eq!(limiter.seconds_until_reset("never-seen", Utc::now()), 0);
    }

    #[test]
    fn jwt_roundtrip() {
        let codec = JwtCodec::new("test-secret");
        let token = codec
            .encode("alice", vec!["admin".into()], Duration::hours(1))
            .unwrap();
        let claims = codec.decode(&token).unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(claims.roles, vec!["admin".to_string()]);
    }

    #[test]
    fn jwt_rejects_expired_token() {
        let codec = JwtCodec::new("test-secret");
        let token = codec.encode("alice", vec![], Duration::seconds(-1)).unwrap();
        assert!(codec.decode(&token).is_err());
    }

    #[test]
    fn jwt_rejects_wrong_secret() {
        let codec_a = JwtCodec::new("secret-a");
        let codec_b = JwtCodec::new("secret-b");
        let token = codec_a.encode("alice", vec![], Duration::hours(1)).unwrap();
        assert!(codec_b.decode(&token).is_err());
    }

    // ── TokenBucketLimiter ─────────────────────────────────────────────

    #[test]
    fn token_bucket_allows_burst_up_to_capacity() {
        let limiter = TokenBucketLimiter::new(3, 1.0);
        let now = Utc::now();
        assert!(limiter.try_acquire("k", now).is_ok());
        assert!(limiter.try_acquire("k", now).is_ok());
        assert!(limiter.try_acquire("k", now).is_ok());
        assert!(limiter.try_acquire("k", now).is_err());
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let limiter = TokenBucketLimiter::new(1, 1.0); // 1 token/s
        let t0 = Utc::now();
        assert!(limiter.try_acquire("k", t0).is_ok());
        assert!(limiter.try_acquire("k", t0).is_err());
        let t1 = t0 + Duration::milliseconds(1500);
        assert!(limiter.try_acquire("k", t1).is_ok());
    }

    #[test]
    fn token_bucket_keys_are_independent() {
        let limiter = TokenBucketLimiter::new(1, 0.001);
        let now = Utc::now();
        assert!(limiter.try_acquire("a", now).is_ok());
        assert!(limiter.try_acquire("b", now).is_ok());
        assert!(limiter.try_acquire("a", now).is_err());
    }

    #[test]
    fn token_bucket_override_gives_key_its_own_budget() {
        let limiter = TokenBucketLimiter::new(1, 0.001).with_override("vip", 3, 0.001);
        let now = Utc::now();
        assert!(limiter.try_acquire("vip", now).is_ok());
        assert!(limiter.try_acquire("vip", now).is_ok());
        assert!(limiter.try_acquire("vip", now).is_ok());
        assert!(limiter.try_acquire("vip", now).is_err());
        assert!(limiter.try_acquire("normal", now).is_ok());
        assert!(limiter.try_acquire("normal", now).is_err());
    }
}
