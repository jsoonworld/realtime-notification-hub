use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{RwLock, broadcast};

use crate::error::AppError;
use super::event::SseEvent;

const MAX_USER_CONNECTIONS: usize = 5;
const MAX_ROOM_CONNECTIONS: usize = 200;

#[derive(Clone)]
pub struct ConnectionManager {
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<SseEvent>>>>,
    user_connections: Arc<DashMap<i64, AtomicUsize>>,
    room_connections: Arc<DashMap<String, AtomicUsize>>,
    channel_capacity: usize,
}

impl ConnectionManager {
    pub fn new(channel_capacity: usize) -> Self {
        Self {
            rooms: Arc::new(RwLock::new(HashMap::new())),
            user_connections: Arc::new(DashMap::new()),
            room_connections: Arc::new(DashMap::new()),
            channel_capacity,
        }
    }

    pub async fn subscribe(
        &self,
        room_id: &str,
        user_id: i64,
    ) -> Result<broadcast::Receiver<SseEvent>, AppError> {
        // Check per-user connection limit
        let user_count = self
            .user_connections
            .entry(user_id)
            .or_insert_with(|| AtomicUsize::new(0));
        let current_user = user_count.load(Ordering::SeqCst);
        if current_user >= MAX_USER_CONNECTIONS {
            return Err(AppError::SseConnectionError(format!(
                "사용자당 최대 SSE 연결 수({}) 초과",
                MAX_USER_CONNECTIONS
            )));
        }

        // Check per-room connection limit
        let room_count = self
            .room_connections
            .entry(room_id.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        let current_room = room_count.load(Ordering::SeqCst);
        if current_room >= MAX_ROOM_CONNECTIONS {
            return Err(AppError::SseConnectionError(format!(
                "방당 최대 SSE 연결 수({}) 초과",
                MAX_ROOM_CONNECTIONS
            )));
        }

        // Get or create the broadcast channel for this room
        let rx = {
            let mut rooms = self.rooms.write().await;
            let tx = rooms
                .entry(room_id.to_string())
                .or_insert_with(|| broadcast::channel(self.channel_capacity).0);
            tx.subscribe()
        };

        // Increment counters after successful subscription
        user_count.fetch_add(1, Ordering::SeqCst);
        room_count.fetch_add(1, Ordering::SeqCst);

        Ok(rx)
    }

    pub fn unsubscribe(&self, room_id: &str, user_id: i64) {
        let should_remove_user = self
            .user_connections
            .get(&user_id)
            .map(|counter| counter.fetch_sub(1, Ordering::SeqCst) <= 1)
            .unwrap_or(false);
        if should_remove_user {
            self.user_connections.remove(&user_id);
        }

        let room_key = room_id.to_string();
        let should_remove_room = self
            .room_connections
            .get(&room_key)
            .map(|counter| counter.fetch_sub(1, Ordering::SeqCst) <= 1)
            .unwrap_or(false);
        if should_remove_room {
            self.room_connections.remove(&room_key);
        }
    }

    pub async fn broadcast(&self, room_id: &str, event: SseEvent) -> usize {
        let rooms = self.rooms.read().await;
        match rooms.get(room_id) {
            Some(tx) => tx.send(event).unwrap_or(0),
            None => 0,
        }
    }

    pub async fn cleanup_empty_rooms(&self) -> Vec<String> {
        let mut rooms = self.rooms.write().await;
        let empty_rooms: Vec<String> = rooms
            .iter()
            .filter(|(_, tx)| tx.receiver_count() == 0)
            .map(|(id, _)| id.clone())
            .collect();

        for room_id in &empty_rooms {
            rooms.remove(room_id);
            self.room_connections.remove(room_id);
        }

        empty_rooms
    }

    pub async fn total_connections(&self) -> usize {
        let rooms = self.rooms.read().await;
        rooms.values().map(|tx| tx.receiver_count()).sum()
    }

    pub async fn active_rooms(&self) -> usize {
        let rooms = self.rooms.read().await;
        rooms.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_event(event_type: &str) -> SseEvent {
        SseEvent {
            event_type: event_type.to_string(),
            data: serde_json::json!({"msg": "hello"}),
            stream_id: None,
        }
    }

    #[tokio::test]
    async fn test_subscribe_and_broadcast() {
        let manager = ConnectionManager::new(16);
        let mut rx = manager.subscribe("room1", 1).await.unwrap();

        let event = test_event("test.event");
        let count = manager.broadcast("room1", event.clone()).await;
        assert_eq!(count, 1);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type, "test.event");
    }

    #[tokio::test]
    async fn test_broadcast_multiple_subscribers() {
        let manager = ConnectionManager::new(16);
        let mut rx1 = manager.subscribe("room1", 1).await.unwrap();
        let mut rx2 = manager.subscribe("room1", 2).await.unwrap();

        let event = test_event("multi.event");
        let count = manager.broadcast("room1", event).await;
        assert_eq!(count, 2);

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert_eq!(e1.event_type, "multi.event");
        assert_eq!(e2.event_type, "multi.event");
    }

    #[tokio::test]
    async fn test_subscriber_drop_decreases_count() {
        let manager = ConnectionManager::new(16);
        let rx = manager.subscribe("room1", 1).await.unwrap();
        assert_eq!(manager.total_connections().await, 1);

        drop(rx);
        assert_eq!(manager.total_connections().await, 0);
    }

    #[tokio::test]
    async fn test_broadcast_nonexistent_room_returns_zero() {
        let manager = ConnectionManager::new(16);
        let count = manager.broadcast("nonexistent", test_event("test")).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_concurrent_multi_room() {
        let manager = ConnectionManager::new(16);
        let mut rx_a = manager.subscribe("roomA", 1).await.unwrap();
        let mut rx_b = manager.subscribe("roomB", 2).await.unwrap();

        manager.broadcast("roomA", test_event("event.a")).await;
        manager.broadcast("roomB", test_event("event.b")).await;

        let ea = rx_a.recv().await.unwrap();
        let eb = rx_b.recv().await.unwrap();
        assert_eq!(ea.event_type, "event.a");
        assert_eq!(eb.event_type, "event.b");
        assert_eq!(manager.active_rooms().await, 2);
    }

    #[tokio::test]
    async fn test_user_connection_limit() {
        let manager = ConnectionManager::new(16);
        let mut _receivers = Vec::new();

        for _ in 0..MAX_USER_CONNECTIONS {
            let rx = manager.subscribe("room1", 1).await.unwrap();
            _receivers.push(rx);
        }

        let result = manager.subscribe("room1", 1).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::SseConnectionError(msg) => {
                assert!(msg.contains(&MAX_USER_CONNECTIONS.to_string()));
            }
            other => panic!("Expected SseConnectionError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_room_connection_limit() {
        let manager = ConnectionManager::new(16);
        let mut _receivers = Vec::new();

        for user_id in 0..MAX_ROOM_CONNECTIONS as i64 {
            let rx = manager.subscribe("room1", user_id).await.unwrap();
            _receivers.push(rx);
        }

        let result = manager.subscribe("room1", 999).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::SseConnectionError(msg) => {
                assert!(msg.contains(&MAX_ROOM_CONNECTIONS.to_string()));
            }
            other => panic!("Expected SseConnectionError, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_cleanup_empty_rooms() {
        let manager = ConnectionManager::new(16);
        let rx = manager.subscribe("room1", 1).await.unwrap();
        let _rx2 = manager.subscribe("room2", 2).await.unwrap();
        assert_eq!(manager.active_rooms().await, 2);

        drop(rx);
        let removed = manager.cleanup_empty_rooms().await;
        assert_eq!(removed.len(), 1);
        assert!(removed.contains(&"room1".to_string()));
        assert_eq!(manager.active_rooms().await, 1);
    }

    #[tokio::test]
    async fn test_unsubscribe_cleans_counters() {
        let manager = ConnectionManager::new(16);
        let _rx = manager.subscribe("room1", 1).await.unwrap();

        manager.unsubscribe("room1", 1);

        // User counter should be removed (was 1, decremented to 0)
        assert!(!manager.user_connections.contains_key(&1));
    }
}
