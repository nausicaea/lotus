use std::net::SocketAddr;

use anyhow::Context;
use axum::{extract::State, http::StatusCode, Json};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info_span, instrument, Instrument};

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
    let response_handler_span = info_span!("response_handler");

    debug!("Bind the axum server to {bind_addr}");
    axum::Server::bind(&bind_addr)
        .serve(
            axum::Router::new()
                .route("/", axum::routing::post(root))
                .with_state(state)
                .into_make_service(),
        )
        .instrument(response_handler_span)
        .await
        .context("When running the event responder server")?;

    Ok(())
}
