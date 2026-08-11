상태: v0 정렬 스냅샷 | 2026-08-11 | 이 문서는 구현 중 갱신된다

# Backlog

연결되지 않은 항목(결정·가정·지표 중 어디에도 안 붙는 것)은 여기 없다.

## 지금

### [B-04] ASSUMPTION 세션 기동 빈도 자기관찰
- 질문: 실제로 각 머신에서 세션을 얼마나 자주 여는가? 토픽별 담당 세션이 "없는 기간"이 얼마나 긴가?
- 타임박스: 1주 (현재 습관 그대로 기록만)
- 산출: 머신별 세션 기동 빈도 로그
- 영향: [open-questions.md](open-questions.md) #19(정체 임계시간), [goals.md](goals.md) A-2
- 선행: 없음, 지금 바로 시작 가능

### [B-01] SPIKE 유사 제품/프레임워크 조사
- 질문: 멀티에이전트/헤드리스 워커 조정을 다루는 기존 도구가 있는가? 상태추적·클레임 방식이 docket과 어떻게 다른가?
- 타임박스: 1시간
- 산출: [landscape.md](landscape.md) 표 갱신
- 영향: [landscape.md](landscape.md) 차별화 가설 재검증
- 선행: 없음

### [B-02] EVAL 사용률 분모 계측 방법 설계
- 질문: docket을 거치지 않은 수동 조정 사례를 어떻게 기록할 것인가?
- 타임박스: 30분
- 산출: 측정 절차 1개 확정
- 영향: [goals.md](goals.md) 사용률 지표의 실제 계측 가능 여부
- 선행: 없음

### [B-06] ENABLER M1 코어 구현
- 산출: [roadmap.md](roadmap.md) 첫 슬라이스 완료
- 영향: L0 게이트([quality-ramp.md](quality-ramp.md))
- 선행: 없음

### ~~[B-10] SPIKE Rust MCP SDK(`rmcp` 등) 성숙도 확인~~ — 완료
공식 SDK(`rmcp`) 존재 확인, 성숙도 충분. mcp도 Rust로 통일 확정. → [ADR-0007](decisions/ADR-0007-language-runtime.md)

## 다음

### [B-07] ENABLER M2 — mcp+cc 구현
- 산출: 두 세션 간 아이템 완주
- 영향: L0/L1 게이트, 북극성·손절선 계측 시작점([goals.md](goals.md))
- 선행: B-06

### [B-03] ASSUMPTION 비동기 조정 충분성
- 질문: M2에서 아이템이 지연 없이 유용하게 소비되는가, 아니면 "너무 늦어서 이미 필요없어짐"이 자주 발생하는가?
- 타임박스: M2 실사용 관찰(별도 시간 불필요, M2 자체가 실험)
- 산출: 완주 사례 vs 지연으로 무의미해진 사례 카운트
- 영향: [goals.md](goals.md) 손절선, A-1
- 선행: B-07

### [B-05] ASSUMPTION 예산/연장 폭주 방지
- 질문: 아이템이 연속 도착하는 상황에서 연장 상한이 실제로 트립하는가?
- 타임박스: M1~M2 구현 중 통합테스트 1개
- 산출: 폭주 시나리오 테스트 통과/실패
- 영향: [open-questions.md](open-questions.md) #35, A-3
- 선행: B-06, B-07

### [B-09] GATE 사용률/번다운 계측 파이프라인 구축
- 산출: [goals.md](goals.md) 선행지표를 실제로 재는 수단
- 영향: 손절선 판정 가능 여부
- 선행: B-02, B-07

### [B-11] EVAL 서버 상시가동 전략 설계
- 질문: 어느 머신이 코어 서버를 상시 호스팅하는가? 그 머신이 꺼지거나 재부팅되면 조정은 어떻게 복구되는가?
- 타임박스: 미정(M4 배포 설계와 함께 다룰 규모)
- 산출: [open-questions.md](open-questions.md) #50 해소, 상시가동 전략 1개 확정
- 영향: [vision.md](vision.md) S4(머신 간 핸드오프) 시나리오의 전제 자체, M4 배포 결정([open-questions.md](open-questions.md) #45~48)
- 선행: 없음(M1과 무관하게 지금 설계 논의 시작 가능)

### [B-12] ENABLER M1 HTTP API 기본 바인딩을 인증 공백 기간 동안 제한
- 질문: 인증(#42~43)이 갖춰지기 전까지 코어 HTTP API의 기본 바인딩을 무엇으로 할 것인가(예: localhost/사설망 제한)?
- 타임박스: M1 스캐폴딩과 동시(경량 결정)
- 산출: [open-questions.md](open-questions.md) #51 해소, M1 구현에 기본값 반영 + README/AGENTS.md에 안내 문구
- 영향: [coverage.md](coverage.md)의 "폐쇄 환경" 가정을 코드 수준에서 최소 보강, [ADR-0005](decisions/ADR-0005-public-scope.md) 공개 스코프와의 정합
- 선행: B-06(M1 코어 구현 — 서버가 있어야 바인딩 기본값을 정할 수 있음)

## 나중

### [B-08] GATE L1 통과 판정
- 산출: S1~S6([vision.md](vision.md)) 전부 수동 실행 확인
- 영향: [quality-ramp.md](quality-ramp.md) L1
- 선행: B-07

### 부록 잔여 유예 항목
[open-questions.md](open-questions.md)의 나머지 항목들 — 구현 중 순차 결정.
