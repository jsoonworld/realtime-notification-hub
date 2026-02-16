# Phase 10: Redis 심화 — 분산 락, 캐시 전략, 실시간 알림

## 한 줄 요약

기존 Redis(Rate Limit + 멱등성)에 **분산 락 · Cache-Aside · Write-Behind · Pub/Sub**를 추가하여, 면접 단골 주제 4가지를 하나의 인프라에서 시연 가능하게 만든다.

---

## 현재 상태 (AS-IS)

| 서비스 | Redis 용도 | 데이터 구조 |
|--------|-----------|------------|
| distributed-rate-limiter | Token Bucket / Sliding Window | Hash / Sorted Set (Lua) |
| fluxpay-engine | 멱등성 키 잠금 | Hash (Lua: ACQUIRE_LOCK) |
| moalog-server | 직접 사용 안 함 (HTTP로 rate-limiter 호출) | — |

**빠진 것**: 분산 락, 캐싱, Write-Behind, Pub/Sub — 전부 실무에서 필수이지만 현재 없음.

---

## 목표 상태 (TO-BE)

```
                    ┌─────────────────────────────────────────────┐
                    │                  Redis 7                    │
                    │                                             │
                    │  ┌──────────┐  ┌──────────┐  ┌───────────┐ │
                    │  │Rate Limit│  │Idempotency│  │  (기존)   │ │
                    │  │ Hash/ZSet│  │   Hash    │  │          │ │
                    │  └──────────┘  └──────────┘  └───────────┘ │
                    │                                             │
                    │  ┌──────────┐  ┌──────────┐  ┌───────────┐ │
                    │  │분산 락    │  │Cache-Aside│  │Write-     │ │
                    │  │ String   │  │ String/  │  │Behind     │ │
                    │  │ (Lua)    │  │ Hash     │  │ List/     │ │
                    │  │          │  │          │  │ Stream    │ │
                    │  └──────────┘  └──────────┘  └───────────┘ │
                    │                                             │
                    │  ┌──────────────────────────────────────┐   │
                    │  │  Pub/Sub (실시간 알림)                  │   │
                    │  │  Channel: retro.{roomId}.events       │   │
                    │  └──────────────────────────────────────┘   │
                    └─────────────────────────────────────────────┘
```

---

## 구현 범위 (4개 기능)

### 기능 1: 분산 락 (Distributed Lock)

**왜 필요한가**: 현재 FluxPay 멱등성 키는 "같은 요청의 중복 실행"은 막지만, "다른 요청 간의 동시성 경쟁"은 막지 못함. 예: 동일 유저가 구독 업그레이드를 동시에 2번 클릭하면 둘 다 진행될 수 있음.

**적용 대상**:
- `fluxpay-engine`: 구독 생성/변경 시 유저 단위 락
- `moalog-server`: 회고 제출(submit) 시 중복 제출 방지

**구현 방식**: Redis Lua 기반 락 (Redisson 스타일, 직접 구현)

```
-- 락 획득 (SET NX EX + owner 식별)
ACQUIRE: SET lock:{resource} {owner} NX EX {ttl}
-- 락 해제 (owner 검증 후 DEL)
RELEASE: if GET == owner then DEL end
-- 락 갱신 (워치독 패턴)
RENEW:   if GET == owner then EXPIRE {ttl} end
```

**Lua 스크립트 (acquire)**:
```lua
local key = KEYS[1]
local owner = ARGV[1]
local ttl = tonumber(ARGV[2])

if redis.call('SET', key, owner, 'NX', 'EX', ttl) then
    return 1
end
return 0
```

**Lua 스크립트 (release)**:
```lua
local key = KEYS[1]
local owner = ARGV[1]

if redis.call('GET', key) == owner then
    return redis.call('DEL', key)
end
return 0
```

**변경 파일**:
- `fluxpay-engine/src/.../infrastructure/lock/RedisDistributedLock.java` (신규)
- `fluxpay-engine/src/.../infrastructure/lock/DistributedLockAspect.java` (신규 — AOP)
- `fluxpay-engine/src/.../domain/subscription/service/SubscriptionCommandService.java` (락 적용)

**트레이드오프: Redlock vs 단일 Redis 락**

| | 단일 Redis 락 (선택) | Redlock (N개 Redis) |
|---|---|---|
| 복잡도 | 낮음 | 높음 (과반수 합의) |
| 안전성 | Redis 장애 시 락 유실 가능 | N/2+1 합의로 안전 |
| 성능 | 1 RTT | N RTT |
| Martin Kleppmann 의견 | "충분" (fencing token과 조합) | "불필요하게 복잡" |

> **판단**: 단일 Redis 인스턴스 환경이고, 결제 정합성은 이미 멱등성 키 + DB 유니크 제약으로 보장됨. 분산 락은 UX 레벨의 중복 방지이므로 단일 락으로 충분. 프로덕션에서 Redis Sentinel/Cluster 전환 시 Redlock 고려.

---

### 기능 2: Cache-Aside (Look-Aside + Cache Stampede 방지)

**왜 필요한가**: 회고 목록/상세는 읽기가 쓰기보다 압도적으로 많은 패턴. 현재는 매 요청마다 MySQL 쿼리.

**적용 대상**:
- `moalog-server`: 회고 상세 조회, 회고 목록, 구독 플랜 조회

**캐시 전략**:

```
Client → moalog-server
               │
               ├─ 1. Redis GET cache:{key}
               │      ├─ HIT  → 반환
               │      └─ MISS ↓
               ├─ 2. MySQL 쿼리
               ├─ 3. Redis SET cache:{key} EX {ttl}
               └─ 4. 반환
```

**Cache Stampede 방지 (Lease 패턴)**:

```
Thread A: MISS → SET cache:{key}:lease {owner} NX EX 5 → 성공 → DB 조회 → SET cache
Thread B: MISS → SET cache:{key}:lease {owner} NX EX 5 → 실패 → 짧은 대기 → 재시도(HIT)
```

**Lua 스크립트 (get-or-lease)**:
```lua
local key = KEYS[1]
local lease_key = key .. ':lease'
local owner = ARGV[1]
local lease_ttl = tonumber(ARGV[2])

-- 캐시 히트
local cached = redis.call('GET', key)
if cached then
    return {'HIT', cached}
end

-- Lease 획득 시도
if redis.call('SET', lease_key, owner, 'NX', 'EX', lease_ttl) then
    return {'LEASE_ACQUIRED', ''}
end

return {'LEASE_WAIT', ''}
```

**변경 파일**:
- `moalog-server/codes/server/Cargo.toml` — `redis` 크레이트 추가
- `moalog-server/codes/server/src/config/redis.rs` (신규 — Redis 커넥션 풀)
- `moalog-server/codes/server/src/utils/cache.rs` (신규 — Cache-Aside + Lease 로직)
- `moalog-server/codes/server/src/domain/retrospect/service.rs` — 조회 함수에 캐시 적용
- `moalog-server/codes/server/src/domain/payment/service.rs` — 플랜 조회 캐시

**캐시 무효화 전략**:

| 이벤트 | 무효화 대상 | 방법 |
|--------|-----------|------|
| 회고 생성/수정/삭제 | `cache:retro:{id}`, `cache:retro_list:{room_id}` | 직접 DEL |
| 구독 변경 | `cache:subscription:{user_id}` | 직접 DEL |
| 방 이름 변경 | `cache:retro_list:{room_id}` | 직접 DEL |

**TTL 정책**:
- 회고 상세: 5분 (자주 변경 가능)
- 회고 목록: 2분 (목록은 더 빈번한 변경)
- 구독 플랜: 1시간 (거의 변경 없음)

**트레이드오프: Cache-Aside vs Read-Through vs Write-Through**

| | Cache-Aside (선택) | Read-Through | Write-Through |
|---|---|---|---|
| 제어 | 애플리케이션이 직접 제어 | 캐시 라이브러리가 제어 | 캐시가 DB에 동기 쓰기 |
| 구현 복잡도 | 중간 | 낮음 (라이브러리) | 낮음 |
| 일관성 | TTL + 수동 무효화 | TTL | 강한 일관성 |
| 유연성 | 높음 | 낮음 | 낮음 |

> **판단**: Rust(moalog-server)에서는 Spring Cache 같은 라이브러리가 없으므로 직접 제어하는 Cache-Aside가 자연스러움. Stampede 방지를 Lease 패턴으로 구현하면 면접에서 차별점.

---

### 기능 3: Write-Behind (비동기 쓰기 버퍼)

**왜 필요한가**: 회고 응답(response)의 좋아요(like) 카운트, 조회수 등은 빈번하게 발생하지만 매번 DB에 쓰면 부하가 큼.

**적용 대상**:
- `moalog-server`: 좋아요 토글, 조회수 증가

**아키텍처**:

```
Client → like 요청
            │
            ├─ 1. Redis HINCRBY like_buffer:{response_id} count 1
            ├─ 2. 즉시 응답 (Redis 카운트 기준)
            │
            └─ (비동기) 주기적 flush
                  ├─ HGETALL like_buffer:*
                  ├─ MySQL UPDATE responses SET like_count = like_count + N
                  └─ DEL like_buffer:{response_id}
```

**변경 파일**:
- `moalog-server/codes/server/src/utils/write_behind.rs` (신규 — 버퍼 + flush 로직)
- `moalog-server/codes/server/src/domain/retrospect/handler.rs` — toggle_like 수정

**Flush 주기**: 10초 (tokio::interval)

**트레이드오프: Write-Behind vs 즉시 쓰기**

| | Write-Behind (선택) | 즉시 DB 쓰기 (현재) |
|---|---|---|
| DB 부하 | 배치로 줄어듬 | 매 요청마다 쿼리 |
| 데이터 유실 | Redis 장애 시 flush 안 된 데이터 유실 | 없음 |
| 일관성 | 최대 10초 지연 | 즉시 |
| 적합한 데이터 | 좋아요, 조회수 (유실 허용) | 결제, 인증 (유실 불가) |

> **판단**: 좋아요 카운트는 ±1 정도의 오차를 허용할 수 있는 데이터. Redis 장애 시에도 원본은 DB에 있으므로 근사치 제공 가능. 결제/인증 데이터에는 절대 적용하지 않음.

---

### 기능 4: Pub/Sub (실시간 이벤트 알림)

**왜 필요한가**: 회고 방(RetroRoom)에서 다른 참가자가 제출하면 실시간으로 알 수 없음. 현재는 폴링만 가능.

**적용 대상**:
- `moalog-server`: 회고 방 내 실시간 이벤트 (제출, 댓글, 좋아요)

**아키텍처**:

```
Member A: 회고 제출
    │
    ├─ DB 저장
    └─ Redis PUBLISH retro.room.{roomId} '{"type":"submitted","userId":1}'
                     │
         ┌───────────┼───────────┐
         ▼           ▼           ▼
    Member B      Member C    Member D
    (SSE 연결)    (SSE 연결)   (SSE 연결)
```

**SSE (Server-Sent Events) 선택 이유**:

| | SSE (선택) | WebSocket |
|---|---|---|
| 프로토콜 | HTTP/1.1, 단방향 | 별도 프로토콜, 양방향 |
| 구현 복잡도 | 낮음 (Axum 기본 지원) | 높음 (연결 관리) |
| 호환성 | 브라우저 네이티브 | 라이브러리 필요 |
| 적합한 케이스 | 서버 → 클라이언트 알림 | 채팅, 실시간 편집 |

> **판단**: 회고 서비스의 실시간 요구사항은 "알림"(단방향)이므로 SSE가 적합. WebSocket은 양방향이 필요한 채팅/편집에 사용.

**변경 파일**:
- `moalog-server/codes/server/src/domain/retrospect/sse.rs` (신규 — SSE 엔드포인트)
- `moalog-server/codes/server/src/domain/retrospect/handler.rs` — submit 후 PUBLISH 추가
- `moalog-server/codes/server/src/main.rs` — SSE 라우트 등록

**새 엔드포인트**:
```
GET /api/v1/retro-rooms/{roomId}/events  → SSE 스트림
```

---

## Docker Compose 변경

```yaml
# 변경 없음 — 기존 Redis 7 인스턴스를 그대로 사용
# 새로운 기능은 모두 기존 Redis에 데이터 구조만 추가
```

> **핵심**: 인프라 추가 비용 0. 기존 Redis 하나로 6가지 역할 (Rate Limit + 멱등성 + 분산 락 + 캐시 + Write-Behind + Pub/Sub).

---

## Prometheus 메트릭 추가

| 메트릭 | 타입 | 설명 |
|--------|------|------|
| `redis_cache_hit_total` | Counter | 캐시 히트 수 |
| `redis_cache_miss_total` | Counter | 캐시 미스 수 |
| `redis_lock_acquired_total` | Counter | 락 획득 성공 수 |
| `redis_lock_contention_total` | Counter | 락 경쟁 (획득 실패) 수 |
| `redis_write_behind_flush_total` | Counter | Write-Behind flush 수 |
| `redis_write_behind_buffer_size` | Gauge | 현재 버퍼 크기 |

---

## 검증 방법

### 부하 테스트 (k6)
```bash
# 분산 락: 동일 유저 동시 구독 변경 → 1건만 성공
# Cache-Aside: 캐시 히트율 측정 (목표: >80%)
# Write-Behind: 좋아요 폭주 시 DB 쿼리 수 비교 (Before/After)
# Pub/Sub: SSE 연결 100개 + 이벤트 발행 → 전달 지연 측정
```

### Grafana 대시보드
- 캐시 히트율 패널 추가
- 락 경쟁률 패널 추가

---

## 예상 작업량

| 기능 | 변경 서비스 | 신규 파일 | 수정 파일 | 난이도 |
|------|-----------|----------|----------|--------|
| 분산 락 | fluxpay-engine | 2 | 1 | ★★☆ |
| Cache-Aside | moalog-server | 2 | 3 | ★★★ |
| Write-Behind | moalog-server | 1 | 1 | ★★☆ |
| Pub/Sub + SSE | moalog-server | 1 | 2 | ★★☆ |

**의존성**: 없음 (4개 기능 독립 구현 가능, 병렬 작업 OK)

---

## 면접 키워드

- 분산 락: Redlock vs 단일 락, TTL, 워치독, fencing token
- Cache-Aside: Stampede, Lease, TTL 정책, 무효화 전략
- Write-Behind: eventual consistency, flush 주기, 유실 허용 기준
- Pub/Sub: fan-out, SSE vs WebSocket, 연결 관리
