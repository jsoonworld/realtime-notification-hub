use std::time::Duration;

use super::connection_manager::ConnectionManager;

pub fn spawn_cleanup_task(manager: ConnectionManager) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let removed = manager.cleanup_empty_rooms().await;
                    if !removed.is_empty() {
                        tracing::debug!(
                            count = removed.len(),
                            rooms = ?removed,
                            "빈 방 정리 완료"
                        );
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Cleanup 태스크 종료");
                    break;
                }
            }
        }
    })
}
