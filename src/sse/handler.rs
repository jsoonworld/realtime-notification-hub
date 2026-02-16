use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde::Deserialize;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::auth::AuthUser;
use crate::error::AppError;
use crate::state::AppState;

use super::connection_manager::ConnectionManager;

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub last_event_id: Option<String>,
}

pub async fn stream_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(room_id): Path<i64>,
    Query(_query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let user_id = auth_user.user_id()?;
    let room_id_str = room_id.to_string();

    let rx = state
        .connection_manager
        .subscribe(&room_id_str, user_id)
        .await?;

    let manager = state.connection_manager.clone();
    let room_for_drop = room_id_str.clone();

    let stream = BroadcastStream::new(rx).map(move |result| {
        let event = match result {
            Ok(sse_event) => Event::from(sse_event),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                let lagged_data = serde_json::json!({
                    "eventType": "system.lagged",
                    "data": { "missed": n },
                    "streamId": null
                });
                Event::default()
                    .event("system.lagged")
                    .data(lagged_data.to_string())
            }
        };
        Ok::<_, Infallible>(event)
    });

    // Wrap the stream to call unsubscribe on drop
    let drop_stream = DropGuardStream {
        inner: stream,
        manager,
        room_id: room_for_drop,
        user_id,
    };

    Ok(Sse::new(drop_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

pin_project_lite::pin_project! {
    struct DropGuardStream<S> {
        #[pin]
        inner: S,
        manager: ConnectionManager,
        room_id: String,
        user_id: i64,
    }

    impl<S> PinnedDrop for DropGuardStream<S> {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();
            this.manager.unsubscribe(this.room_id, *this.user_id);
        }
    }
}

impl<S: Stream> Stream for DropGuardStream<S> {
    type Item = S::Item;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().inner.poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}
