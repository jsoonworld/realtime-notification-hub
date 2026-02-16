use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseResponse<T: Serialize> {
    pub is_success: bool,
    pub code: String,
    pub message: String,
    pub result: Option<T>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    pub is_success: bool,
    pub code: String,
    pub message: String,
    pub result: Option<()>,
}

impl<T: Serialize> BaseResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            is_success: true,
            code: "SUCCESS".to_string(),
            message: "성공".to_string(),
            result: Some(data),
        }
    }

    pub fn message(msg: &str) -> BaseResponse<()> {
        BaseResponse {
            is_success: true,
            code: "SUCCESS".to_string(),
            message: msg.to_string(),
            result: None,
        }
    }
}

impl<T: Serialize> IntoResponse for BaseResponse<T> {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}
