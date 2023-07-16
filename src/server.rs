use std::net::SocketAddr;

use anyhow::Context;
use axum::{extract::State, http::StatusCode, Json};
use tokio::sync::mpsc::Sender;

use crate::OUTPUT_PORT;

#[derive(Debug, Clone)]
struct ServerState {
    sender: Sender<serde_json::Value>,
}

async fn root(
    State(state): State<ServerState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    state
        .sender
        .send(payload)
        .await
        .context("When sending the request payload to the main task")
        .unwrap();

    StatusCode::NO_CONTENT
}

pub async fn run_server(sender: Sender<serde_json::Value>) -> anyhow::Result<()> {
    let state = ServerState { sender };
    axum::Server::bind(&SocketAddr::from(([0, 0, 0, 0], OUTPUT_PORT)))
        .serve(
            axum::Router::new()
                .route("/", axum::routing::post(root))
                .with_state(state)
                .into_make_service(),
        )
        .await
        .context("When running the event responder server")?;

    Ok(())
}
