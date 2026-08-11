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

### [B-10] SPIKE Rust MCP SDK(`rmcp` 등) 성숙도 확인
- 질문: 공식/커뮤니티 Rust MCP SDK가 실사용 가능한 수준인가? TS SDK 대비 기능 격차는?
- 타임박스: 1시간
- 산출: [ADR-0007](decisions/ADR-0007-language-runtime.md) mcp 언어를 잠정→확정으로 갱신
- 영향: ADR-0007, mcp 계층 툴체인 통일 여부
- 선행: 없음

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

## 나중

### [B-08] GATE L1 통과 판정
- 산출: S1~S6([vision.md](vision.md)) 전부 수동 실행 확인
- 영향: [quality-ramp.md](quality-ramp.md) L1
- 선행: B-07

### 부록 잔여 유예 항목
[open-questions.md](open-questions.md)의 나머지 항목들 — 구현 중 순차 결정.
