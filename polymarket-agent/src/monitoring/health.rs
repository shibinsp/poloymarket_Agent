//! Health check HTTP endpoint.
//!
//! Provides a tiny HTTP server on localhost:9090/health that returns
//! agent status as JSON. Used by external uptime monitors.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::market::models::AgentState;

/// Shared health state updated by the agent loop.
#[derive(Clone)]
pub struct HealthState {
    inner: Arc<RwLock<HealthData>>,
}

#[derive(Debug, Clone, Serialize)]
struct HealthData {
    status: String,
    agent_state: String,
    cycle_number: u64,
    started_at: DateTime<Utc>,
    last_cycle_at: Option<DateTime<Utc>>,
    uptime_seconds: i64,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HealthData {
                status: "ok".to_string(),
                agent_state: "INITIALIZING".to_string(),
                cycle_number: 0,
                started_at: Utc::now(),
                last_cycle_at: None,
                uptime_seconds: 0,
            })),
        }
    }

    pub fn record_cycle(&self, cycle_number: u64, state: AgentState) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut data = inner.write().await;
            data.cycle_number = cycle_number;
            data.agent_state = state.to_string();
            data.last_cycle_at = Some(Utc::now());
            data.uptime_seconds = (Utc::now() - data.started_at).num_seconds();
            data.status = if state == AgentState::Dead {
                "dead".to_string()
            } else {
                "ok".to_string()
            };
        });
    }
}

/// Spawn the health check HTTP server. Returns a handle that can be aborted.
pub fn spawn_health_server(state: HealthState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let addr = "127.0.0.1:9090";
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => {
                info!(addr, "Health check server listening");
                l
            }
            Err(e) => {
                warn!(error = %e, addr, "Failed to bind health check server — continuing without it");
                return;
            }
        };

        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!(error = %e, "Failed to accept health check connection");
                    continue;
                }
            };

            let state = state.clone();
            tokio::spawn(async move {
                // Read the request (we don't care about the contents)
                let mut buf = [0u8; 1024];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;

                // Build JSON response
                let data = state.inner.read().await;
                let body = serde_json::to_string(&*data).unwrap_or_else(|_| {
                    r#"{"status":"error","message":"serialization failed"}"#.to_string()
                });

                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    body.len(),
                    body
                );

                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_state_creation() {
        let state = HealthState::new();
        // Should be constructable without async runtime
        let _ = state.clone();
    }

    #[tokio::test]
    async fn test_health_state_update() {
        let state = HealthState::new();
        state.record_cycle(5, AgentState::Alive);

        // Give the spawned task time to complete
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let data = state.inner.read().await;
        assert_eq!(data.cycle_number, 5);
        assert_eq!(data.agent_state, "ALIVE");
        assert_eq!(data.status, "ok");
    }

    #[tokio::test]
    async fn test_health_state_dead() {
        let state = HealthState::new();
        state.record_cycle(10, AgentState::Dead);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let data = state.inner.read().await;
        assert_eq!(data.status, "dead");
        assert_eq!(data.agent_state, "DEAD");
    }

    #[tokio::test]
    async fn test_health_server_responds() {
        let state = HealthState::new();
        state.record_cycle(1, AgentState::Alive);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let handle = spawn_health_server(state);

        // Give the server time to bind
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect and send a GET request
        let mut stream = tokio::net::TcpStream::connect("127.0.0.1:9090")
            .await
            .expect("should connect to health server");

        let request = "GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
            .await
            .unwrap();

        // Read response
        let mut buf = vec![0u8; 4096];
        let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buf)
            .await
            .unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);

        assert!(response.contains("200 OK"));
        assert!(response.contains("\"status\":\"ok\""));
        assert!(response.contains("\"agent_state\":\"ALIVE\""));

        handle.abort();
    }
}
