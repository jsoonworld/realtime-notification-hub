use thiserror::Error;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub server_port: u16,
    pub redis_url: String,
    pub jwt_secret: String,
    pub internal_api_key: String,
    pub allowed_origins: Vec<String>,
    pub sse_channel_capacity: usize,
    pub presence_user_ttl_secs: u64,
    pub presence_hash_ttl_secs: u64,
    pub write_behind_interval_secs: u64,
    pub write_behind_batch_size: usize,
    pub stream_maxlen: usize,
    pub lock_ttl_secs: u64,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("유효하지 않은 포트 번호")]
    InvalidPort,
    #[error("JWT_SECRET 환경변수가 프로덕션 환경에서 필수입니다")]
    MissingJwtSecret,
    #[error("유효하지 않은 숫자 설정: {0}")]
    InvalidNumber(String),
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let server_port = std::env::var("SERVER_PORT")
            .unwrap_or_else(|_| "8083".to_string())
            .parse()
            .map_err(|_| ConfigError::InvalidPort)?;

        let redis_url = std::env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://:moalog_redis_local@localhost:6379".to_string());

        let jwt_secret = match std::env::var("JWT_SECRET") {
            Ok(v) => v,
            Err(_) if cfg!(debug_assertions) => {
                tracing::warn!("JWT_SECRET 미설정. 개발 환경 기본값 사용.");
                "local_dev_secret".to_string()
            }
            Err(_) => return Err(ConfigError::MissingJwtSecret),
        };

        let internal_api_key = std::env::var("INTERNAL_API_KEY")
            .unwrap_or_else(|_| "local_dev_internal_key".to_string());

        let allowed_origins = std::env::var("ALLOWED_ORIGINS")
            .unwrap_or_else(|_| {
                "http://localhost:3000,http://localhost:5173,http://localhost:5174".to_string()
            })
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let sse_channel_capacity = parse_env_or("SSE_CHANNEL_CAPACITY", 256)?;
        let presence_user_ttl_secs = parse_env_or("PRESENCE_USER_TTL_SECS", 60)?;
        let presence_hash_ttl_secs = parse_env_or("PRESENCE_HASH_TTL_SECS", 120)?;
        let write_behind_interval_secs = parse_env_or("WRITE_BEHIND_INTERVAL_SECS", 30)?;
        let write_behind_batch_size = parse_env_or("WRITE_BEHIND_BATCH_SIZE", 200)?;
        let stream_maxlen = parse_env_or("STREAM_MAXLEN", 10000)?;
        let lock_ttl_secs = parse_env_or("LOCK_TTL_SECS", 10)?;

        Ok(Self {
            server_port,
            redis_url,
            jwt_secret,
            internal_api_key,
            allowed_origins,
            sse_channel_capacity,
            presence_user_ttl_secs,
            presence_hash_ttl_secs,
            write_behind_interval_secs,
            write_behind_batch_size,
            stream_maxlen,
            lock_ttl_secs,
        })
    }
}

fn parse_env_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T, ConfigError> {
    match std::env::var(key) {
        Ok(val) => val
            .parse()
            .map_err(|_| ConfigError::InvalidNumber(key.to_string())),
        Err(_) => Ok(default),
    }
}
