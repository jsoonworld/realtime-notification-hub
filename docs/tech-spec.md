# Real-time Notification Hub - 기술 명세서

> **문서 버전**: 1.0.0
> **최종 수정**: 2026-02-16
> **기반 문서**: [1-pager.md](./1-pager.md)

---

## 목차

1. [개요](#1-개요)
2. [기술 스택 상세](#2-기술-스택-상세)
3. [프로젝트 구조](#3-프로젝트-구조)
4. [핵심 모듈 설계](#4-핵심-모듈-설계)
5. [데이터 모델](#5-데이터-모델)
6. [API 상세 설계](#6-api-상세-설계)
7. [인증/인가](#7-인증인가)
8. [에러 처리](#8-에러-처리)
9. [설정 관리](#9-설정-관리)
10. [모니터링](#10-모니터링)
11. [Docker](#11-docker)
12. [테스트 전략](#12-테스트-전략)
13. [구현 페이즈](#13-구현-페이즈)
14. [Open Questions / Future Work](#14-open-questions--future-work)

---

## 1. 개요

### 1.1 프로젝트 요약

Real-time Notification Hub는 moalog-platform 생태계의 실시간 알림 서비스이다.
회고 방(RetroRoom)에서 발생하는 이벤트(제출, 댓글, 좋아요)를 Redis Pub/Sub를 통해 SSE(Server-Sent Events) 연결된 클라이언트에게 즉시 전달한다.

```
현재: Client --> 폴링(5초마다) --> 서버 --> DB 조회 --> 변경 있나?
목표: Client <-- SSE <-- 알림 허브 <-- Redis Pub/Sub <-- 이벤트 발생
```

### 1.2 목표 (Goals)

| # | 목표 | 측정 기준 |
|---|------|----------|
| G1 | 실시간 이벤트 전달 | 이벤트 발생 ~ SSE 수신 p95 < 50ms |
| G2 | 접속 현황(Presence) 실시간 표시 | heartbeat 주기 30초, 만료 60초 |
| G3 | 이벤트 영속성 + 재연결 시 catch-up | Redis Streams XRANGE로 미수신 이벤트 복원 |
| G4 | 중복 알림 방지 | 분산 락(Lua SET NX EX)으로 동일 이벤트 1회 처리 보장 |
| G5 | 읽음 상태 효율적 관리 | Write-Behind 패턴으로 Redis -> DB 배치 flush |
| G6 | 수평 확장 가능 | Redis Pub/Sub로 인스턴스 간 이벤트 팬아웃 |
| G7 | moalog-server와의 일관성 | 동일 Rust/Axum 스택, JWT 공유, 응답 포맷 통일 |

### 1.3 비목표 (Non-Goals)

| # | 비목표 | 사유 |
|---|--------|------|
| N1 | WebSocket 양방향 통신 | 알림은 서버->클라이언트 단방향이므로 SSE로 충분 |
| N2 | Kafka/RabbitMQ 도입 | 알림 허브 규모에서 Redis Pub/Sub + Streams로 충분 |
| N3 | 푸시 알림(FCM/APNs) | 모바일 앱이 없는 현재 단계에서 웹 SSE만 지원 |
| N4 | Redlock(다중 Redis 노드 락) | 알림 중복은 최악의 경우 "2번 알림"이지 데이터 유실이 아님 |
| N5 | E2E 메시지 암호화 | 회고 방 알림은 민감 데이터가 아님 |

---

## 2. 기술 스택 상세

### 2.1 핵심 의존성

| 크레이트 | 버전 | 용도 | 선택 이유 |
|----------|------|------|-----------|
| `axum` | 0.7 | HTTP 프레임워크 + SSE | moalog-server와 동일 스택. `axum::response::sse` 네이티브 SSE 지원 |
| `tokio` | 1.x | 비동기 런타임 | Rust 비동기 표준. `full` feature로 모든 기능 활성화 |
| `redis` | 0.27 | Redis 클라이언트 | `tokio-comp` + `aio` feature로 비동기 Pub/Sub, Streams, Lua 스크립트 지원 |
| `serde` | 1.0 | 직렬화/역직렬화 | Rust 생태계 표준. `derive` feature |
| `serde_json` | 1.0 | JSON 처리 | SSE data 필드 직렬화 |
| `jsonwebtoken` | 10.2 | JWT 검증 | moalog-server와 동일 크레이트. `rust_crypto` feature |
| `tower-http` | 0.5 | CORS, Trace 미들웨어 | moalog-server와 동일 |
| `axum-extra` | 0.9 | 쿠키 추출 | `cookie` feature. JWT 쿠키 인증 호환 |
| `tracing` | 0.1 | 구조화 로깅 | moalog-server와 동일 |
| `tracing-subscriber` | 0.3 | 로그 출력 | `env-filter`, `json`, `time` features |
| `chrono` | 0.4 | 날짜/시간 | `serde` feature. 이벤트 타임스탬프 |
| `uuid` | 1.0 | 고유 ID 생성 | `v4`, `serde` features. 이벤트 ID, 락 소유자 ID |
| `thiserror` | 1.0 | 에러 타입 정의 | derive 매크로로 에러 타입 간결 정의 |
| `dotenvy` | 0.15 | 환경 변수 로딩 | 로컬 개발용 `.env` 파일 지원 |
| `axum-prometheus` | 0.7 | Prometheus 메트릭 | axum 0.7 호환. HTTP 요청 메트릭 자동 수집 |
| `metrics` | 0.24 | 커스텀 메트릭 | 커넥션 수, 이벤트 처리량 등 비즈니스 메트릭 |
| `metrics-exporter-prometheus` | 0.16 | Prometheus 내보내기 | `metrics` 크레이트의 Prometheus 백엔드 |

### 2.2 개발/테스트 의존성

| 크레이트 | 버전 | 용도 |
|----------|------|------|
| `tokio-test` | 0.4 | 비동기 테스트 유틸 |
| `testcontainers` | 0.23 | Redis 컨테이너 통합 테스트 |
| `testcontainers-modules` | 0.11 | Redis 모듈 (`redis` feature) |
| `tower` | 0.5 | 테스트용 서비스 호출 (`util` feature) |
| `http-body-util` | 0.1 | 응답 본문 추출 |
| `hyper` | 1.0 | HTTP 테스트 |
| `fake` | 3.0 | 테스트 데이터 생성 |

### 2.3 Cargo.toml

```toml
[package]
name = "realtime-notification-hub"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"

[dependencies]
# Web Framework + SSE
axum = { version = "0.7", features = ["json", "macros"] }
axum-extra = { version = "0.9", features = ["cookie"] }
tokio = { version = "1", features = ["full"] }
tower = "0.4"
tower-http = { version = "0.5", features = ["cors", "trace"] }

# Redis
redis = { version = "0.27", features = ["tokio-comp", "aio", "streams"] }

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Auth
jsonwebtoken = { version = "10.2", features = ["rust_crypto"] }

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json", "time"] }
time = { version = "0.3", features = ["formatting"] }

# Monitoring
axum-prometheus = "0.7"
metrics = "0.24"
metrics-exporter-prometheus = "0.16"

# Date/Time
chrono = { version = "0.4", features = ["serde"] }

# UUID
uuid = { version = "1.0", features = ["v4", "serde"] }

# Error
thiserror = "1.0"

# Environment
dotenvy = "0.15"

# Async
futures = "0.3"
tokio-stream = "0.1"
pin-project-lite = "0.2"

[dev-dependencies]
tokio-test = "0.4"
tower = { version = "0.5", features = ["util"] }
http-body-util = "0.1"
hyper = "1.0"
testcontainers = "0.23"
testcontainers-modules = { version = "0.11", features = ["redis"] }
fake = { version = "3.0", features = ["derive"] }
```

---

## 3. 프로젝트 구조

```
realtime-notification-hub/
├── Cargo.toml
├── Cargo.lock
├── Dockerfile
├── .env.example
├── docs/
│   ├── 1-pager.md                     # 1-pager (원본)
│   └── tech-spec.md                   # 이 문서
├── lua/
│   ├── lock_acquire.lua              # 분산 락 획득
│   ├── lock_release.lua              # 분산 락 해제
│   └── write_behind_flush.lua        # 읽음 상태 배치 flush
├── src/
│   ├── main.rs                       # 엔트리포인트, 라우터 구성
│   ├── lib.rs                        # 모듈 선언
│   ├── state.rs                      # AppState 정의
│   │
│   ├── config/
│   │   ├── mod.rs
│   │   └── app_config.rs             # 환경 변수 기반 설정
│   │
│   ├── auth/
│   │   ├── mod.rs
│   │   ├── jwt.rs                    # JWT Claims, decode_access_token
│   │   └── extractor.rs             # AuthUser FromRequestParts 구현
│   │
│   ├── error/
│   │   ├── mod.rs
│   │   └── app_error.rs             # AppError enum, IntoResponse 구현
│   │
│   ├── response/
│   │   ├── mod.rs
│   │   └── base.rs                  # BaseResponse, ErrorResponse
│   │
│   ├── sse/
│   │   ├── mod.rs
│   │   ├── handler.rs               # GET /rooms/{roomId}/stream 핸들러
│   │   ├── connection_manager.rs     # 방별 SSE 커넥션 관리
│   │   └── event.rs                 # SSE 이벤트 타입 정의
│   │
│   ├── notification/
│   │   ├── mod.rs
│   │   ├── handler.rs               # POST /notifications/publish 등
│   │   ├── dto.rs                   # Request/Response 구조체
│   │   └── service.rs              # 알림 발행 비즈니스 로직
│   │
│   ├── presence/
│   │   ├── mod.rs
│   │   ├── handler.rs               # GET /presence, POST /heartbeat
│   │   ├── dto.rs                   # Presence 관련 DTO
│   │   └── manager.rs              # Redis Hash 기반 접속자 관리
│   │
│   ├── redis_pubsub/
│   │   ├── mod.rs
│   │   └── listener.rs             # Pub/Sub 구독 + 이벤트 라우팅
│   │
│   ├── redis_streams/
│   │   ├── mod.rs
│   │   └── logger.rs               # XADD/XRANGE 이벤트 로그
│   │
│   ├── distributed_lock/
│   │   ├── mod.rs
│   │   └── lock.rs                 # Lua 스크립트 기반 분산 락
│   │
│   ├── write_behind/
│   │   ├── mod.rs
│   │   └── flusher.rs              # 읽음 상태 배치 flush
│   │
│   └── monitoring/
│       ├── mod.rs
│       └── metrics.rs              # 커스텀 Prometheus 메트릭
│
└── tests/
    ├── common/
    │   └── mod.rs                   # 테스트 헬퍼 (Redis 컨테이너 등)
    ├── sse_test.rs                  # SSE 연결 통합 테스트
    ├── pubsub_test.rs              # Pub/Sub 통합 테스트
    ├── presence_test.rs            # Presence 통합 테스트
    ├── lock_test.rs                # 분산 락 통합 테스트
    └── write_behind_test.rs        # Write-Behind 통합 테스트
```

---

## 4. 핵심 모듈 설계

### 4.1 SSE Connection Manager

방(room) 단위로 SSE 연결을 추적하고, 이벤트 수신 시 해당 방의 모든 연결에 팬아웃한다.

#### 4.1.1 핵심 자료구조

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// 방별 SSE 브로드캐스트 채널을 관리하는 구조체.
/// 각 방에 대해 하나의 broadcast::Sender를 유지하며,
/// 새 SSE 연결이 들어오면 해당 Sender에서 Receiver를 복제한다.
#[derive(Clone)]
pub struct ConnectionManager {
    /// room_id -> broadcast::Sender<SseEvent>
    /// RwLock: 방 목록 수정 시에만 write lock, 이벤트 전송 시에는 read lock
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<SseEvent>>>>,
    /// 방당 브로드캐스트 채널 용량 (기본: 256)
    channel_capacity: usize,
}

/// SSE로 전송될 이벤트
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SseEvent {
    /// 이벤트 타입 (예: "retrospect.submitted", "comment.created")
    pub event_type: String,
    /// JSON 직렬화된 페이로드
    pub data: serde_json::Value,
    /// Redis Stream ID (catch-up 용). None이면 실시간 이벤트
    pub stream_id: Option<String>,
}
```

#### 4.1.2 핵심 메서드

```rust
impl ConnectionManager {
    /// 새 ConnectionManager 생성.
    /// channel_capacity: 방당 브로드캐스트 버퍼 크기
    pub fn new(channel_capacity: usize) -> Self {
        Self {
            rooms: Arc::new(RwLock::new(HashMap::new())),
            channel_capacity,
        }
    }

    /// 특정 방에 대한 SSE Receiver를 얻는다.
    /// 해당 방의 채널이 없으면 새로 생성한다.
    pub async fn subscribe(&self, room_id: &str) -> broadcast::Receiver<SseEvent> {
        let mut rooms = self.rooms.write().await;
        let sender = rooms
            .entry(room_id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(self.channel_capacity);
                tx
            });
        sender.subscribe()
    }

    /// 특정 방의 모든 SSE 연결에 이벤트를 팬아웃한다.
    /// 수신자가 없으면 (Sender만 남은 경우) 방을 정리한다.
    pub async fn broadcast(&self, room_id: &str, event: SseEvent) -> usize {
        let rooms = self.rooms.read().await;
        if let Some(sender) = rooms.get(room_id) {
            match sender.send(event) {
                Ok(count) => count,
                Err(_) => 0, // 수신자 없음
            }
        } else {
            0
        }
    }

    /// 빈 방(수신자 0)을 주기적으로 정리한다.
    /// 60초마다 백그라운드 태스크에서 호출.
    pub async fn cleanup_empty_rooms(&self) {
        let mut rooms = self.rooms.write().await;
        rooms.retain(|_room_id, sender| sender.receiver_count() > 0);
    }

    /// 현재 전체 SSE 연결 수 (모니터링용).
    pub async fn total_connections(&self) -> usize {
        let rooms = self.rooms.read().await;
        rooms.values().map(|s| s.receiver_count()).sum()
    }

    /// 현재 활성 방 수 (모니터링용).
    pub async fn active_rooms(&self) -> usize {
        let rooms = self.rooms.read().await;
        rooms.len()
    }
}
```

#### 4.1.3 SSE 핸들러 흐름

```
Client: GET /api/v1/rooms/{roomId}/stream
           │
           ▼
    [JWT 검증 (AuthUser extractor)]
           │
           ▼
    [ConnectionManager.subscribe(roomId)]
           │
           ▼
    [Redis Streams XRANGE로 catch-up 이벤트 전송]
           │
           ▼
    [broadcast::Receiver를 Sse<Event> 스트림으로 변환]
           │
           ▼
    [무한 루프: receiver.recv() -> axum::sse::Event 전송]
```

### 4.2 Redis Pub/Sub Listener

서비스 시작 시 백그라운드 태스크로 Redis Pub/Sub를 구독한다.
수신된 메시지를 역직렬화하여 ConnectionManager에 전달한다.

#### 4.2.1 구독 패턴

```
# 패턴 구독 (방 ID가 동적이므로)
PSUBSCRIBE room:*

# 수신되는 메시지 형식
channel: room:{roomId}
message: {
  "eventType": "retrospect.submitted",
  "roomId": "42",
  "data": { ... },
  "timestamp": "2026-02-16T10:30:00Z"
}
```

#### 4.2.2 Listener 구현

```rust
use redis::aio::PubSub;

/// Redis Pub/Sub 채널에서 메시지를 수신하여
/// ConnectionManager로 팬아웃하는 백그라운드 태스크.
pub struct PubSubListener {
    redis_url: String,
    connection_manager: ConnectionManager,
}

/// Pub/Sub 채널을 통해 전달되는 메시지 구조.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PubSubMessage {
    pub event_type: String,
    pub room_id: String,
    pub data: serde_json::Value,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl PubSubListener {
    /// 백그라운드 태스크 시작.
    /// 연결 끊김 시 자동 재연결 (지수 백오프).
    pub async fn start(self) {
        loop {
            match self.run_subscription().await {
                Ok(()) => {
                    tracing::warn!("Pub/Sub 구독이 정상 종료됨. 재연결 시도...");
                }
                Err(e) => {
                    tracing::error!("Pub/Sub 에러: {}. 3초 후 재연결...", e);
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
    }

    async fn run_subscription(&self) -> Result<(), redis::RedisError> {
        let client = redis::Client::open(self.redis_url.as_str())?;
        let mut pubsub = client.get_async_pubsub().await?;

        // 패턴 구독: room:* 채널
        pubsub.psubscribe("room:*").await?;
        tracing::info!("Redis Pub/Sub 구독 시작: room:*");

        let mut stream = pubsub.on_message();

        while let Some(msg) = futures::StreamExt::next(&mut stream).await {
            let channel: String = msg.get_channel_name().to_string();
            let payload: String = match msg.get_payload() {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Pub/Sub 페이로드 파싱 실패: {}", e);
                    continue;
                }
            };

            // channel 형식: "room:{roomId}"
            let room_id = match channel.strip_prefix("room:") {
                Some(id) => id.to_string(),
                None => {
                    tracing::warn!("알 수 없는 채널: {}", channel);
                    continue;
                }
            };

            match serde_json::from_str::<PubSubMessage>(&payload) {
                Ok(message) => {
                    let event = SseEvent {
                        event_type: message.event_type,
                        data: message.data,
                        stream_id: None,
                    };
                    let count = self.connection_manager.broadcast(&room_id, event).await;
                    tracing::debug!(
                        room_id = %room_id,
                        receivers = count,
                        "이벤트 팬아웃 완료"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        channel = %channel,
                        "Pub/Sub 메시지 역직렬화 실패: {}",
                        e
                    );
                }
            }
        }

        Ok(())
    }
}
```

### 4.3 Redis Streams Logger

모든 이벤트를 Redis Streams에 영속 저장한다.
클라이언트가 재연결 시 `since` 파라미터로 놓친 이벤트를 XRANGE 조회한다.

#### 4.3.1 스트림 키 패턴

```
room:events:{roomId}
```

#### 4.3.2 구현

```rust
use redis::AsyncCommands;

pub struct StreamLogger {
    redis_pool: redis::aio::MultiplexedConnection,
}

/// Redis Stream에 저장되는 이벤트 엔트리의 필드 구조.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEntry {
    /// 이벤트 타입 (예: "retrospect.submitted")
    pub event_type: String,
    /// JSON 직렬화된 이벤트 페이로드
    pub payload: String,
    /// 발행자 user_id
    pub publisher_id: String,
    /// 이벤트 발생 시각 (ISO 8601)
    pub timestamp: String,
}

impl StreamLogger {
    /// 이벤트를 Redis Stream에 추가한다.
    /// 반환값: Redis Stream ID (예: "1708070400000-0")
    ///
    /// 스트림당 최대 10,000개 엔트리 유지 (MAXLEN ~ 10000).
    /// 근사치(~) 사용으로 성능 최적화.
    pub async fn log_event(
        &self,
        room_id: &str,
        entry: &StreamEntry,
    ) -> Result<String, redis::RedisError> {
        let key = format!("room:events:{}", room_id);

        let stream_id: String = redis::cmd("XADD")
            .arg(&key)
            .arg("MAXLEN")
            .arg("~")
            .arg("10000")
            .arg("*") // 자동 ID 생성
            .arg("type")
            .arg(&entry.event_type)
            .arg("payload")
            .arg(&entry.payload)
            .arg("publisher_id")
            .arg(&entry.publisher_id)
            .arg("timestamp")
            .arg(&entry.timestamp)
            .query_async(&mut self.redis_pool.clone())
            .await?;

        Ok(stream_id)
    }

    /// 특정 Stream ID 이후의 이벤트를 조회한다 (catch-up).
    /// since_id: 마지막으로 수신한 Stream ID. 이 ID 이후의 이벤트만 반환.
    /// count: 최대 반환 개수 (기본 100, 최대 500)
    pub async fn get_events_since(
        &self,
        room_id: &str,
        since_id: &str,
        count: usize,
    ) -> Result<Vec<(String, StreamEntry)>, redis::RedisError> {
        let key = format!("room:events:{}", room_id);
        let count = count.min(500);

        // XRANGE key (since_id "+inf" 범위에서 since_id 제외를 위해
        // since_id 직후부터 조회하려면 exclusive range 사용
        let results: redis::streams::StreamRangeReply = redis::cmd("XRANGE")
            .arg(&key)
            .arg(format!("({}", since_id)) // ( prefix = exclusive
            .arg("+")
            .arg("COUNT")
            .arg(count)
            .query_async(&mut self.redis_pool.clone())
            .await?;

        let entries = results
            .ids
            .into_iter()
            .filter_map(|id| {
                let stream_id = id.id.clone();
                let event_type = id.map.get("type")?.as_str()?.to_string();
                let payload = id.map.get("payload")?.as_str()?.to_string();
                let publisher_id = id.map.get("publisher_id")?.as_str()?.to_string();
                let timestamp = id.map.get("timestamp")?.as_str()?.to_string();

                Some((
                    stream_id,
                    StreamEntry {
                        event_type,
                        payload,
                        publisher_id,
                        timestamp,
                    },
                ))
            })
            .collect();

        Ok(entries)
    }
}
```

### 4.4 Presence Manager

Redis Hash + TTL 기반으로 방별 접속자를 실시간 추적한다.

#### 4.4.1 설계 원칙

- 클라이언트는 SSE 연결 후 30초마다 heartbeat를 전송한다.
- 서버는 `HSET presence:{roomId} {userId} {timestamp}`를 실행하고, `EXPIRE presence:{roomId} 60`으로 갱신한다.
- 60초 내에 heartbeat가 없으면 Hash 자체가 만료되어 해당 방의 모든 접속 정보가 초기화된다.
- 개별 사용자 만료는 HSET 시 기록된 timestamp를 비교하여 판단한다.

#### 4.4.2 구현

```rust
pub struct PresenceManager {
    redis_pool: redis::aio::MultiplexedConnection,
    /// 개별 사용자 heartbeat 만료 시간 (초). 기본 60.
    user_ttl_secs: u64,
    /// Hash 전체 TTL (초). 기본 120. 방에 아무도 없으면 키 자동 삭제.
    hash_ttl_secs: u64,
}

/// 접속 현황 응답 DTO.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceInfo {
    /// 현재 접속 중인 사용자 ID 목록
    pub online_users: Vec<i64>,
    /// 총 접속자 수
    pub count: usize,
}

impl PresenceManager {
    /// heartbeat 처리: 사용자의 접속 상태를 갱신한다.
    /// 반환: 갱신 전 해당 사용자의 접속 여부 (false면 새로 접속)
    pub async fn heartbeat(
        &self,
        room_id: &str,
        user_id: i64,
    ) -> Result<bool, redis::RedisError> {
        let key = format!("presence:{}", room_id);
        let now = chrono::Utc::now().timestamp();

        // Pipeline: HSET + EXPIRE (원자적 실행)
        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("HEXISTS").arg(&key).arg(user_id.to_string())
            .cmd("HSET").arg(&key).arg(user_id.to_string()).arg(now.to_string())
            .cmd("EXPIRE").arg(&key).arg(self.hash_ttl_secs);

        let results: (bool, i32, bool) = pipe
            .query_async(&mut self.redis_pool.clone())
            .await?;

        Ok(results.0) // HEXISTS 결과: true = 기존 접속자, false = 신규 접속
    }

    /// 특정 방의 접속자 현황을 조회한다.
    /// 만료된 사용자(timestamp + user_ttl_secs < now)는 제외하고,
    /// 해당 엔트리를 HDEL로 정리한다.
    pub async fn get_presence(
        &self,
        room_id: &str,
    ) -> Result<PresenceInfo, redis::RedisError> {
        let key = format!("presence:{}", room_id);
        let now = chrono::Utc::now().timestamp();

        let all: HashMap<String, String> = redis::cmd("HGETALL")
            .arg(&key)
            .query_async(&mut self.redis_pool.clone())
            .await?;

        let mut online = Vec::new();
        let mut expired = Vec::new();

        for (user_id_str, ts_str) in &all {
            let ts: i64 = ts_str.parse().unwrap_or(0);
            if now - ts < self.user_ttl_secs as i64 {
                if let Ok(uid) = user_id_str.parse::<i64>() {
                    online.push(uid);
                }
            } else {
                expired.push(user_id_str.clone());
            }
        }

        // 만료된 엔트리 정리
        if !expired.is_empty() {
            let mut pipe = redis::pipe();
            for user_id_str in &expired {
                pipe.cmd("HDEL").arg(&key).arg(user_id_str);
            }
            let _: () = pipe
                .query_async(&mut self.redis_pool.clone())
                .await?;
        }

        let count = online.len();
        Ok(PresenceInfo {
            online_users: online,
            count,
        })
    }

    /// 사용자 퇴장 처리 (SSE 연결 종료 시 호출).
    pub async fn leave(
        &self,
        room_id: &str,
        user_id: i64,
    ) -> Result<(), redis::RedisError> {
        let key = format!("presence:{}", room_id);
        let _: () = redis::cmd("HDEL")
            .arg(&key)
            .arg(user_id.to_string())
            .query_async(&mut self.redis_pool.clone())
            .await?;
        Ok(())
    }
}
```

### 4.5 Distributed Lock

Lua 스크립트 기반 분산 락으로 동일 이벤트의 중복 처리를 방지한다.

#### 4.5.1 Lua 스크립트

**`lua/lock_acquire.lua`**:
```lua
-- 분산 락 획득
-- KEYS[1]: 락 키
-- ARGV[1]: 소유자 ID (UUID)
-- ARGV[2]: TTL (초)
-- 반환: 1 (성공) / 0 (실패)
local key = KEYS[1]
local owner = ARGV[1]
local ttl = tonumber(ARGV[2])
if redis.call('SET', key, owner, 'NX', 'EX', ttl) then
    return 1
end
return 0
```

**`lua/lock_release.lua`**:
```lua
-- 분산 락 해제 (소유자 검증)
-- KEYS[1]: 락 키
-- ARGV[1]: 소유자 ID
-- 반환: 1 (해제 성공) / 0 (소유자 불일치 또는 락 없음)
if redis.call('GET', KEYS[1]) == ARGV[1] then
    return redis.call('DEL', KEYS[1])
end
return 0
```

#### 4.5.2 구현

```rust
pub struct DistributedLock {
    redis_pool: redis::aio::MultiplexedConnection,
    /// 락 획득 Lua 스크립트 (컴파일된 SHA)
    acquire_script: redis::Script,
    /// 락 해제 Lua 스크립트 (컴파일된 SHA)
    release_script: redis::Script,
}

/// 락 획득 결과. 성공 시 LockGuard를 반환하며,
/// Drop 시 자동 해제하지 않으므로 명시적으로 release()를 호출해야 한다.
pub struct LockGuard {
    key: String,
    owner_id: String,
}

impl DistributedLock {
    pub fn new(redis_pool: redis::aio::MultiplexedConnection) -> Self {
        let acquire_script = redis::Script::new(include_str!("../../lua/lock_acquire.lua"));
        let release_script = redis::Script::new(include_str!("../../lua/lock_release.lua"));
        Self {
            redis_pool,
            acquire_script,
            release_script,
        }
    }

    /// 락 획득 시도.
    /// key: 락 키 (예: "lock:event:{eventId}")
    /// ttl_secs: 락 자동 만료 시간 (초). 기본 10초 권장.
    /// 반환: Some(LockGuard) = 획득 성공, None = 이미 다른 소유자가 보유 중
    pub async fn acquire(
        &self,
        key: &str,
        ttl_secs: u64,
    ) -> Result<Option<LockGuard>, redis::RedisError> {
        let owner_id = uuid::Uuid::new_v4().to_string();

        let result: i32 = self
            .acquire_script
            .key(key)
            .arg(&owner_id)
            .arg(ttl_secs)
            .invoke_async(&mut self.redis_pool.clone())
            .await?;

        if result == 1 {
            Ok(Some(LockGuard {
                key: key.to_string(),
                owner_id,
            }))
        } else {
            Ok(None)
        }
    }

    /// 락 해제.
    /// guard: acquire()에서 반환된 LockGuard.
    /// 반환: true = 해제 성공, false = 소유자 불일치 (이미 만료되었을 수 있음)
    pub async fn release(
        &self,
        guard: LockGuard,
    ) -> Result<bool, redis::RedisError> {
        let result: i32 = self
            .release_script
            .key(&guard.key)
            .arg(&guard.owner_id)
            .invoke_async(&mut self.redis_pool.clone())
            .await?;

        Ok(result == 1)
    }
}
```

### 4.6 Write-Behind Flusher

읽음 상태(read status)를 즉시 DB에 쓰지 않고, Redis에 먼저 기록한 후 배치로 DB에 flush한다.

#### 4.6.1 설계

```
PATCH /notifications/{id}/read
         │
         ▼
  Redis HSET read_status:{userId} {notificationId} {timestamp}
         │
         ▼ (30초마다 백그라운드 태스크)
  배치 flush: HGETALL read_status:{userId}
         │
         ▼
  DB UPDATE (moalog-server API 호출 또는 직접 DB)
         │
         ▼
  Redis HDEL read_status:{userId} {flushed_ids...}
```

#### 4.6.2 구현

```rust
pub struct WriteBehindFlusher {
    redis_pool: redis::aio::MultiplexedConnection,
    /// flush 주기 (초). 기본 30.
    flush_interval_secs: u64,
    /// 1회 flush 시 최대 처리 건수. 기본 200.
    batch_size: usize,
}

impl WriteBehindFlusher {
    /// 백그라운드 flush 루프를 시작한다.
    /// 종료 시그널(shutdown_rx)을 받으면 마지막 flush 후 종료.
    pub async fn start(self, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(
            std::time::Duration::from_secs(self.flush_interval_secs)
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.flush_all().await {
                        tracing::error!("Write-behind flush 실패: {}", e);
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::info!("Write-behind flusher 종료 신호 수신. 마지막 flush...");
                    let _ = self.flush_all().await;
                    break;
                }
            }
        }
    }

    /// 모든 read_status:* 키를 스캔하여 배치 flush.
    async fn flush_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        // SCAN으로 read_status:* 키 탐색
        let mut cursor = 0u64;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("read_status:*")
                .arg("COUNT")
                .arg(100)
                .query_async(&mut self.redis_pool.clone())
                .await?;

            for key in keys {
                self.flush_user_reads(&key).await?;
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(())
    }

    /// 개별 사용자의 읽음 상태를 flush.
    async fn flush_user_reads(
        &self,
        key: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let all: HashMap<String, String> = redis::cmd("HGETALL")
            .arg(key)
            .query_async(&mut self.redis_pool.clone())
            .await?;

        if all.is_empty() {
            return Ok(());
        }

        let entries: Vec<(&String, &String)> = all.iter().take(self.batch_size).collect();
        let notification_ids: Vec<&String> = entries.iter().map(|(id, _)| *id).collect();

        // TODO: DB에 배치 UPDATE 수행
        // 현재는 moalog-server API 호출 또는 직접 DB 연결
        tracing::info!(
            key = %key,
            count = notification_ids.len(),
            "읽음 상태 flush 완료"
        );

        // flush 성공한 엔트리만 Redis에서 삭제
        let mut pipe = redis::pipe();
        for id in &notification_ids {
            pipe.cmd("HDEL").arg(key).arg(id.as_str());
        }
        let _: () = pipe
            .query_async(&mut self.redis_pool.clone())
            .await?;

        Ok(())
    }
}
```

---

## 5. 데이터 모델

### 5.1 Redis 키 패턴 종합

| 패턴 | 타입 | TTL | 용도 | 예시 |
|------|------|-----|------|------|
| `room:{roomId}` | Pub/Sub Channel | - | 실시간 이벤트 팬아웃 | `room:42` |
| `room:events:{roomId}` | Stream | MAXLEN ~10000 | 이벤트 영속 로그 | `room:events:42` |
| `presence:{roomId}` | Hash | 120s (갱신) | 방별 접속자 추적 | `presence:42` |
| `read_status:{userId}` | Hash | - | 알림 읽음 상태 (Write-Behind) | `read_status:1` |
| `lock:event:{eventId}` | String | 10s | 중복 이벤트 처리 방지 | `lock:event:abc-123` |
| `unread_count:{userId}` | String | - | 미읽은 알림 수 캐시 | `unread_count:1` |
| `notif:settings:{userId}` | Hash | 3600s | 사용자 알림 설정 캐시 | `notif:settings:1` |

### 5.2 상세 키 구조

#### 5.2.1 Pub/Sub Channel: `room:{roomId}`

```json
// PUBLISH room:42
{
  "eventType": "retrospect.submitted",
  "roomId": "42",
  "data": {
    "userId": 1,
    "userName": "김철수",
    "retrospectId": 100,
    "retrospectTitle": "Sprint 15 회고"
  },
  "timestamp": "2026-02-16T10:30:00Z"
}
```

#### 5.2.2 Stream: `room:events:{roomId}`

```
XADD room:events:42 MAXLEN ~ 10000 *
  type "retrospect.submitted"
  payload '{"userId":1,"userName":"김철수","retrospectId":100}'
  publisher_id "1"
  timestamp "2026-02-16T10:30:00Z"

# 결과 ID: 1708070400000-0
```

```
XRANGE room:events:42 (1708070400000-0 + COUNT 100
# 1708070400000-0 이후의 이벤트 최대 100개 반환
```

#### 5.2.3 Hash: `presence:{roomId}`

```
HSET presence:42 1 1708070400        # userId=1, timestamp
HSET presence:42 2 1708070405        # userId=2
EXPIRE presence:42 120               # 120초 후 전체 만료

HGETALL presence:42
# "1" -> "1708070400"
# "2" -> "1708070405"
```

#### 5.2.4 Hash: `read_status:{userId}`

```
HSET read_status:1 "notif-abc" "1708070500"
HSET read_status:1 "notif-def" "1708070510"

# flush 후:
HDEL read_status:1 "notif-abc" "notif-def"
```

#### 5.2.5 String: `lock:event:{eventId}`

```
SET lock:event:abc-123 "owner-uuid-xxx" NX EX 10
# 성공: OK / 실패: nil

GET lock:event:abc-123
# "owner-uuid-xxx"

# 해제 (Lua):
# if GET == owner then DEL
```

### 5.3 이벤트 타입 목록

| 이벤트 타입 | 발생 시점 | data 필드 |
|-------------|----------|-----------|
| `retrospect.submitted` | 회고 제출 완료 | userId, userName, retrospectId, retrospectTitle |
| `retrospect.created` | 새 회고 생성 | userId, userName, retrospectId, retrospectTitle |
| `comment.created` | 댓글 작성 | userId, userName, responseId, content (요약) |
| `like.toggled` | 좋아요 토글 | userId, userName, responseId, liked (boolean) |
| `presence.update` | 접속 현황 변경 | onlineUsers (id[]), joined (id?), left (id?) |
| `member.joined` | 방 참가 | userId, userName |

---

## 6. API 상세 설계

### 6.1 엔드포인트 요약

| Method | Path | 인증 | 설명 |
|--------|------|------|------|
| GET | `/api/v1/rooms/{roomId}/stream` | Required | SSE 실시간 스트림 |
| POST | `/api/v1/notifications/publish` | Internal | 이벤트 발행 (moalog-server -> hub) |
| GET | `/api/v1/rooms/{roomId}/events` | Required | 미수신 이벤트 조회 (catch-up) |
| GET | `/api/v1/rooms/{roomId}/presence` | Required | 접속자 현황 조회 |
| POST | `/api/v1/rooms/{roomId}/presence/heartbeat` | Required | Heartbeat 갱신 |
| GET | `/api/v1/notifications/unread` | Required | 미읽은 알림 수 |
| PATCH | `/api/v1/notifications/{notificationId}/read` | Required | 읽음 처리 |
| GET | `/health` | None | 헬스 체크 |
| GET | `/metrics` | None | Prometheus 메트릭 |

### 6.2 상세 스펙

---

#### 6.2.1 `GET /api/v1/rooms/{roomId}/stream`

SSE 실시간 스트림. 연결을 유지하며 이벤트가 발생할 때마다 전송한다.

**Path Parameters**:

```rust
#[derive(Debug, serde::Deserialize)]
pub struct RoomPath {
    pub room_id: i64,
}
```

**Query Parameters**:

```rust
/// SSE 스트림 연결 시 catch-up을 위한 쿼리 파라미터.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamQuery {
    /// 마지막으로 수신한 Redis Stream ID.
    /// 제공 시 해당 ID 이후의 이벤트를 먼저 전송 후 실시간 전환.
    pub last_event_id: Option<String>,
}
```

**Response** (SSE):

```
HTTP/1.1 200 OK
Content-Type: text/event-stream
Cache-Control: no-cache
Connection: keep-alive

event: retrospect.submitted
id: 1708070400000-0
data: {"userId":1,"userName":"김철수","retrospectId":100,"retrospectTitle":"Sprint 15 회고","timestamp":"2026-02-16T10:30:00Z"}

event: comment.created
id: 1708070460000-0
data: {"userId":2,"userName":"이영희","responseId":15,"content":"좋은 포인트!","timestamp":"2026-02-16T10:31:00Z"}

event: presence.update
data: {"onlineUsers":[1,2,3],"joined":3,"left":null,"count":3}

:keepalive
```

**에러 코드**:

| HTTP | 에러 코드 | 조건 |
|------|----------|------|
| 401 | `AUTH4001` | JWT 누락 또는 만료 |
| 404 | `ROOM4041` | 존재하지 않는 roomId |

**핸들러 구현**:

```rust
use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use std::convert::Infallible;

pub async fn stream_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(RoomPath { room_id }): Path<RoomPath>,
    Query(query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let user_id = auth_user.user_id()?;

    // 1. 방 존재 여부 검증 (선택적: moalog-server에 위임 가능)

    // 2. Presence에 사용자 등록
    state.presence_manager.heartbeat(&room_id.to_string(), user_id).await
        .map_err(|e| AppError::InternalError(format!("Presence 등록 실패: {}", e)))?;

    // 3. catch-up 이벤트 조회
    let catch_up_events = if let Some(ref last_id) = query.last_event_id {
        state.stream_logger
            .get_events_since(&room_id.to_string(), last_id, 100)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // 4. broadcast receiver 구독
    let mut receiver = state.connection_manager
        .subscribe(&room_id.to_string())
        .await;

    // 5. SSE 스트림 생성
    let stream = async_stream::stream! {
        // catch-up 이벤트 먼저 전송
        for (stream_id, entry) in catch_up_events {
            let event = Event::default()
                .event(&entry.event_type)
                .id(&stream_id)
                .data(&entry.payload);
            yield Ok(event);
        }

        // 실시간 이벤트 수신
        loop {
            match receiver.recv().await {
                Ok(sse_event) => {
                    let data = serde_json::to_string(&sse_event.data)
                        .unwrap_or_default();
                    let mut event = Event::default()
                        .event(&sse_event.event_type)
                        .data(data);
                    if let Some(ref id) = sse_event.stream_id {
                        event = event.id(id);
                    }
                    yield Ok(event);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        room_id = %room_id,
                        user_id = %user_id,
                        lagged = n,
                        "SSE receiver lagged, 일부 이벤트 누락"
                    );
                    // lagged 알림 이벤트 전송
                    let event = Event::default()
                        .event("system.lagged")
                        .data(format!("{{\"missed\":{}}}", n));
                    yield Ok(event);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keepalive"),
    ))
}
```

---

#### 6.2.2 `POST /api/v1/notifications/publish`

moalog-server에서 이벤트 발생 시 호출하는 내부 API.

**Request Body**:

```rust
/// 이벤트 발행 요청.
/// moalog-server가 회고 제출/댓글/좋아요 발생 시 호출.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRequest {
    /// 대상 방 ID
    pub room_id: i64,
    /// 이벤트 타입 (예: "retrospect.submitted")
    pub event_type: String,
    /// 이벤트 페이로드 (JSON)
    pub data: serde_json::Value,
    /// 발행자 user ID
    pub publisher_id: i64,
    /// 멱등성 키 (선택). 중복 발행 방지.
    pub idempotency_key: Option<String>,
}
```

**Response Body**:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResponse {
    /// Redis Stream ID
    pub stream_id: String,
    /// Pub/Sub로 수신한 연결 수
    pub delivered_to: usize,
}
```

**처리 흐름**:

```
1. idempotency_key가 있으면 분산 락 획득 시도
   - 획득 실패 → 이미 처리 중 → 200 OK (중복)
2. Redis Streams에 XADD (영속 저장)
3. Redis Pub/Sub에 PUBLISH (실시간 팬아웃)
4. unread_count 증가 (INCR unread_count:{각 대상 userId})
5. 분산 락 해제
6. 200 OK 응답
```

**에러 코드**:

| HTTP | 에러 코드 | 조건 |
|------|----------|------|
| 400 | `NOTIF4001` | 잘못된 event_type |
| 401 | `AUTH4001` | 인증 실패 (내부 API 키) |
| 500 | `COMMON500` | Redis 연결 실패 |

---

#### 6.2.3 `GET /api/v1/rooms/{roomId}/events`

SSE 연결 전 또는 재연결 시 놓친 이벤트를 조회한다.

**Query Parameters**:

```rust
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    /// 마지막으로 수신한 Redis Stream ID
    pub since: String,
    /// 최대 반환 개수 (기본 100, 최대 500)
    pub count: Option<usize>,
}
```

**Response Body**:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsResponse {
    pub events: Vec<EventItem>,
    /// 다음 조회 시 사용할 마지막 Stream ID
    pub last_id: Option<String>,
    /// 추가 이벤트 존재 여부
    pub has_more: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventItem {
    pub id: String,
    pub event_type: String,
    pub data: serde_json::Value,
    pub publisher_id: String,
    pub timestamp: String,
}
```

**에러 코드**:

| HTTP | 에러 코드 | 조건 |
|------|----------|------|
| 400 | `COMMON400` | since 파라미터 누락 또는 유효하지 않은 Stream ID |
| 401 | `AUTH4001` | JWT 누락/만료 |

---

#### 6.2.4 `GET /api/v1/rooms/{roomId}/presence`

방의 현재 접속자 목록을 반환한다.

**Response Body**:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceResponse {
    pub online_users: Vec<PresenceUser>,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceUser {
    pub user_id: i64,
    /// 마지막 heartbeat 시각
    pub last_seen: String,
}
```

---

#### 6.2.5 `POST /api/v1/rooms/{roomId}/presence/heartbeat`

클라이언트가 30초마다 호출하여 접속 상태를 갱신한다.

**Response Body**:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatResponse {
    /// heartbeat 갱신 성공 여부
    pub acknowledged: bool,
    /// 다음 heartbeat까지 권장 대기 시간 (초)
    pub next_in_secs: u64,
}
```

---

#### 6.2.6 `GET /api/v1/notifications/unread`

현재 사용자의 미읽은 알림 수를 반환한다.

**Response Body**:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCountResponse {
    pub count: i64,
}
```

---

#### 6.2.7 `PATCH /api/v1/notifications/{notificationId}/read`

알림을 읽음 처리한다. Write-Behind 패턴으로 Redis에 먼저 기록.

**Response Body**:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResponse {
    pub notification_id: String,
    pub read_at: String,
}
```

---

#### 6.2.8 `GET /health`

**Response Body**:

```rust
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub status: String,
    pub redis_connected: bool,
    pub active_connections: usize,
    pub active_rooms: usize,
    pub uptime_secs: u64,
}
```

---

## 7. 인증/인가

### 7.1 JWT 검증

moalog-server와 동일한 JWT secret을 공유하여 토큰을 검증한다.
별도의 사용자 DB 조회 없이, JWT Claims의 `sub` 필드(user ID)만으로 인증한다.

#### 7.1.1 Claims 구조 (moalog-server 호환)

```rust
/// moalog-server의 JWT Claims와 동일한 구조.
/// 이 서비스에서는 sub (user ID)와 token_type만 사용.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Claims {
    /// Subject (User ID). 문자열로 인코딩된 숫자.
    pub sub: String,
    /// Issued At (Unix timestamp)
    pub iat: usize,
    /// Expiration (Unix timestamp)
    pub exp: usize,
    /// JWT ID (Refresh Token 전용, 이 서비스에서는 무시)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
    /// Email (Signup Token 전용, 이 서비스에서는 무시)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Token Type: "access" | "refresh" | "signup"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    /// Social Provider (Signup Token 전용)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}
```

#### 7.1.2 AuthUser Extractor

```rust
use axum::{
    async_trait,
    extract::FromRequestParts,
    http::header::{AUTHORIZATION, COOKIE},
    http::request::Parts,
};

/// 인증된 사용자 정보를 담는 Extractor.
/// moalog-server의 AuthUser와 동일한 로직.
pub struct AuthUser(pub Claims);

impl AuthUser {
    pub fn user_id(&self) -> Result<i64, AppError> {
        self.0
            .sub
            .parse()
            .map_err(|_| AppError::Unauthorized("유효하지 않은 사용자 ID입니다.".to_string()))
    }
}

#[async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 1. Authorization: Bearer {token} 헤더 확인
        let token = if let Some(auth_header) = parts.headers.get(AUTHORIZATION) {
            let auth_str = auth_header
                .to_str()
                .map_err(|_| AppError::Unauthorized("잘못된 헤더 형식입니다.".to_string()))?;

            if !auth_str.starts_with("Bearer ") {
                return Err(AppError::Unauthorized(
                    "토큰 형식이 올바르지 않습니다.".to_string(),
                ));
            }
            auth_str[7..].to_string()
        } else {
            // 2. Cookie: access_token={token} 확인
            extract_token_from_cookie(parts)?
        };

        // 3. Access Token 전용 검증
        let claims = decode_access_token(&token, &state.config.jwt_secret)?;
        Ok(AuthUser(claims))
    }
}

const ACCESS_TOKEN_COOKIE: &str = "access_token";

fn extract_token_from_cookie(parts: &Parts) -> Result<String, AppError> {
    let cookie_header = parts
        .headers
        .get(COOKIE)
        .ok_or_else(|| AppError::Unauthorized("로그인이 필요합니다.".to_string()))?;

    let cookie_str = cookie_header
        .to_str()
        .map_err(|_| AppError::Unauthorized("잘못된 쿠키 형식입니다.".to_string()))?;

    for cookie in cookie_str.split(';') {
        let cookie = cookie.trim();
        if let Some(value) = cookie.strip_prefix(&format!("{}=", ACCESS_TOKEN_COOKIE)) {
            if !value.is_empty() {
                return Ok(value.to_string());
            }
        }
    }

    Err(AppError::Unauthorized("로그인이 필요합니다.".to_string()))
}
```

### 7.2 내부 API 인증

`POST /api/v1/notifications/publish`는 moalog-server에서만 호출하는 내부 API이다.
공유 시크릿(`INTERNAL_API_KEY`) 기반의 `X-Internal-Key` 헤더로 인증한다.

```rust
/// 내부 API 인증 미들웨어.
/// X-Internal-Key 헤더 값이 환경변수 INTERNAL_API_KEY와 일치하는지 검증.
pub async fn internal_auth_middleware(
    State(state): State<AppState>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, AppError> {
    let api_key = request
        .headers()
        .get("X-Internal-Key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized("내부 API 키가 필요합니다.".to_string()))?;

    if api_key != state.config.internal_api_key {
        return Err(AppError::Unauthorized("유효하지 않은 API 키입니다.".to_string()));
    }

    Ok(next.run(request).await)
}
```

### 7.3 SSE 연결의 인증

SSE는 `EventSource` API 특성상 커스텀 헤더를 설정할 수 없다.
따라서 두 가지 방법을 지원한다:

1. **쿠키 기반** (권장): `access_token` 쿠키에 JWT 포함
2. **쿼리 파라미터** (폴백): `?token={jwt}` (보안상 쿠키를 권장)

```rust
// SSE 전용: 쿼리 파라미터에서도 토큰을 추출하는 확장 extractor
#[derive(Debug, serde::Deserialize)]
pub struct SseAuthQuery {
    pub token: Option<String>,
}
```

---

## 8. 에러 처리

### 8.1 AppError 정의

moalog-server의 에러 패턴과 동일하게, enum 기반 에러 타입을 사용한다.

```rust
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // ─── 공통 ──────────────────────────────
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

    // ─── Redis ─────────────────────────────
    #[error("Redis 연결 실패: {0}")]
    RedisConnectionError(String),

    #[error("Redis 명령 실패: {0}")]
    RedisCommandError(String),

    // ─── 알림 ──────────────────────────────
    #[error("알림 발행 실패: {0}")]
    PublishFailed(String),

    #[error("유효하지 않은 이벤트 타입: {0}")]
    InvalidEventType(String),

    #[error("중복 이벤트: {0}")]
    DuplicateEvent(String),

    // ─── SSE ───────────────────────────────
    #[error("SSE 연결 실패: {0}")]
    SseConnectionError(String),

    // ─── 방 ────────────────────────────────
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

        // 5xx 에러만 상세 로깅
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
```

### 8.2 에러 코드 매핑 종합

| 에러 코드 | HTTP | 설명 |
|-----------|------|------|
| `COMMON400` | 400 | 잘못된 요청 |
| `COMMON403` | 403 | 권한 없음 |
| `COMMON404` | 404 | 리소스 없음 |
| `COMMON500` | 500 | 서버 내부 오류 |
| `AUTH4001` | 401 | 인증 실패 (JWT) |
| `NOTIF4001` | 400 | 유효하지 않은 이벤트 타입 |
| `NOTIF4091` | 409 | 중복 이벤트 (멱등성) |
| `NOTIF5001` | 500 | 알림 발행 실패 |
| `REDIS5001` | 500 | Redis 연결 실패 |
| `REDIS5002` | 500 | Redis 명령 실패 |
| `SSE5001` | 500 | SSE 연결 실패 |
| `ROOM4041` | 404 | 방 없음 |

### 8.3 재시도 전략

| 컴포넌트 | 재시도 전략 | 최대 횟수 | 백오프 |
|---------|-----------|----------|--------|
| Redis Pub/Sub 연결 | 자동 재연결 | 무제한 | 지수 백오프 (3s, 6s, 12s, max 60s) |
| Redis 명령 실패 | 즉시 재시도 | 3 | 100ms 고정 |
| 이벤트 발행 실패 | 재시도 없음 | - | - (fire-and-forget) |
| Write-Behind flush | 다음 주기에 재시도 | - | flush_interval_secs |

---

## 9. 설정 관리

### 9.1 환경 변수

| 변수명 | 필수 | 기본값 | 설명 |
|--------|------|--------|------|
| `SERVER_PORT` | N | `8083` | 서버 포트 |
| `REDIS_URL` | N | `redis://:moalog_redis_local@localhost:6379` | Redis 연결 URL (비밀번호 포함) |
| `JWT_SECRET` | Y (prod) | `local_dev_secret` (dev) | JWT 시크릿 (moalog-server와 동일) |
| `INTERNAL_API_KEY` | N | `local_dev_internal_key` | 내부 API 인증 키 |
| `RUST_LOG` | N | `info,realtime_notification_hub=debug` | 로그 레벨 |
| `ALLOWED_ORIGINS` | N | `http://localhost:3000,http://localhost:5173` | CORS 허용 오리진 |
| `SSE_CHANNEL_CAPACITY` | N | `256` | 방당 브로드캐스트 채널 버퍼 크기 |
| `PRESENCE_USER_TTL_SECS` | N | `60` | 개별 사용자 heartbeat 만료 시간 |
| `PRESENCE_HASH_TTL_SECS` | N | `120` | 방 presence hash TTL |
| `WRITE_BEHIND_INTERVAL_SECS` | N | `30` | Write-Behind flush 주기 |
| `WRITE_BEHIND_BATCH_SIZE` | N | `200` | flush 1회 최대 처리 건수 |
| `STREAM_MAXLEN` | N | `10000` | 방당 스트림 최대 엔트리 수 |
| `LOCK_TTL_SECS` | N | `10` | 분산 락 기본 TTL |

### 9.2 AppConfig 구조체

```rust
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

#[derive(Debug, thiserror::Error)]
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
                tracing::warn!(
                    "JWT_SECRET 미설정. 개발 환경 기본값 사용."
                );
                "local_dev_secret".to_string()
            }
            Err(_) => return Err(ConfigError::MissingJwtSecret),
        };

        let internal_api_key = std::env::var("INTERNAL_API_KEY")
            .unwrap_or_else(|_| "local_dev_internal_key".to_string());

        let allowed_origins = std::env::var("ALLOWED_ORIGINS")
            .unwrap_or_else(|_| "http://localhost:3000,http://localhost:5173".to_string())
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
```

### 9.3 `.env.example`

```env
# Server
SERVER_PORT=8083

# Redis (moalog-server docker-compose의 Redis 공유)
REDIS_URL=redis://:moalog_redis_local@localhost:6382

# Auth (moalog-server와 동일한 JWT secret)
JWT_SECRET=local_dev_secret

# Internal API
INTERNAL_API_KEY=local_dev_internal_key

# CORS
ALLOWED_ORIGINS=http://localhost:3000,http://localhost:5173,http://localhost:5174

# Logging
RUST_LOG=info,realtime_notification_hub=debug

# SSE
SSE_CHANNEL_CAPACITY=256

# Presence
PRESENCE_USER_TTL_SECS=60
PRESENCE_HASH_TTL_SECS=120

# Write-Behind
WRITE_BEHIND_INTERVAL_SECS=30
WRITE_BEHIND_BATCH_SIZE=200

# Redis Streams
STREAM_MAXLEN=10000

# Distributed Lock
LOCK_TTL_SECS=10
```

---

## 10. 모니터링

### 10.1 Prometheus 메트릭

#### 10.1.1 자동 수집 메트릭 (axum-prometheus)

| 메트릭 | 타입 | 설명 |
|--------|------|------|
| `http_requests_total` | Counter | HTTP 요청 총 수 (method, path, status 라벨) |
| `http_request_duration_seconds` | Histogram | HTTP 요청 처리 시간 |

#### 10.1.2 커스텀 비즈니스 메트릭

| 메트릭 | 타입 | 설명 |
|--------|------|------|
| `sse_active_connections` | Gauge | 현재 활성 SSE 연결 수 |
| `sse_active_rooms` | Gauge | 현재 활성 방 수 |
| `events_published_total` | Counter | 발행된 이벤트 총 수 (event_type 라벨) |
| `events_delivered_total` | Counter | SSE로 전달된 이벤트 총 수 |
| `events_lagged_total` | Counter | broadcast lag으로 누락된 이벤트 수 |
| `pubsub_reconnect_total` | Counter | Pub/Sub 재연결 횟수 |
| `presence_heartbeat_total` | Counter | Heartbeat 처리 총 수 |
| `write_behind_flush_total` | Counter | Write-Behind flush 실행 횟수 |
| `write_behind_flush_entries` | Histogram | flush당 처리된 엔트리 수 |
| `lock_acquire_total` | Counter | 분산 락 획득 시도 (result=success/failure 라벨) |
| `redis_command_duration_seconds` | Histogram | Redis 명령 처리 시간 (command 라벨) |

#### 10.1.3 메트릭 등록

```rust
use metrics::{counter, gauge, histogram};

pub fn record_event_published(event_type: &str) {
    counter!("events_published_total", "event_type" => event_type.to_string()).increment(1);
}

pub fn record_sse_connection(delta: i64) {
    if delta > 0 {
        gauge!("sse_active_connections").increment(delta as f64);
    } else {
        gauge!("sse_active_connections").decrement((-delta) as f64);
    }
}

pub fn record_redis_command_duration(command: &str, duration: std::time::Duration) {
    histogram!("redis_command_duration_seconds", "command" => command.to_string())
        .record(duration.as_secs_f64());
}
```

### 10.2 Health Endpoint

`GET /health` 응답에 Redis 연결 상태, 활성 연결 수, 활성 방 수, uptime을 포함한다.

```rust
pub async fn health_handler(
    State(state): State<AppState>,
) -> Json<BaseResponse<HealthResponse>> {
    let redis_ok = state.redis_pool.get_async_connection().await.is_ok();
    let connections = state.connection_manager.total_connections().await;
    let rooms = state.connection_manager.active_rooms().await;
    let uptime = state.start_time.elapsed().as_secs();

    Json(BaseResponse::success(HealthResponse {
        status: if redis_ok { "healthy" } else { "degraded" }.to_string(),
        redis_connected: redis_ok,
        active_connections: connections,
        active_rooms: rooms,
        uptime_secs: uptime,
    }))
}
```

### 10.3 Grafana 대시보드 쿼리 예시

```promql
# 활성 SSE 연결 수
sse_active_connections

# 이벤트 발행률 (초당)
rate(events_published_total[1m])

# Redis 명령 p95 지연
histogram_quantile(0.95, rate(redis_command_duration_seconds_bucket[5m]))

# Pub/Sub 재연결 빈도
rate(pubsub_reconnect_total[5m])

# Write-Behind flush 당 처리 엔트리 평균
rate(write_behind_flush_entries_sum[5m]) / rate(write_behind_flush_entries_count[5m])
```

---

## 11. Docker

### 11.1 Dockerfile (Multi-Stage)

```dockerfile
# ============================================================
# Stage 1: Planner - 의존성 정보 추출
# ============================================================
FROM rust:1.85-bookworm AS planner

RUN cargo install cargo-chef --locked
WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ============================================================
# Stage 2: Builder - 의존성 캐시 빌드 + 소스 빌드
# ============================================================
FROM rust:1.85-bookworm AS builder

RUN cargo install cargo-chef --locked
WORKDIR /app

# 의존성만 먼저 빌드 (캐시 레이어)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Lua 스크립트 복사
COPY lua/ lua/

# 소스 복사 후 실제 빌드
COPY . .
RUN cargo build --release && strip target/release/realtime-notification-hub

# ============================================================
# Stage 3: Runtime - 최소 런타임 이미지
# ============================================================
FROM debian:bookworm-slim AS runtime

# 런타임 의존성 설치
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# 보안: non-root 사용자
RUN groupadd --gid 1001 appuser && \
    useradd --uid 1001 --gid appuser --create-home appuser

WORKDIR /app

# 바이너리 복사
COPY --from=builder /app/target/release/realtime-notification-hub /app/notification-hub

# Lua 스크립트 복사 (include_str!로 컴파일에 포함되지만, 디버깅용으로 보관)
COPY --from=builder /app/lua/ /app/lua/

# 소유권 설정
RUN chown -R appuser:appuser /app

USER appuser

# 환경변수 기본값
ENV SERVER_PORT=8080
ENV RUST_LOG=info

EXPOSE 8080

# 헬스체크 (slim 이미지에는 curl 없음 - /dev/tcp 사용)
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD bash -c 'exec 3<>/dev/tcp/localhost/8080 && echo -e "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n" >&3 && cat <&3 | grep -q healthy'

ENTRYPOINT ["/app/notification-hub"]
```

### 11.2 docker-compose.yaml 추가 서비스

moalog-server의 기존 `docker-compose.yaml`에 아래 서비스를 추가한다:

```yaml
  # --- Real-time Notification Hub ---------------------
  notification-hub:
    build:
      context: ../realtime-notification-hub
      dockerfile: Dockerfile
    container_name: moalog-notification-hub
    depends_on:
      redis:
        condition: service_healthy
      moalog-server:
        condition: service_healthy
    environment:
      SERVER_PORT: 8080
      REDIS_URL: redis://:${REDIS_PASSWORD:-moalog_redis_local}@redis:6379
      JWT_SECRET: ${JWT_SECRET:-local_dev_secret}
      INTERNAL_API_KEY: ${INTERNAL_API_KEY:-local_dev_internal_key}
      ALLOWED_ORIGINS: ${ALLOWED_ORIGINS:-http://localhost:3000,http://localhost:5173,http://localhost:5174}
      RUST_LOG: ${RUST_LOG_NOTIFICATION:-info,realtime_notification_hub=debug}
      SSE_CHANNEL_CAPACITY: 256
      PRESENCE_USER_TTL_SECS: 60
      PRESENCE_HASH_TTL_SECS: 120
      WRITE_BEHIND_INTERVAL_SECS: 30
      STREAM_MAXLEN: 10000
    ports:
      - "${NOTIFICATION_HUB_PORT:-8083}:8080"
    healthcheck:
      test: ["CMD-SHELL", "bash -c 'exec 3<>/dev/tcp/localhost/8080 && echo -e \"GET /health HTTP/1.1\\r\\nHost: localhost\\r\\nConnection: close\\r\\n\\r\\n\" >&3 && cat <&3 | grep -q healthy'"]
      interval: 15s
      timeout: 5s
      start_period: 15s
      retries: 3
    restart: unless-stopped
```

**포트 배정**: `8083` (호스트) -> `8080` (컨테이너 내부)

기존 포트 사용 현황:
- 8080: moalog-server (가변, SERVER_PORT)
- 8081: FluxPay Engine
- 8082: Rate Limiter
- **8083: Notification Hub (신규)**

### 11.3 Prometheus 스크레이프 설정 추가

`monitoring/prometheus.yml`에 추가:

```yaml
  - job_name: 'notification-hub'
    scrape_interval: 15s
    static_configs:
      - targets: ['notification-hub:8080']
    metrics_path: '/metrics'
```

---

## 12. 테스트 전략

### 12.1 단위 테스트

| 대상 | 테스트 항목 | 위치 |
|------|-----------|------|
| `SseEvent` 직렬화 | JSON 포맷 검증, camelCase 변환 | `src/sse/event.rs` |
| JWT Claims | 토큰 디코딩 성공/실패, 만료, 잘못된 타입 | `src/auth/jwt.rs` |
| AppConfig | 환경변수 파싱, 기본값, 누락 시 에러 | `src/config/app_config.rs` |
| AppError | 에러 코드 매핑, HTTP 상태 코드, IntoResponse | `src/error/app_error.rs` |
| PubSubMessage | 역직렬화, 잘못된 JSON 처리 | `src/redis_pubsub/listener.rs` |
| StreamEntry | 직렬화/역직렬화 왕복 | `src/redis_streams/logger.rs` |

**예시**:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_serialize_sse_event_as_camel_case() {
        // Arrange
        let event = SseEvent {
            event_type: "retrospect.submitted".to_string(),
            data: serde_json::json!({
                "userId": 1,
                "userName": "김철수"
            }),
            stream_id: Some("1708070400000-0".to_string()),
        };

        // Act
        let json = serde_json::to_string(&event).unwrap();

        // Assert
        assert!(json.contains("\"eventType\""));
        assert!(json.contains("\"streamId\""));
        assert!(!json.contains("\"event_type\""));
    }

    #[test]
    fn should_deserialize_pubsub_message() {
        // Arrange
        let json = r#"{
            "eventType": "comment.created",
            "roomId": "42",
            "data": {"content": "테스트"},
            "timestamp": "2026-02-16T10:30:00Z"
        }"#;

        // Act
        let msg: PubSubMessage = serde_json::from_str(json).unwrap();

        // Assert
        assert_eq!(msg.event_type, "comment.created");
        assert_eq!(msg.room_id, "42");
    }
}
```

### 12.2 통합 테스트 (testcontainers-rs)

Redis 컨테이너를 사용한 실제 Redis 연동 테스트.

```rust
use testcontainers::{runners::AsyncRunner, ContainerAsync};
use testcontainers_modules::redis::Redis;

/// 테스트용 Redis 컨테이너 + 연결 설정.
pub async fn setup_redis() -> (ContainerAsync<Redis>, redis::aio::MultiplexedConnection) {
    let container = Redis::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(6379).await.unwrap();
    let redis_url = format!("redis://127.0.0.1:{}", host_port);
    let client = redis::Client::open(redis_url).unwrap();
    let conn = client.get_multiplexed_tokio_connection().await.unwrap();
    (container, conn)
}
```

| 테스트 시나리오 | 검증 항목 |
|---------------|----------|
| Pub/Sub 팬아웃 | PUBLISH -> 구독자 2개 인스턴스 모두 수신 |
| Streams XADD/XRANGE | 이벤트 저장 후 since 기반 조회 정확성 |
| Presence heartbeat | HSET + EXPIRE 동작, 만료 후 자동 삭제 |
| 분산 락 | 동시 3개 태스크 중 1개만 획득, 소유자 검증 해제 |
| Write-Behind flush | Redis에 쓴 후 flush -> Redis에서 삭제 확인 |
| SSE catch-up | Streams에 이벤트 5개 저장 -> SSE 연결 시 since 이후 이벤트만 수신 |
| 연결 해제 시 정리 | SSE 연결 해제 -> Presence에서 사용자 제거 |

### 12.3 부하 테스트 (k6)

```javascript
// k6-notification-hub.js
import http from 'k6/http';
import { check, sleep } from 'k6';

// 시나리오 1: SSE 연결 유지
export const options = {
  scenarios: {
    sse_connections: {
      executor: 'ramping-vus',
      startVUs: 0,
      stages: [
        { duration: '30s', target: 100 },  // 100 SSE 연결
        { duration: '5m', target: 100 },   // 5분 유지
        { duration: '30s', target: 0 },    // 정리
      ],
    },
    event_publishing: {
      executor: 'constant-rate',
      rate: 50,           // 초당 50 이벤트 발행
      timeUnit: '1s',
      duration: '5m',
      preAllocatedVUs: 10,
    },
  },
};

export function sse_connections() {
  // SSE 연결 (k6는 네이티브 SSE 미지원이므로 HTTP long-polling 시뮬레이션)
  const res = http.get(
    `http://localhost:8083/api/v1/rooms/1/stream`,
    {
      headers: { 'Authorization': `Bearer ${__ENV.JWT_TOKEN}` },
      timeout: '60s',
    }
  );
  check(res, { 'SSE connected': (r) => r.status === 200 });
}

export function event_publishing() {
  const res = http.post(
    'http://localhost:8083/api/v1/notifications/publish',
    JSON.stringify({
      roomId: Math.ceil(Math.random() * 10),
      eventType: 'retrospect.submitted',
      data: { userId: 1, retrospectId: 100 },
      publisherId: 1,
    }),
    {
      headers: {
        'Content-Type': 'application/json',
        'X-Internal-Key': 'local_dev_internal_key',
      },
    }
  );
  check(res, { 'published': (r) => r.status === 200 });
}
```

**성능 목표**:

| 메트릭 | 기준 |
|--------|------|
| SSE 동시 연결 | 1,000+ |
| 이벤트 발행 -> SSE 수신 p95 | < 50ms |
| 이벤트 발행 처리량 | 100+ events/sec |
| 메모리 사용량 (1,000 연결) | < 100MB |
| Pub/Sub 재연결 빈도 | 0 (정상 운영 시) |

---

## 13. 구현 페이즈

### Phase 10-A: 프로젝트 스캐폴딩 (예상: 2시간)

1. Cargo.toml 작성 (위 2.3절 기반)
2. 프로젝트 디렉토리 구조 생성
3. `AppConfig` 환경 변수 로딩
4. `AppError` + `BaseResponse` + `ErrorResponse`
5. `main.rs` 기본 라우터 (health + metrics)
6. tracing-subscriber 로깅 초기화
7. `.env.example` 작성
8. `cargo build` + `cargo clippy` 통과 확인

**완료 기준**: `cargo run` -> `GET /health` 200 OK

### Phase 10-B: JWT 인증 + Redis 연결 (예상: 2시간)

1. `auth/jwt.rs` - Claims, decode_access_token (moalog-server에서 포팅)
2. `auth/extractor.rs` - AuthUser FromRequestParts
3. Redis 연결 풀 초기화 (`redis::Client` -> `MultiplexedConnection`)
4. `state.rs` - AppState (config, redis_pool, connection_manager)
5. JWT 단위 테스트
6. Redis 연결 통합 테스트 (testcontainers)

**완료 기준**: `AuthUser` extractor가 JWT를 검증하고 user_id를 추출

### Phase 10-C: SSE Connection Manager + 핸들러 (예상: 3시간)

1. `sse/connection_manager.rs` - 방별 broadcast 채널 관리
2. `sse/event.rs` - SseEvent 타입 정의
3. `sse/handler.rs` - `GET /rooms/{roomId}/stream` SSE 핸들러
4. 빈 방 정리 백그라운드 태스크
5. SSE keepalive 설정 (15초)
6. 단위 테스트: ConnectionManager subscribe/broadcast
7. 통합 테스트: SSE 연결 수립 + 이벤트 수신

**완료 기준**: SSE 연결을 열고, broadcast로 이벤트를 보내면 수신됨

### Phase 10-D: Redis Pub/Sub Listener (예상: 2시간)

1. `redis_pubsub/listener.rs` - PSUBSCRIBE room:* + 자동 재연결
2. PubSubMessage 역직렬화
3. 수신 메시지 -> ConnectionManager.broadcast() 라우팅
4. Pub/Sub 재연결 지수 백오프
5. 통합 테스트: PUBLISH -> Listener -> SSE 연결 수신

**완료 기준**: Redis CLI에서 `PUBLISH room:42 {...}` -> SSE 클라이언트에서 수신

### Phase 10-E: Redis Streams Logger + Catch-up (예상: 2시간)

1. `redis_streams/logger.rs` - XADD + XRANGE
2. `GET /rooms/{roomId}/events?since={id}` 핸들러
3. SSE 연결 시 `lastEventId` 기반 catch-up 로직
4. MAXLEN 트리밍 설정
5. 통합 테스트: XADD 후 XRANGE 조회

**완료 기준**: SSE 재연결 시 놓친 이벤트 수신 확인

### Phase 10-F: 이벤트 발행 API + 분산 락 (예상: 2시간)

1. `notification/handler.rs` - `POST /notifications/publish`
2. `notification/service.rs` - 발행 로직 (락 -> Streams -> Pub/Sub -> 락 해제)
3. `distributed_lock/lock.rs` - Lua 스크립트 기반 분산 락
4. `lua/lock_acquire.lua`, `lua/lock_release.lua`
5. 내부 API 인증 미들웨어 (`X-Internal-Key`)
6. 멱등성 키 기반 중복 발행 방지
7. 통합 테스트: 동시 발행 시 중복 방지 확인

**완료 기준**: moalog-server에서 POST 호출 -> SSE 수신 e2e 동작

### Phase 10-G: Presence Manager (예상: 2시간)

1. `presence/manager.rs` - heartbeat, get_presence, leave
2. `presence/handler.rs` - GET presence, POST heartbeat
3. SSE 연결 시 자동 presence 등록, 해제 시 leave
4. `presence.update` 이벤트 Pub/Sub 발행
5. 통합 테스트: heartbeat -> get_presence -> leave

**완료 기준**: 방 접속 현황이 실시간으로 업데이트됨

### Phase 10-H: Write-Behind + 읽음 처리 (예상: 2시간)

1. `write_behind/flusher.rs` - 배치 flush 백그라운드 태스크
2. `PATCH /notifications/{id}/read` 핸들러
3. `GET /notifications/unread` 핸들러
4. unread_count 캐시 (INCR/DECR)
5. graceful shutdown 시 마지막 flush
6. 통합 테스트: 읽음 처리 -> flush 확인

**완료 기준**: 읽음 처리 후 미읽은 수 감소 확인

### Phase 10-I: Prometheus 메트릭 + 모니터링 (예상: 1시간)

1. `monitoring/metrics.rs` - 커스텀 메트릭 등록
2. axum-prometheus 레이어 추가
3. 각 모듈에 메트릭 계측 추가
4. Prometheus 스크레이프 설정 추가
5. Grafana 대시보드 JSON 작성

**완료 기준**: `GET /metrics`에서 모든 커스텀 메트릭 노출

### Phase 10-J: Docker + docker-compose 통합 (예상: 2시간)

1. `Dockerfile` 작성 (멀티스테이지)
2. docker-compose.yaml에 notification-hub 서비스 추가
3. Prometheus 스크레이프 타겟 추가
4. 전체 스택 `docker compose up -d` 테스트
5. 헬스체크 확인

**완료 기준**: `docker compose up -d` -> 전체 서비스 healthy

### Phase 10-K: 부하 테스트 + 최적화 (예상: 2시간)

1. k6 부하 테스트 시나리오 작성
2. SSE 동시 연결 1,000개 테스트
3. 이벤트 발행 처리량 측정
4. 병목 지점 프로파일링 + 최적화
5. 결과 리포트 작성

**완료 기준**: 성능 목표 달성 (Section 12.3 기준)

---

## 14. Open Questions / Future Work

### 14.1 Open Questions

| # | 질문 | 의사결정 필요 시점 |
|---|------|------------------|
| Q1 | moalog-server에서 notification-hub 호출을 동기(HTTP) vs 비동기(Redis Pub/Sub 직접 발행)로 할 것인지? | Phase 10-F |
| Q2 | 방 존재 여부 검증을 notification-hub에서 할지, moalog-server에 위임할지? | Phase 10-C |
| Q3 | Write-Behind flush 대상이 moalog-server DB(MySQL)인데, 직접 DB 연결 vs moalog-server API 호출? | Phase 10-H |
| Q4 | SSE 연결 인증에 쿼리 파라미터 `?token=`을 허용할지? (보안 트레이드오프) | Phase 10-C |
| Q5 | Redis Streams의 MAXLEN을 방 활성도에 따라 동적으로 조절할 필요가 있는지? | Phase 10-K |

### 14.2 Future Work

| # | 항목 | 우선순위 | 설명 |
|---|------|---------|------|
| F1 | Consumer Group 기반 워커 | Low | 현재는 단순 XADD/XRANGE. 추후 이벤트 후처리(이메일 알림 등)가 필요하면 Consumer Group 도입 |
| F2 | 사용자별 알림 설정 | Medium | `notif:settings:{userId}` Hash 활용, 이벤트 타입별 on/off |
| F3 | 멀티 방 구독 | Medium | 한 SSE 연결로 여러 방 이벤트 동시 수신 |
| F4 | WebSocket 업그레이드 | Low | 채팅 기능 추가 시 양방향 통신 필요 |
| F5 | Redis Cluster 지원 | Low | 단일 Redis 인스턴스 한계 도달 시 |
| F6 | Rate Limiting | Medium | SSE 연결 수 제한 (사용자당 최대 5개 등) |
| F7 | Kubernetes HPA 연동 | Low | SSE 연결 수 기반 자동 스케일링 |
| F8 | OpenTelemetry 트레이싱 | Medium | moalog-server -> notification-hub 이벤트 흐름 추적 |
| F9 | 프론트엔드 SDK | Medium | EventSource 래퍼 + 자동 재연결 + catch-up 로직 캡슐화 |

---

## 부록 A: AppState 전체 정의

```rust
use std::sync::Arc;
use std::time::Instant;

use crate::config::AppConfig;
use crate::distributed_lock::DistributedLock;
use crate::presence::PresenceManager;
use crate::redis_streams::StreamLogger;
use crate::sse::ConnectionManager;
use crate::write_behind::WriteBehindFlusher;

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub redis_pool: redis::aio::MultiplexedConnection,
    pub connection_manager: ConnectionManager,
    pub stream_logger: Arc<StreamLogger>,
    pub presence_manager: Arc<PresenceManager>,
    pub distributed_lock: Arc<DistributedLock>,
    pub start_time: Instant,
}
```

## 부록 B: main.rs 라우터 구성 스케치

```rust
use axum::{routing::{get, post, patch}, Router, middleware};
use axum_prometheus::PrometheusMetricLayer;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    // 로깅 초기화
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
        )
        .json()
        .init();

    // 설정 로드
    let config = AppConfig::from_env()?;
    let port = config.server_port;

    // Redis 연결
    let redis_client = redis::Client::open(config.redis_url.as_str())?;
    let redis_pool = redis_client.get_multiplexed_tokio_connection().await?;
    tracing::info!("Redis 연결 성공");

    // 모듈 초기화
    let connection_manager = ConnectionManager::new(config.sse_channel_capacity);
    let stream_logger = Arc::new(StreamLogger::new(redis_pool.clone()));
    let presence_manager = Arc::new(PresenceManager::new(
        redis_pool.clone(),
        config.presence_user_ttl_secs,
        config.presence_hash_ttl_secs,
    ));
    let distributed_lock = Arc::new(DistributedLock::new(redis_pool.clone()));

    let app_state = AppState {
        config: config.clone(),
        redis_pool: redis_pool.clone(),
        connection_manager: connection_manager.clone(),
        stream_logger,
        presence_manager,
        distributed_lock,
        start_time: std::time::Instant::now(),
    };

    // Pub/Sub Listener 시작 (백그라운드)
    let pubsub_listener = PubSubListener::new(
        config.redis_url.clone(),
        connection_manager.clone(),
    );
    tokio::spawn(async move { pubsub_listener.start().await });

    // Write-Behind Flusher 시작 (백그라운드)
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let flusher = WriteBehindFlusher::new(
        redis_pool.clone(),
        config.write_behind_interval_secs,
        config.write_behind_batch_size,
    );
    tokio::spawn(async move { flusher.start(shutdown_rx).await });

    // 빈 방 정리 태스크 (60초마다)
    let cm_cleanup = connection_manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            cm_cleanup.cleanup_empty_rooms().await;
        }
    });

    // Prometheus 메트릭
    let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();

    // CORS 설정
    let cors = build_cors_layer(&config);

    // 내부 API 라우터 (X-Internal-Key 인증)
    let internal_routes = Router::new()
        .route(
            "/api/v1/notifications/publish",
            post(notification::handler::publish),
        )
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            internal_auth_middleware,
        ));

    // 인증 필요 라우터
    let authenticated_routes = Router::new()
        .route(
            "/api/v1/rooms/:room_id/stream",
            get(sse::handler::stream_handler),
        )
        .route(
            "/api/v1/rooms/:room_id/events",
            get(notification::handler::get_events),
        )
        .route(
            "/api/v1/rooms/:room_id/presence",
            get(presence::handler::get_presence),
        )
        .route(
            "/api/v1/rooms/:room_id/presence/heartbeat",
            post(presence::handler::heartbeat),
        )
        .route(
            "/api/v1/notifications/unread",
            get(notification::handler::get_unread_count),
        )
        .route(
            "/api/v1/notifications/:notification_id/read",
            patch(notification::handler::mark_read),
        );

    // 공개 라우터
    let public_routes = Router::new()
        .route("/health", get(health_handler))
        .route("/metrics", get(|| async move { metric_handle.render() }));

    let app = Router::new()
        .merge(public_routes)
        .merge(internal_routes)
        .merge(authenticated_routes)
        .layer(TraceLayer::new_for_http())
        .layer(prometheus_layer)
        .layer(cors)
        .with_state(app_state);

    // 서버 시작
    let listener = tokio::net::TcpListener::bind(
        format!("0.0.0.0:{}", port)
    ).await?;
    tracing::info!("Notification Hub 시작: http://0.0.0.0:{}", port);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_tx))
        .await?;

    Ok(())
}

async fn shutdown_signal(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    tokio::signal::ctrl_c().await.ok();
    tracing::info!("Shutdown 신호 수신");
    let _ = shutdown_tx.send(true);
}
```
