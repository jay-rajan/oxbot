//! The door's own HTTP routes: `POST /pair`, and the key check in front of
//! the page and WebSocket.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use oxbot_core::Viewer;
use oxbot_protocol::{DeviceInfo, PairRequest};

use super::{Door, PairError};

/// Who a request on the door comes from (set by the key check).
#[derive(Debug, Clone)]
pub struct DeviceAuth {
    /// The paired device.
    pub device: DeviceInfo,
    /// Its person, by id: never the home computer.
    pub viewer: Viewer,
}

/// `POST /pair`: a device pairs with a code.
pub async fn pair(State(door): State<Arc<Door>>, Json(req): Json<PairRequest>) -> Response {
    match door.pair(&req) {
        Ok(reply) => Json(reply).into_response(),
        Err(err) => {
            // Slow down guessing.
            tokio::time::sleep(Duration::from_millis(400)).await;
            let status = match err {
                PairError::HomeComputer(_) => StatusCode::SERVICE_UNAVAILABLE,
                PairError::NoCode | PairError::WrongCode => StatusCode::FORBIDDEN,
            };
            (status, err.to_string()).into_response()
        }
    }
}

/// Every door route but `/pair` needs a paired device's key. A key the home computer
/// doesn't know gets 401 (the device forgets its pairing); a database hiccup
/// gets 503 (the device keeps it and tries again).
pub async fn require_key(
    State(door): State<Arc<Door>>,
    mut request: Request,
    next: Next,
) -> Response {
    let key = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let device = match key.map(|k| door.check_key(k)) {
        Some(Ok(Some(found))) => found,
        Some(Err(err)) => {
            tracing::warn!(%err, "couldn't check a device's key");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "The home computer couldn't check this phone just now. Trying again…",
            )
                .into_response();
        }
        Some(Ok(None)) | None => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return (
                StatusCode::UNAUTHORIZED,
                "This device isn't paired with the home computer (or was removed). Pair it again.",
            )
                .into_response();
        }
    };
    let (device, viewer) = device;
    request
        .extensions_mut()
        .insert(DeviceAuth { device, viewer });
    next.run(request).await
}
