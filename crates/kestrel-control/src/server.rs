//! The HTTP command API and the WebSocket state feed.
//!
//! Two channels carrying different things, the same split the rest of the
//! fleet's control APIs use:
//!
//! * **`/ws`** — state, pushed. Re-snapshotted at 5 Hz and sent only when the
//!   revision moved, so a control surface gets tally without polling and an
//!   idle show costs nothing.
//! * **HTTP** — commands, and the one-off reads a surface does at connect.
//!
//! Refusals come back as **HTTP 200 with `ok: false`** and a populated `error`,
//! not a 4xx. A client checking only the status code would report every refusal
//! as a success, so the module on the other end checks the body — but it never
//! has to supply the message itself.

use crate::state::{EnableBody, OutputBody, Reply, RoiBody, RouteBody, Shared};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use kestrel_core::{NormRect, OutputId, RoiId, MIN_SIZE};
use std::sync::Arc;
use std::time::Duration;

/// How often the WebSocket re-snapshots. Fast enough that a crosspoint change
/// made on one surface lights up on another before the operator's hand leaves
/// the button; slow enough to be free.
const PUSH_INTERVAL: Duration = Duration::from_millis(200);

// axum 0.8 takes capture groups as `{id}`; `:id` now panics at router build
// time rather than silently not matching.
pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/state", get(state))
        .route("/api/route", post(route))
        .route("/api/output/enable", post(enable))
        .route("/api/roi", post(create_roi))
        .route("/api/roi/{id}", post(update_roi))
        .route("/api/roi/{id}", delete(delete_roi))
        .route("/api/output/{id}", post(update_output))
        .route("/ws", get(ws_upgrade))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(shared)
}

/// Bind and serve. Returns the address actually bound, which is how port 0
/// works for tests.
pub async fn serve(
    shared: Arc<Shared>,
    addr: std::net::SocketAddr,
) -> anyhow::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let app = router(shared);
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "control server stopped");
        }
    });
    tracing::info!(%bound, "control server listening");
    Ok((bound, handle))
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "app": "kestrel",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn state(State(s): State<Arc<Shared>>) -> impl IntoResponse {
    Json(s.snapshot())
}

async fn route(State(s): State<Arc<Shared>>, Json(body): Json<RouteBody>) -> impl IntoResponse {
    Json(Reply::from(
        s.edit(|show| show.route(body.output, body.roi)),
    ))
}

async fn enable(State(s): State<Arc<Shared>>, Json(body): Json<EnableBody>) -> impl IntoResponse {
    let reply = s.edit(|show| {
        if body.toggle {
            show.outputs_enabled = !show.outputs_enabled;
            Reply::ok()
        } else if let Some(v) = body.enabled {
            show.outputs_enabled = v;
            Reply::ok()
        } else {
            Reply::refused("pass either \"enabled\": true/false or \"toggle\": true")
        }
    });
    Json(reply)
}

async fn create_roi(State(s): State<Arc<Shared>>, Json(body): Json<RoiBody>) -> impl IntoResponse {
    let rect = body
        .rect
        .map(|r| NormRect::new(r[0], r[1], r[2], r[3]))
        .unwrap_or(NormRect::new(0.25, 0.25, 0.5, 0.5));
    // Two different failures, handled differently on purpose. A region that is
    // merely *off* the frame is clamped back in, matching what dragging one off
    // the edge does. A region with no area cannot be clamped into something
    // meaningful, so it is refused — note that checking `rect.clamped()` here
    // would test nothing at all, because `clamped()` forces the size up to
    // MIN_SIZE and can never produce an invalid rect.
    if !rect.w.is_finite() || !rect.h.is_finite() || rect.w < MIN_SIZE || rect.h < MIN_SIZE {
        return Json(Reply::refused(format!(
            "a region must be at least {MIN_SIZE} of the frame in each axis; got {} x {}",
            rect.w, rect.h
        )));
    }
    let name = body.name.unwrap_or_else(|| "Region".into());
    let id = s.edit(|show| {
        let id = show.add_roi(name, rect);
        if let (Some(lock), Some(r)) = (body.lock_aspect, show.roi_mut(id)) {
            r.lock_aspect = lock;
        }
        id
    });
    Json(Reply::created(id.0))
}

async fn update_roi(
    State(s): State<Arc<Shared>>,
    Path(id): Path<u32>,
    Json(body): Json<RoiBody>,
) -> impl IntoResponse {
    let id = RoiId(id);
    let reply = s.edit(|show| {
        let aspect = show.output_format.size.aspect();
        let input = show.input_size;
        let Some(roi) = show.roi_mut(id) else {
            return Reply::refused(format!("no region with id {id}"));
        };
        if let Some(n) = body.name {
            roi.name = n;
        }
        if let Some(l) = body.lock_aspect {
            roi.lock_aspect = l;
        }
        if let Some(r) = body.rect {
            let mut rect = NormRect::new(r[0], r[1], r[2], r[3]).clamped();
            if roi.lock_aspect {
                rect = rect.with_aspect(aspect, input);
            }
            roi.rect = rect;
        }
        Reply::ok()
    });
    Json(reply)
}

async fn delete_roi(State(s): State<Arc<Shared>>, Path(id): Path<u32>) -> impl IntoResponse {
    Json(Reply::from(s.edit(|show| show.remove_roi(RoiId(id)))))
}

async fn update_output(
    State(s): State<Arc<Shared>>,
    Path(id): Path<u32>,
    Json(body): Json<OutputBody>,
) -> impl IntoResponse {
    let id = OutputId(id);
    let reply = s.edit(|show| {
        let Some(o) = show.output_mut(id) else {
            return Reply::refused(format!("no output with id {id}"));
        };
        if let Some(l) = body.label {
            o.label = l;
        }
        if let Some(i) = body.idle {
            o.idle = i;
        }
        if let Some(f) = body.fit {
            o.fit = f;
        }
        Reply::ok()
    });
    Json(reply)
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(s): State<Arc<Shared>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| push_state(socket, s))
}

async fn push_state(mut socket: WebSocket, shared: Arc<Shared>) {
    // Send once immediately: a surface that connects mid-show must not sit
    // blank until something changes.
    let mut last = u64::MAX;
    let mut ticker = tokio::time::interval(PUSH_INTERVAL);
    loop {
        ticker.tick().await;
        let rev = shared.revision();
        if rev == last {
            continue;
        }
        last = rev;
        let Ok(text) = serde_json::to_string(&shared.snapshot()) else {
            continue;
        };
        // axum 0.8 carries ws text as Utf8Bytes rather than String; the
        // conversion is a move, not a copy.
        if socket.send(Message::Text(text.into())).await.is_err() {
            return; // the surface went away
        }
    }
}
