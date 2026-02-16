use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::response::ErrorResponse;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // Common
    #[error("잘못된 요청: {0}")]
    BadRequest(String),

    #[error("인증 실패: {0}")]
    Unauthorized(String),

    #[error("권한 없음: {0}")]
    Forbidden(String),

    #[error("리소스를 찾을 수 없음: {0}")]
    NotFound(String),

    #[error("서버 내부 오류: {0}")]
    InternalError(String),

    // Redis
    #[error("Redis 연결 실패: {0}")]
    RedisConnectionError(String),

    #[error("Redis 명령 실패: {0}")]
    RedisCommandError(String),

    // Notification
    #[error("알림 발행 실패: {0}")]
    PublishFailed(String),

    #[error("유효하지 않은 이벤트 타입: {0}")]
    InvalidEventType(String),

    #[error("중복 이벤트: {0}")]
    DuplicateEvent(String),

    // SSE
    #[error("SSE 연결 실패: {0}")]
    SseConnectionError(String),

    // Room
    #[error("존재하지 않는 방: {0}")]
    RoomNotFound(String),
}

impl AppError {
    pub fn error_code(&self) -> &str {
        match self {
            Self::BadRequest(_) => "COMMON400",
            Self::Unauthorized(_) => "AUTH4001",
            Self::Forbidden(_) => "COMMON403",
            Self::NotFound(_) => "COMMON404",
            Self::InternalError(_) => "COMMON500",
            Self::RedisConnectionError(_) => "REDIS5001",
            Self::RedisCommandError(_) => "REDIS5002",
            Self::PublishFailed(_) => "NOTIF5001",
            Self::InvalidEventType(_) => "NOTIF4001",
            Self::DuplicateEvent(_) => "NOTIF4091",
            Self::SseConnectionError(_) => "SSE5001",
            Self::RoomNotFound(_) => "ROOM4041",
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) | Self::InvalidEventType(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) | Self::RoomNotFound(_) => StatusCode::NOT_FOUND,
            Self::DuplicateEvent(_) => StatusCode::CONFLICT,
            Self::InternalError(_)
            | Self::RedisConnectionError(_)
            | Self::RedisCommandError(_)
            | Self::PublishFailed(_)
            | Self::SseConnectionError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let error_code = self.error_code().to_string();
        let message = self.to_string();

        if status.is_server_error() {
            tracing::error!(error_code = %error_code, "{}", message);
        }

        let body = ErrorResponse {
            is_success: false,
            code: error_code,
            message,
            result: None,
        };

        (status, Json(body)).into_response()
    }
}

impl From<redis::RedisError> for AppError {
    fn from(err: redis::RedisError) -> Self {
        AppError::RedisCommandError(err.to_string())
    }
}
