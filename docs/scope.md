상태: v0 정렬 스냅샷 | 2026-08-11 | 이 문서는 구현 중 갱신된다

# Scope

## In

- 4계층 전체: `docket-core`(작업큐 엔진) · `docket-mcp`(MCP 표면) · `docket-cc`(Claude Code 어댑터) · `docket-console`(관리 UI)
- 워커·토픽·아이템·클레임 도메인 모델 (코어)
- 아이템 생명주기: `open → claimed → resolved → closed` + `resolution`([architecture.md](architecture.md))
- 정체 감지(담당 없음 / 갱신 없음의 구분)
- 관리 콘솔의 전 조작 — 특히 "보완"(모호한 요구를 다듬어 재투입)
- 질의(`question`) — 상태 기계 없는 즉시 실패형 요청
- 단일 리포, 전체 공개([ADR-0005](decisions/ADR-0005-public-scope.md))

## Out

| 항목 | 근거 |
|---|---|
| 파일 동기화 | git이 해결. 코어는 참조(`refs`)만 다룬다 |
| 자동 작업 배분(오케스트레이션) | P-3, 사람의 수동 배정은 예외 |
| 실시간 채팅 | 차별화 가설에서 명시적으로 포기(지고도 괜찮은 축) |
| 복수 워커 협업 클레임 | 단일 클레임만 허용으로 확정 — 배타적 클레임이 도메인 모델의 정의 |
| 프롬프트 인젝션 방어(완전한 형태) | v1 커버리지에서 제외 — L2~L3로 유예([coverage.md](coverage.md)) |
| 규제 대응, 성능 튜닝 | 하드 제약 없음으로 확정([principles.md](principles.md)) |

## Later

| 항목 | 조건 | 관련 문서 |
|---|---|---|
| 다중 사용자(팀 단위) | §12.1 인증 방식을 확정하는 시점에 재검토 | [ADR-0006](decisions/ADR-0006-single-owner-later.md) |
| 다른 에이전트 런타임(예: `aims`) 확장 | 3번 층만 추가하면 되는 구조는 이미 확보. 실제 수요가 생기면 | [architecture.md](architecture.md) 확장 지점 |
| 사람 워커(모바일에서 사람이 아이템을 집음) | 4번 층 확장만으로 가능, 코어 무변경. 수요 발생 시 | [architecture.md](architecture.md) |
| 리포 외 토픽 네임스페이스 표준화 | 실제로 리포가 아닌 토픽(조직 지식, 머신, 환경)을 쓰기 시작할 때 | [open-questions.md](open-questions.md) #8 |
