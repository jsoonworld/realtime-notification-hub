# Real-time Notification Hub

## 한 줄 요약

Redis Pub/Sub · Streams · 분산 락을 활용한 **실시간 알림 허브 서비스**. 회고 방에서 발생하는 이벤트(제출, 댓글, 좋아요)를 연결된 클라이언트에게 즉시 전달한다.

---

## 풀고자 하는 문제

현재 moalog-server의 회고 방(RetroRoom)에서는:

1. 다른 참가자가 회고를 **제출해도 알 수 없음** → 새로고침해야 확인
2. 댓글/좋아요가 달려도 **실시간 반영 안 됨**
3. 현재 방에 **누가 접속 중인지** 알 수 없음 (Presence)

```
현재: Client → 폴링(5초마다) → 서버 → DB 조회 → 변경 있나?
목표: Client ←── SSE ←── 알림 허브 ←── Redis Pub/Sub ←── 이벤트 발생
```

---

## 아키텍처

```
┌──────────────────────────────────────────────────────────────┐
│                  Real-time Notification Hub                   │
│                  Kotlin · Spring WebFlux                      │
│                                                              │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────────────┐ │
│  │ SSE Endpoint│  │ Notification │  │ Presence Manager    │ │
│  │ /stream     │  │ Router       │  │ (누가 접속 중인지)   │ │
│  │             │◀─│              │  │                     │ │
│  │ Client별    │  │ 이벤트 수신  │  │ Redis Hash +        │ │
│  │ 연결 관리   │  │ → 대상 필터  │  │ Expire (heartbeat)  │ │
│  │             │  │ → SSE 전송   │  │                     │ │
│  └─────────────┘  └──────┬───────┘  └─────────────────────┘ │
│                          │                                    │
│  ┌───────────────────────▼────────────────────────────────┐  │
│  │                    Redis 7                              │  │
│  │                                                        │  │
│  │  ┌──────────┐  ┌──────────────┐  ┌──────────────────┐ │  │
│  │  │ Pub/Sub  │  │ Streams      │  │ Hash + Sorted Set│ │  │
│  │  │          │  │              │  │                  │ │  │
│  │  │ 실시간   │  │ 이벤트 로그  │  │ 사용자 설정 캐시 │ │  │
│  │  │ 팬아웃   │  │ (영속)       │  │ + 알림 읽음 상태 │ │  │
│  │  └──────────┘  └──────────────┘  └──────────────────┘ │  │
│  │                                                        │  │
│  │  ┌──────────┐  ┌──────────────┐                       │  │
│  │  │분산 락    │  │Write-Behind │                       │  │
│  │  │(Lua SET  │  │읽음 상태    │                       │  │
│  │  │ NX EX)   │  │배치 flush   │                       │  │
│  │  └──────────┘  └──────────────┘                       │  │
│  └────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────┘
        ▲                                          ▲
        │ REST API (이벤트 발행)                     │ SSE (실시간 수신)
        │                                          │
┌───────┴──────┐                          ┌────────┴───────┐
│ moalog-server│                          │   Frontend     │
│ (이벤트 발행) │                          │  (EventSource) │
└──────────────┘                          └────────────────┘
```

---

## 기술 스택

| 카테고리 | 기술 | 선택 이유 |
|---------|------|----------|
| 언어 | Kotlin 1.9 | 기존 rate-limiter와 일관성, 코루틴 지원 |
| 프레임워크 | Spring Boot 3.2 + WebFlux | Reactive SSE 네이티브 지원 |
| 실시간 전송 | SSE (Server-Sent Events) | 단방향 알림에 적합, HTTP 표준 |
| 메시지 브로커 | Redis Pub/Sub | 초저지연 팬아웃 (<1ms) |
| 이벤트 로그 | Redis Streams | 영속성 + Consumer Group 지원 |
| 캐싱 | Redis Hash | 사용자 알림 설정, 읽음 상태 |
| 동시성 제어 | Redis 분산 락 (Lua) | 중복 알림 방지 |
| 모니터링 | Micrometer + Prometheus | 연결 수, 이벤트 처리량 메트릭 |

---

## 핵심 기능 (5개)

### 1. SSE 실시간 스트림

클라이언트가 SSE 연결을 맺으면, 해당 방의 이벤트를 실시간으로 수신.

**엔드포인트**:
```
GET /api/v1/rooms/{roomId}/stream
Accept: text/event-stream
Authorization: Bearer {token}

→ Response (SSE):
event: retrospect.submitted
data: {"userId":1,"userName":"김철수","retrospectId":42,"timestamp":"2026-02-16T10:30:00Z"}

event: comment.created
data: {"userId":2,"responseId":15,"content":"좋은 포인트!","timestamp":"2026-02-16T10:31:00Z"}

event: presence.update
data: {"online":[1,2,3],"joined":3,"left":null}
```

**트레이드오프: SSE vs WebSocket**

| | SSE (선택) | WebSocket |
|---|---|---|
| 방향 | 서버 → 클라이언트 (단방향) | 양방향 |
| 프로토콜 | HTTP/1.1 표준 | 별도 프로토콜 (ws://) |
| 자동 재연결 | 브라우저 네이티브 지원 | 직접 구현 필요 |
| 로드밸런서 | HTTP 그대로 | Upgrade 핸들링 필요 |
| 적합한 케이스 | **알림** (읽기 전용) | 채팅, 실시간 편집 |

> **판단**: 회고 방 알림은 서버→클라이언트 단방향. SSE로 충분하며 인프라 복잡도가 낮음.

---

### 2. Redis Pub/Sub 기반 이벤트 팬아웃

서비스 인스턴스가 여러 개일 때, 모든 인스턴스의 SSE 연결에 이벤트를 전달.

```
moalog-server: POST /api/v1/notifications/publish
    │
    └─ Redis PUBLISH room:{roomId} {event_json}
                │
    ┌───────────┼───────────┐
    ▼           ▼           ▼
 Instance A  Instance B  Instance C
 (SSE 연결)  (SSE 연결)  (SSE 연결)
  User 1,2    User 3      User 4,5
```

**트레이드오프: Pub/Sub vs Streams (팬아웃용)**

| | Pub/Sub (선택) | Streams |
|---|---|---|
| 지연 | < 1ms | ~ 수 ms (폴링) |
| 영속성 | 없음 (fire-and-forget) | 있음 |
| 수신자 부재 시 | 메시지 유실 | 보관됨 |

> **판단**: 실시간 알림은 유실 허용 가능. 초저지연이 중요하므로 Pub/Sub. 이벤트 로그는 Streams에 별도 저장.

---

### 3. Redis Streams 이벤트 로그

모든 이벤트를 Redis Streams에 영속 저장. 클라이언트가 재연결 시 놓친 이벤트 조회.

```
XADD room:events:{roomId} * type "retrospect.submitted" payload "{...}"
```

**미읽은 이벤트 조회**: `GET /api/v1/rooms/{roomId}/events?since={id}`

---

### 4. Presence (접속 현황)

Redis Hash + TTL로 실시간 접속자 추적.

```
HSET presence:{roomId} {userId} {timestamp}
EXPIRE presence:{roomId} 60
# 클라이언트 30초마다 heartbeat → HSET + EXPIRE 갱신
```

---

### 5. 분산 락 + 중복 알림 방지

Lua 스크립트 기반 분산 락으로 동일 이벤트의 중복 처리 방지.

```lua
-- acquire
local key = KEYS[1]
local owner = ARGV[1]
local ttl = tonumber(ARGV[2])
if redis.call('SET', key, owner, 'NX', 'EX', ttl) then return 1 end
return 0

-- release
if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) end
return 0
```

**트레이드오프: 단일 Redis 락 vs Redlock** — 알림 중복은 최악의 경우 "2번 알림"이지 데이터 유실이 아님. 단일 락으로 충분.

---

## API 설계

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/v1/rooms/{roomId}/stream` | SSE 실시간 스트림 |
| POST | `/api/v1/notifications/publish` | 이벤트 발행 |
| GET | `/api/v1/rooms/{roomId}/events?since={id}` | 미읽은 이벤트 조회 |
| GET | `/api/v1/rooms/{roomId}/presence` | 접속자 현황 |
| POST | `/api/v1/rooms/{roomId}/presence/heartbeat` | Heartbeat |
| GET | `/api/v1/notifications/unread?userId={id}` | 미읽은 알림 수 |
| PATCH | `/api/v1/notifications/{id}/read` | 읽음 처리 |

---

## 면접 키워드

- SSE vs WebSocket: 단방향 vs 양방향, 자동 재연결
- Redis Pub/Sub: 팬아웃, 인스턴스 간 전파, fire-and-forget
- Redis Streams: 영속성, Consumer Group, XRANGE
- Presence: heartbeat, TTL 기반 세션 만료
- 분산 락: SET NX EX, Lua 원자성, Redlock 트레이드오프
