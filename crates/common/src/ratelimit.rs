//! Shared in-process rate limiting.
//!
//! Ported from `api-gateway/services/ratelimit.py`. This is the cheap,
//! per-worker first-line damping layer; the authoritative cross-worker login
//! throttle counts failures out of `audit_log` instead.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Buckets are swept for expired keys at most once per window, and only when
/// the map has grown past this size, so the common case pays nothing.
const SWEEP_THRESHOLD: usize = 1024;

struct State {
    buckets: HashMap<String, Vec<Instant>>,
    last_sweep: Instant,
}

/// Allow `limit` events per `window`, per key.
pub struct SlidingWindowLimiter {
    limit: usize,
    window: Duration,
    name: String,
    state: Mutex<State>,
}

impl SlidingWindowLimiter {
    pub fn new(limit: usize, window: Duration, name: impl Into<String>) -> Self {
        Self {
            limit,
            window,
            name: name.into(),
            state: Mutex::new(State {
                buckets: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    /// A panic elsewhere while holding the lock must not turn every later
    /// request into a panic; the data is a plain map and stays usable.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record an attempt. `false` when the caller is over budget.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut state = self.lock();

        // Keys that stop attempting are otherwise never revisited, so a
        // stream of distinct IPs or usernames would grow the map forever.
        if state.buckets.len() > SWEEP_THRESHOLD
            && now.duration_since(state.last_sweep) >= self.window
        {
            let window = self.window;
            state
                .buckets
                .retain(|_, b| b.last().is_some_and(|t| now.duration_since(*t) < window));
            state.last_sweep = now;
        }

        let bucket = state.buckets.entry(key.to_string()).or_default();
        bucket.retain(|t| now.duration_since(*t) < self.window);
        if bucket.len() >= self.limit {
            drop(state);
            tracing::warn!("{}: over limit for '{key}'", self.name);
            return false;
        }
        bucket.push(now);
        true
    }

    /// Seconds until the oldest attempt in the window ages out.
    pub fn retry_after(&self, key: &str) -> u64 {
        let state = self.lock();
        let Some(bucket) = state.buckets.get(key).filter(|b| !b.is_empty()) else {
            return 0;
        };
        let elapsed = bucket[0].elapsed().as_secs();
        self.window.as_secs().saturating_sub(elapsed).max(1)
    }

    /// Clear a key. Called on a successful login so a legitimate user who
    /// mistyped twice is not still counting against the limit.
    pub fn reset(&self, key: &str) {
        self.lock().buckets.remove(key);
    }

    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.lock().buckets.len()
    }
}

impl Default for SlidingWindowLimiter {
    fn default() -> Self {
        Self::new(30, Duration::from_secs(60), "limiter")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_limit_per_key() {
        let l = SlidingWindowLimiter::new(2, Duration::from_secs(60), "t");
        assert!(l.allow("a"));
        assert!(l.allow("a"));
        assert!(!l.allow("a"));
        assert!(l.allow("b"));
        l.reset("a");
        assert!(l.allow("a"));
    }

    #[test]
    fn sweeps_expired_keys() {
        let l = SlidingWindowLimiter::new(5, Duration::from_millis(20), "t");
        for i in 0..(SWEEP_THRESHOLD + 10) {
            l.allow(&format!("k{i}"));
        }
        std::thread::sleep(Duration::from_millis(40));
        l.allow("fresh");
        assert_eq!(l.tracked_keys(), 1);
    }

    #[test]
    fn survives_a_poisoned_lock() {
        let l = std::sync::Arc::new(SlidingWindowLimiter::new(5, Duration::from_secs(60), "t"));
        let l2 = l.clone();
        let _ = std::thread::spawn(move || {
            let _guard = l2.state.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(l.allow("a"));
    }
}
