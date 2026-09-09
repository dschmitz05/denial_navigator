//! Shared in-process rate limiting.
//!
//! Ported from `api-gateway/services/ratelimit.py`. This is the cheap,
//! per-worker first-line damping layer; the authoritative cross-worker login
//! throttle counts failures out of `audit_log` instead.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Allow `limit` events per `window`, per key.
pub struct SlidingWindowLimiter {
    limit: usize,
    window: Duration,
    name: String,
    buckets: Mutex<HashMap<String, Vec<Instant>>>,
}

impl SlidingWindowLimiter {
    pub fn new(limit: usize, window: Duration, name: impl Into<String>) -> Self {
        Self {
            limit,
            window,
            name: name.into(),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Record an attempt. `false` when the caller is over budget.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut map = self.buckets.lock().unwrap();
        let bucket = map.entry(key.to_string()).or_default();
        bucket.retain(|t| now.duration_since(*t) < self.window);
        if bucket.len() >= self.limit {
            if bucket.is_empty() {
                map.remove(key);
            }
            drop(map);
            tracing::warn!("{}: over limit for '{key}'", self.name);
            return false;
        }
        bucket.push(now);
        true
    }

    /// Seconds until the oldest attempt in the window ages out.
    pub fn retry_after(&self, key: &str) -> u64 {
        let map = self.buckets.lock().unwrap();
        let Some(bucket) = map.get(key).filter(|b| !b.is_empty()) else {
            return 0;
        };
        let elapsed = bucket[0].elapsed().as_secs();
        self.window.as_secs().saturating_sub(elapsed).max(1)
    }

    /// Clear a key. Called on a successful login so a legitimate user who
    /// mistyped twice is not still counting against the limit.
    pub fn reset(&self, key: &str) {
        self.buckets.lock().unwrap().remove(key);
    }
}

impl Default for SlidingWindowLimiter {
    fn default() -> Self {
        Self::new(30, Duration::from_secs(60), "limiter")
    }
}
