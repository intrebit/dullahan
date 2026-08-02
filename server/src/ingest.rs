//! `/collect` ingest — validate the event, then hand it to the writer task.
//!
//! Events are queued on a bounded channel and written in batches by a single
//! task ([`spawn_writer`]). The queue is the backpressure: a task-per-event
//! design against a 10-connection pool turns overload into acquire timeouts and
//! silently lost rows, whereas a full queue is something the handler can *tell
//! the client about* — and something a counter can alert on before rows are lost.

use crate::db::{self, PendingEvent};
use crate::state::AppState;
use crate::types::RawPayload;
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use sqlx::PgPool;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Events per INSERT. Times `db`'s 15 columns this is 1920 bind parameters,
/// comfortably inside Postgres' 65535 ceiling.
pub const BATCH_MAX: usize = 128;

/// Queue depth. Sized to absorb a burst several times the rate limiter's own
/// allowance while still being a hard bound: past this the handler sheds load
/// instead of letting the backlog grow until the process is OOM-killed.
pub const QUEUE_CAPACITY: usize = 10_000;

/// Handle used by request handlers to enqueue an event.
pub type IngestSender = mpsc::Sender<PendingEvent>;

/// Start the writer task, returning the sender for [`AppState`] and a handle to
/// await at shutdown.
///
/// Awaiting the handle after the server stops accepting connections is what
/// makes a restart lossless: the task drains whatever is queued once the last
/// sender is dropped, rather than dying with the runtime.
pub fn spawn_writer(pool: PgPool) -> (IngestSender, JoinHandle<()>) {
    let (tx, rx) = channel(QUEUE_CAPACITY);
    spawn_writer_on(pool, rx, tx)
}

/// The ingest channel on its own, with no writer attached.
///
/// Public so tests can drive the load-shedding path: hold the receiver without
/// draining it and the queue fills deterministically at whatever depth is asked
/// for, which is not something a 10,000-deep production queue can demonstrate.
pub fn channel(capacity: usize) -> (IngestSender, mpsc::Receiver<PendingEvent>) {
    mpsc::channel(capacity)
}

fn spawn_writer_on(
    pool: PgPool,
    mut rx: mpsc::Receiver<PendingEvent>,
    tx: IngestSender,
) -> (IngestSender, JoinHandle<()>) {
    let handle = tokio::spawn(async move {
        let mut batch: Vec<PendingEvent> = Vec::with_capacity(BATCH_MAX);
        // `recv_many` returns as soon as *one* event is available, taking up to
        // BATCH_MAX if more are already queued. So an idle server writes each
        // event immediately and batches form only under load, exactly when they
        // help — no flush timer, and no latency floor for tests to wait out.
        while rx.recv_many(&mut batch, BATCH_MAX).await > 0 {
            if let Err(err) = db::insert_events(&pool, &batch).await {
                // One bad row fails the whole statement, which would take the
                // good rows with it. Retry singly so a poison event costs one
                // event, and so the failure counter reflects rows actually lost.
                tracing::warn!(
                    error = %err,
                    events = batch.len(),
                    "event batch insert failed; retrying individually"
                );
                for event in &batch {
                    if let Err(err) = db::insert_events(&pool, std::slice::from_ref(event)).await {
                        metrics::counter!("dullahan_ingest_insert_failures_total").increment(1);
                        tracing::warn!(error = %err, "failed to insert event");
                    }
                }
            }
            batch.clear();
        }
        tracing::debug!("ingest writer drained and stopped");
    });

    (tx, handle)
}

pub async fn collect(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(mut payload): Json<RawPayload>,
) -> StatusCode {
    if let Err(reason) = payload.validate() {
        tracing::debug!(reason, "rejected /collect payload");
        return StatusCode::BAD_REQUEST;
    }
    payload.clamp_ts(chrono::Utc::now().timestamp_millis());

    // Admission against the cached tenant registry — no DB round trip on this
    // hot path, and cheaper than the linear scan over `ALLOWED_SITES` it
    // replaces. Setting a site inactive stops its collection within one refresh.
    let site_id = payload.site_id();
    if !crate::sites::snapshot(&state.sites).is_active(site_id) {
        return StatusCode::FORBIDDEN;
    }

    let country = headers
        .get("x-country")
        .and_then(|v| v.to_str().ok())
        .filter(|c| c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
        .map(|c| c.to_ascii_uppercase());

    // Rung 2 enrichment (opt-in). With sessions disabled this handler reads
    // neither the User-Agent nor the selected client IP for analytics. When
    // enabled, the UA feeds the salted daily hash only — it is never stored.
    let visitor_hash = if state.config.sessions_enabled {
        let ua = headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let peer_ip = peer.map(|ConnectInfo(addr)| addr.ip());
        match crate::client_ip::select_client_ip(
            &headers,
            peer_ip,
            state.config.trust_proxy_headers,
        ) {
            Some(ip) => {
                let today = chrono::Utc::now().date_naive();
                match crate::salt::current_salt(&state.pool, &state.salt_cache, today).await {
                    Ok(salt) => Some(crate::salt::visitor_hash(
                        &salt,
                        payload.site_id(),
                        &ip.to_string(),
                        ua,
                    )),
                    Err(err) => {
                        tracing::warn!(error = %err, "salt lookup failed; skipping visitor hash");
                        None
                    }
                }
            }
            None => None,
        }
    } else {
        None
    };

    // Enqueue rather than spawn. `try_send` never awaits, so a saturated writer
    // cannot make the handler itself slow — the response is immediate either way.
    match state
        .ingest_tx
        .try_send(PendingEvent::new(payload, country, visitor_hash))
    {
        Ok(()) => StatusCode::ACCEPTED,
        Err(mpsc::error::TrySendError::Full(_)) => {
            // Shed load honestly. A 202 here would promise durability the server
            // is not providing; 503 lets a caller retry and makes the loss
            // visible in its own metric rather than hiding inside insert failures.
            metrics::counter!("dullahan_ingest_queue_full_total").increment(1);
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            // The writer task is gone, so nothing will ever be persisted again.
            // Distinct counter: this is a bug or a shutdown race, not overload.
            metrics::counter!("dullahan_ingest_queue_closed_total").increment(1);
            tracing::error!("ingest writer is gone; dropping event");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
