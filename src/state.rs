use redis::aio::MultiplexedConnection;
use std::time::Instant;

use crate::config::AppConfig;
use crate::sse::ConnectionManager;

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub redis_pool: MultiplexedConnection,
    pub start_time: Instant,
    pub connection_manager: ConnectionManager,
}
