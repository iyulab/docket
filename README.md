# docket

헤드리스 워커를 위한 작업큐 서비스. Claude Code 세션은 그 워커의 한 종류일 뿐이다.

## 누구를 위한 것인가

여러 대의 머신에서 다수의 Claude Code(혹은 유사 헤드리스) 세션을 상시 운영하며, 세션들이 서로 다른 저장소나 서로 의존하는 컴포넌트를 다루는 1인 개발자. 코어 엔진은 Claude Code에 종속되지 않으므로, 타 프로젝트가 헤드리스 워커 조정에 재사용할 수 있다.

## 무엇이 아닌가

- 파일 동기화 서비스가 아니다 — 산출물 공유는 git이 한다.
- 오케스트레이터가 아니다 — 중앙이 워커에게 작업을 자동 배분하지 않는다. 워커가 스스로 집는 pull 모델이다(사람의 수동 개입은 예외).
- 실시간 채팅이 아니다.
- 다중 사용자 협업 도구가 아니다 — 1차 목표는 한 사람이 소유한 여러 머신이다(다중 사용자는 Later, [ADR-0006](docs/decisions/ADR-0006-single-owner-later.md) 참조).

## 현재 상태

**v0 정렬 완료, 구현 전.** 도메인 모델·계층 경계·성공 지표·손절선까지 정렬됐고, 코드는 아직 없다. 첫 슬라이스는 [roadmap.md](docs/roadmap.md)의 M1이다.

## 다음 백로그 (지금 구간)

- `[B-04]` ASSUMPTION 세션 기동 빈도 자기관찰 (지금 바로 시작 가능)
- `[B-01]` SPIKE 유사 제품/프레임워크 조사
- `[B-06]` ENABLER M1 코어 구현 (첫 슬라이스)

전체 백로그: [backlog.md](docs/backlog.md)

## 문서

| 문서 | 내용 |
|---|---|
| [vision.md](docs/vision.md) | 문제 · 사용자 · 시나리오 |
| [principles.md](docs/principles.md) | 철학 · 원칙 · 비목표 |
| [scope.md](docs/scope.md) | In / Out / Later |
| [goals.md](docs/goals.md) | 북극성 · 목표 트리 · 지표 · 손절선 |
| [landscape.md](docs/landscape.md) | 대안 지형 (대부분 미조사) |
| [architecture.md](docs/architecture.md) | 시스템 경계 · 도메인 모델 · Type-1 결정 |
| [coverage.md](docs/coverage.md) | 역량 × 사례 커버리지 행렬 |
| [quality-ramp.md](docs/quality-ramp.md) | L0~L3 품질 레벨과 통과 기준 |
| [backlog.md](docs/backlog.md) | 지금 / 다음 / 나중 |
| [roadmap.md](docs/roadmap.md) | 마일스톤과 첫 슬라이스 |
| [glossary.md](docs/glossary.md) | 코어 어휘 대응표 |
| [open-questions.md](docs/open-questions.md) | 구현 중 결정할 것들 |
| [decisions/](docs/decisions/) | ADR — Type-1 결정 1건당 1파일 |

데이터 전략(`data-strategy.md`)은 이 프로젝트의 핵심이 아니므로 생략한다 — docket은 학습 데이터가 아니라 운영 상태(워커·아이템·클레임)를 다룬다.

이후 세션이 지킬 규칙은 [AGENTS.md](AGENTS.md)에 있다.
