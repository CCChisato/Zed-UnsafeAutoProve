use axum::{Router, extract::State, http::StatusCode, routing::post};
use std::sync::Arc;

/// Shared state accessible from axum handler.
pub struct CallbackState {
    /// The port the server is listening on.
    pub port: u16,
    /// Callback to invoke when a subagent completes.
    /// Passed the parsed JSON body of the POST request.
    pub on_completion: Arc<dyn Fn(serde_json::Value) + Send + Sync>,
}

/// Start the callback server on the given port in a dedicated thread,
/// returning a shutdown sender.
pub fn start_server(
    port: u16,
    on_completion: Arc<dyn Fn(serde_json::Value) + Send + Sync>,
) -> tokio::sync::oneshot::Sender<()> {
    let state = Arc::new(CallbackState {
        port,
        on_completion,
    });

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Spawn a dedicated OS thread with its own tokio runtime.
    // We cannot use tokio::spawn because gpui does not run on a tokio runtime.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async move {
            let addr = format!("127.0.0.1:{}", port);
            let listener = match tokio::net::TcpListener::bind(&addr).await {
                Ok(l) => l,
                Err(e) => {
                    log::error!("claw_callback: failed to bind port {}: {}", port, e);
                    return;
                }
            };

            let app = Router::new()
                .route("/claw_callback", post(handle_callback))
                .with_state(state);

            log::info!("claw_callback: listening on {}", addr);

            let tcp_listener = listener.into_std().unwrap();
            axum::Server::from_tcp(tcp_listener)
                .unwrap()
                .serve(app.into_make_service())
                .with_graceful_shutdown(async {
                    shutdown_rx.await.ok();
                })
                .await
                .ok();
        });
    });

    shutdown_tx
}

/// POST /claw_callback — called by claw when a subagent completes.
async fn handle_callback(
    State(state): State<Arc<CallbackState>>,
    body: axum::extract::Json<serde_json::Value>,
) -> StatusCode {
    let raw = body.0.clone();
    let session_id = raw.get("session_id").and_then(|v| v.as_str());
    let status = raw.get("status").and_then(|v| v.as_str());

    if session_id.is_none() || status.is_none() {
        log::warn!("claw_callback: invalid payload: {:?}", raw);
        return StatusCode::BAD_REQUEST;
    }

    log::info!(
        "claw_callback: subagent {} completed with status {}",
        session_id.unwrap(),
        status.unwrap()
    );

    (state.on_completion)(raw);
    StatusCode::OK
}

/// Build the callback URL string.
pub fn callback_url(port: u16) -> String {
    format!("http://127.0.0.1:{}/claw_callback", port)
}
