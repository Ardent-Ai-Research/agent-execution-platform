//! Per-API-key rate limiting using a token bucket algorithm.
//!
//! Each API key gets its own bucket that refills at `rate` tokens per second
//! up to a maximum of `burst` tokens.  When a request arrives, one token is
//! consumed.  If the bucket is empty the request is rejected with `429 Too
//! Many Requests` and a `Retry-After` header indicating how long until a
//! token becomes available.
//!
//! The implementation is fully in-memory (no Redis dependency) and uses
//! [`DashMap`] for lock-free concurrent access across Tokio tasks.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::IntoResponse,
};
use dashmap::DashMap;
use uuid::Uuid;

use crate::types::ApiKeyContext;

// ──────────────────────── Token Bucket ───────────────────────────────

/// State for a single API key's token bucket.
struct Bucket {
    /// Number of tokens currently available (can be fractional due to refill).
    tokens: f64,
    /// Last time the bucket was checked / refilled.
    last_refill: Instant,
}

/// Shared rate limiter state, keyed by `api_key_id`.
///
/// Cheaply cloneable (wraps an `Arc`).
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateLimiterInner>,
}

struct RateLimiterInner {
    buckets: DashMap<Uuid, Bucket>,
    /// Tokens added per second (sustained rate).
    rate: f64,
    /// Maximum tokens a bucket can hold (burst capacity).
    burst: f64,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// * `rate`  — requests per second each API key is allowed (sustained).
    /// * `burst` — maximum burst size (peak requests before throttling kicks in).
    ///
    /// Example: `rate = 10.0, burst = 30` means a key can send 30 requests
    /// instantly, then is limited to 10/s until the bucket refills.
    pub fn new(rate: f64, burst: f64) -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                buckets: DashMap::new(),
                rate: rate.max(0.1),   // floor at 0.1 rps to avoid div-by-zero
                burst: burst.max(1.0), // at least 1 token
            }),
        }
    }

    /// Try to consume one token for `key`.
    ///
    /// Returns `Ok(())` if allowed, or `Err(retry_after_secs)` if the
    /// bucket is empty.
    pub fn check(&self, key: Uuid) -> Result<(), f64> {
        let now = Instant::now();
        let inner = &self.inner;

        let mut entry = inner.buckets.entry(key).or_insert_with(|| Bucket {
            tokens: inner.burst,
            last_refill: now,
        });

        let bucket = entry.value_mut();

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * inner.rate).min(inner.burst);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            // How long until 1 token is available?
            let deficit = 1.0 - bucket.tokens;
            let retry_after = deficit / inner.rate;
            Err(retry_after)
        }
    }

    /// Remove stale buckets that haven't been used in a while.
    ///
    /// Call this periodically (e.g. every 60 s) to prevent unbounded memory
    /// growth from one-off API keys.  A bucket is considered stale if it
    /// has been idle long enough to have fully refilled (i.e. no activity
    /// for `burst / rate` seconds).
    pub fn evict_stale(&self) {
        let now = Instant::now();
        let inner = &self.inner;
        let stale_threshold_secs = (inner.burst / inner.rate) + 60.0; // full refill + 1 min grace

        inner.buckets.retain(|_key, bucket| {
            let idle_secs = now.duration_since(bucket.last_refill).as_secs_f64();
            idle_secs < stale_threshold_secs
        });
    }
}

// ──────────────────────── Axum Middleware ─────────────────────────────

/// Axum middleware function that enforces per-API-key rate limits.
///
/// Must be applied **after** the API key auth middleware (so that
/// `ApiKeyContext` is present in the request extensions).
pub async fn rate_limit_middleware(
    State(limiter): State<RateLimiter>,
    req: Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    // Extract the API key identity set by the auth middleware.
    // If missing (shouldn't happen — auth middleware runs first), let through
    // and rely on downstream handlers to reject.
    let api_key_id = req
        .extensions()
        .get::<ApiKeyContext>()
        .map(|ctx| ctx.api_key_id);

    let Some(key_id) = api_key_id else {
        return next.run(req).await.into_response();
    };

    match limiter.check(key_id) {
        Ok(()) => next.run(req).await.into_response(),
        Err(retry_after) => {
            let retry_secs = retry_after.ceil() as u64;
            tracing::warn!(
                api_key_id = %key_id,
                retry_after_secs = retry_secs,
                "rate limited"
            );
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", retry_secs.to_string())],
                axum::Json(serde_json::json!({
                    "error": "rate_limit_exceeded",
                    "message": "Too many requests — please slow down.",
                    "retry_after_secs": retry_secs,
                })),
            )
                .into_response()
        }
    }
}
