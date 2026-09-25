//! Per-client token-bucket rate limiting, shared by WebSocket and REST.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

#[derive(Debug)]
pub struct RateLimiter {
    rate: f64,
    burst: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(requests_per_second: f64, burst: u32) -> Self {
        Self {
            rate: requests_per_second.max(0.001),
            burst: f64::from(burst.max(1)),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token for `key`; `false` means the request must be rejected.
    pub fn check(&self, key: &str) -> bool {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: &str, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        // Bound memory: client ids come from configuration, but be defensive.
        if buckets.len() > 10_000 {
            buckets.retain(|_, b| now.duration_since(b.last).as_secs() < 60);
        }
        let bucket = buckets.entry(key.to_owned()).or_insert(Bucket {
            tokens: self.burst,
            last: now,
        });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn allows_burst_then_refills() {
        let limiter = RateLimiter::new(10.0, 3);
        let t0 = Instant::now();
        assert!((0..3).all(|_| limiter.check_at("a", t0)));
        assert!(!limiter.check_at("a", t0));
        assert!(limiter.check_at("b", t0), "clients are independent");
        assert!(limiter.check_at("a", t0 + Duration::from_millis(150)));
    }
}
