상태: v0 정렬 스냅샷 | 2026-08-11 | 이 문서는 구현 중 갱신된다

# Roadmap

먼 마일스톤일수록 추상적이고, 가까울수록 구체적이다.

## M1 — 코어 (첫 슬라이스)

**범위**
- 언어: Rust
- 코어 API 최소셋: 워커 등록, 아이템 생성(file), 클레임, submit(→resolved), 요청자 승인(→closed, resolution=done)
- 저장소: SQLite
- 인터페이스: HTTP API만. mcp/cc/console 없음. 사람이 curl로 워커를 흉내낸다.

**완료 판정**: 터미널 두 개에서 서로 다른 "워커"인 척 curl로 조작한다. 워커A가 토픽 X 앞으로 아이템 생성 → 워커B가 X를 담당 등록 후 `list`로 발견 → claim → submit → 워커A가 승인(close). SQLite에 상태 전이가 정확히 기록된다. 동시에 두 워커가 같은 아이템을 claim 시도하면 하나만 성공한다(클레임 배타성 검증).

근거: [ADR-0001](decisions/ADR-0001-work-queue-model.md), [ADR-0007](decisions/ADR-0007-language-runtime.md), [quality-ramp.md](quality-ramp.md) L0.

## M2 — 존재 증명

`docket-mcp` + `docket-cc`. 두 세션이 실제로 아이템을 주고받아 [vision.md](vision.md) S1~S6을 수동으로 완주한다. 이것이 되면 제품이 성립한다.

훅 기반 능동 알림(§10)은 이 단계에서 필수가 아니다 — MCP 수동 호출만으로 완주 가능해야 한다([backlog.md](backlog.md) A-1 검증과 겹친다).

## M3 — 콘솔

보드, 정체 감지, 관리자 조작 전체. 특히 "보완"(§11.4) — 이것이 콘솔의 존재 이유다. 사용률/번다운 계측이 실제로 시작되는 지점([goals.md](goals.md)).

## M4 — 안전장치와 다중 머신

예산 전체([open-questions.md](open-questions.md) #33~#39), 인증(#41~#43), 머신 간 라우팅, 배포 채널(#45~#48).

## 로드맵 밖

다중 사용자 협업은 이 로드맵의 어느 마일스톤에도 없다 — Later([ADR-0006](decisions/ADR-0006-single-owner-later.md)).
