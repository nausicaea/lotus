use std::net::SocketAddr;

use anyhow::Context;
use axum::{Json, extract::State, http::StatusCode};
use tokio::sync::mpsc::Sender;
use tracing::{Instrument, debug, info_span, instrument};

use crate::OUTPUT_PORT;

#[derive(Debug, Clone)]
struct ServerState {
    sender: Sender<serde_json::Value>,
}

#[instrument]
async fn root(
    State(state): State<ServerState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    let mpsc_span = info_span!("mpsc_sender_server");

    debug!("Forward the request payload to the main task");
    state
        .sender
        .send(payload)
        .instrument(mpsc_span)
        .await
        .context("When sending the request payload to the main task")
        .unwrap();

    StatusCode::NO_CONTENT
}

#[instrument]
pub async fn run_server(sender: Sender<serde_json::Value>) -> anyhow::Result<()> {
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], OUTPUT_PORT));
    let state = ServerState { sender };

    let app = axum::Router::new()
        .route("/", axum::routing::post(root))
        .with_state(state);

    debug!("Bind the axum server to {bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    axum::serve(listener, app)
        .await
        .context("When running the event responder server")?;

    Ok(())
}
