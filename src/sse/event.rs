use axum::response::sse::Event;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SseEvent {
    pub event_type: String,
    pub data: serde_json::Value,
    pub stream_id: Option<String>,
}

impl From<SseEvent> for Event {
    fn from(sse_event: SseEvent) -> Self {
        let data = serde_json::to_string(&sse_event).unwrap_or_default();
        Event::default().event(&sse_event.event_type).data(data)
    }
}
