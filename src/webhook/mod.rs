//! Webhook delivery — pushes execution results to agent-supplied callback URLs.
//!
//! When an agent includes a `callback_url` in their `/execute` request, the
//! platform POSTs the final status to that URL once the transaction reaches a
//! terminal state (Confirmed, Failed, Reverted).
//!
//! ## Security
//!
//! Every webhook request carries an `X-Webhook-Signature` header containing
//! an HMAC-SHA256 signature of the JSON body, keyed with the API key's hash.
//! This lets agents verify that the webhook came from the platform (and not
//! a spoofed source) without exposing the raw API key.
//!
//! ## Reliability
//!
//! Delivery uses exponential backoff with up to [`MAX_RETRIES`] attempts.
//! If all retries fail the webhook is logged as undeliverable but does **not**
//! block the main execution flow — the agent can always fall back to polling
//! `/status/{id}`.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::Serialize;
use sha2::Sha256;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::types::ExecutionStatus;

// ──────────────────────── Constants ──────────────────────────────────

/// Maximum number of delivery attempts (initial + retries).
const MAX_RETRIES: u32 = 3;

/// Initial backoff before the first retry.
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);

/// Maximum time to wait for a webhook endpoint to respond.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum time to wait for connection establishment.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

// ──────────────────────── Payload ────────────────────────────────────

/// The JSON body POSTed to the agent's callback URL.
///
/// Mirrors `StatusResponse` so agents use the same deserialization logic
/// whether they poll `/status/{id}` or receive a webhook push.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    /// Unique event identifier for idempotency on the receiver side.
    pub event_id: Uuid,
    /// Always `"execution.completed"` for now — room for more event types later.
    pub event_type: String,
    /// The execution request ID.
    pub request_id: Uuid,
    /// Terminal status: confirmed, failed, or reverted.
    pub status: ExecutionStatus,
    /// The blockchain the transaction was executed on.
    pub chain: String,
    /// On-chain transaction hash (present for confirmed / reverted).
    pub tx_hash: Option<String>,
    /// Final gas cost in USD (if available).
    pub cost_usd: Option<f64>,
    /// Error message (if failed / reverted).
    pub error: Option<String>,
    /// When the execution request was originally created.
    pub created_at: DateTime<Utc>,
    /// When the terminal status was reached.
    pub completed_at: DateTime<Utc>,
}

// ──────────────────────── Delivery ───────────────────────────────────

/// Build a shared `reqwest::Client` suitable for webhook delivery.
///
/// Callers should keep one instance and pass it around (connection pooling).
pub fn build_http_client() -> Client {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none()) // Don't follow redirects
        .user_agent("agent-execution-platform/webhook")
        .build()
        .expect("failed to build webhook HTTP client")
}

/// Deliver a webhook payload to the callback URL with retries.
///
/// `signing_secret` is the SHA-256 hash of the API key (stored in the DB as
/// `api_keys.key_hash`).  Agents know their raw API key, so they can derive
/// the same hash and verify the HMAC signature.
///
/// Returns `true` if delivery succeeded (2xx), `false` if all attempts failed.
pub async fn deliver(
    client: &Client,
    callback_url: &str,
    payload: &WebhookPayload,
    signing_secret: &str,
) -> bool {
    let body = match serde_json::to_string(payload) {
        Ok(b) => b,
        Err(e) => {
            error!(
                request_id = %payload.request_id,
                error = %e,
                "failed to serialize webhook payload"
            );
            return false;
        }
    };

    // Compute HMAC-SHA256 signature
    let signature = compute_signature(&body, signing_secret);

    let mut backoff = INITIAL_BACKOFF;

    for attempt in 1..=MAX_RETRIES {
        match client
            .post(callback_url)
            .header("Content-Type", "application/json")
            .header("X-Webhook-Signature", &signature)
            .header("X-Webhook-Event", &payload.event_type)
            .header("X-Webhook-Id", payload.event_id.to_string())
            .body(body.clone())
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    info!(
                        request_id = %payload.request_id,
                        callback_url,
                        attempt,
                        status = %status,
                        "webhook delivered successfully"
                    );
                    return true;
                }

                // Non-2xx — treat as failure and retry
                warn!(
                    request_id = %payload.request_id,
                    callback_url,
                    attempt,
                    status = %status,
                    "webhook endpoint returned non-success status"
                );
            }
            Err(e) => {
                warn!(
                    request_id = %payload.request_id,
                    callback_url,
                    attempt,
                    error = %e,
                    "webhook delivery failed"
                );
            }
        }

        if attempt < MAX_RETRIES {
            tokio::time::sleep(backoff).await;
            backoff *= 2; // exponential backoff: 2s → 4s → 8s
        }
    }

    error!(
        request_id = %payload.request_id,
        callback_url,
        max_retries = MAX_RETRIES,
        "webhook delivery exhausted all retries"
    );
    false
}

// ──────────────────────── HMAC Signing ───────────────────────────────

/// Compute `HMAC-SHA256(body, secret)` and return as a hex string.
fn compute_signature(body: &str, secret: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    fn sample_payload() -> WebhookPayload {
        WebhookPayload {
            event_id: Uuid::new_v4(),
            event_type: "execution.completed".into(),
            request_id: Uuid::new_v4(),
            status: ExecutionStatus::Confirmed,
            chain: "ethereum".into(),
            tx_hash: Some("0xabc".into()),
            cost_usd: Some(1.23),
            error: None,
            created_at: Utc::now(),
            completed_at: Utc::now(),
        }
    }

    #[test]
    fn test_signature_deterministic() {
        let sig1 = compute_signature(r#"{"hello":"world"}"#, "secret123");
        let sig2 = compute_signature(r#"{"hello":"world"}"#, "secret123");
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_signature_changes_with_different_secret() {
        let sig1 = compute_signature(r#"{"hello":"world"}"#, "secret_a");
        let sig2 = compute_signature(r#"{"hello":"world"}"#, "secret_b");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn test_signature_changes_with_different_body() {
        let sig1 = compute_signature(r#"{"a":1}"#, "secret");
        let sig2 = compute_signature(r#"{"a":2}"#, "secret");
        assert_ne!(sig1, sig2);
    }

    #[tokio::test]
    async fn test_deliver_sends_post_with_hmac_header() {
        let hit_count = Arc::new(AtomicUsize::new(0));
        let captured_sig: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        async fn handler(
            State((hits, sig_store)): State<(Arc<AtomicUsize>, Arc<Mutex<Option<String>>>)>,
            headers: HeaderMap,
        ) -> StatusCode {
            hits.fetch_add(1, Ordering::SeqCst);
            let sig = headers
                .get("x-webhook-signature")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);
            *sig_store.lock().await = sig;
            StatusCode::OK
        }

        let app = Router::new()
            .route("/", post(handler))
            .with_state((Arc::clone(&hit_count), Arc::clone(&captured_sig)));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let client = build_http_client();
        let payload = sample_payload();
        let secret = "test-secret";

        let delivered = deliver(
            &client,
            &format!("http://{addr}"),
            &payload,
            secret,
        )
        .await;

        assert!(delivered);
        assert_eq!(hit_count.load(Ordering::SeqCst), 1);

        let body = serde_json::to_string(&payload).expect("payload json");
        let expected_sig = compute_signature(&body, secret);
        assert_eq!(captured_sig.lock().await.as_deref(), Some(expected_sig.as_str()));

        server.abort();
    }

    #[tokio::test]
    async fn test_deliver_retries_on_5xx_then_succeeds() {
        let hit_count = Arc::new(AtomicUsize::new(0));

        async fn handler(State(hits): State<Arc<AtomicUsize>>) -> StatusCode {
            let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
            if n < 3 {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::OK
            }
        }

        let app = Router::new()
            .route("/", post(handler))
            .with_state(Arc::clone(&hit_count));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let client = build_http_client();
        let payload = sample_payload();

        let delivered = deliver(
            &client,
            &format!("http://{addr}"),
            &payload,
            "retry-secret",
        )
        .await;

        assert!(delivered);
        assert_eq!(hit_count.load(Ordering::SeqCst), 3);

        server.abort();
    }
}
