use redis::aio::MultiplexedConnection;

use crate::error::AppError;

const MAX_RETRY_ATTEMPTS: u32 = 3;
const RETRY_INTERVAL_SECS: u64 = 1;

pub async fn create_redis_pool(redis_url: &str) -> Result<MultiplexedConnection, AppError> {
    let client = redis::Client::open(redis_url)
        .map_err(|e| AppError::RedisConnectionError(format!("Redis URL 파싱 실패: {}", e)))?;

    let mut last_err = None;

    for attempt in 1..=MAX_RETRY_ATTEMPTS {
        match client.get_multiplexed_tokio_connection().await {
            Ok(mut conn) => {
                // PING check
                let pong: Result<String, _> = redis::cmd("PING").query_async(&mut conn).await;
                match pong {
                    Ok(response) if response == "PONG" => {
                        tracing::info!(
                            "Redis 연결 성공 (시도 {}/{})",
                            attempt,
                            MAX_RETRY_ATTEMPTS
                        );
                        return Ok(conn);
                    }
                    Ok(unexpected) => {
                        last_err = Some(format!("PING 응답이 PONG이 아님: {}", unexpected));
                    }
                    Err(e) => {
                        last_err = Some(format!("PING 명령 실패: {}", e));
                    }
                }
            }
            Err(e) => {
                last_err = Some(format!("연결 실패: {}", e));
            }
        }

        if attempt < MAX_RETRY_ATTEMPTS {
            tracing::warn!(
                "Redis 연결 실패 (시도 {}/{}): {}. {}초 후 재시도...",
                attempt,
                MAX_RETRY_ATTEMPTS,
                last_err.as_deref().unwrap_or("unknown"),
                RETRY_INTERVAL_SECS
            );
            tokio::time::sleep(std::time::Duration::from_secs(RETRY_INTERVAL_SECS)).await;
        }
    }

    Err(AppError::RedisConnectionError(format!(
        "Redis 연결 실패 ({}회 시도 후): {}",
        MAX_RETRY_ATTEMPTS,
        last_err.unwrap_or_else(|| "unknown".to_string())
    )))
}
