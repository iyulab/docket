# AGENTS.md

이 문서는 이후 세션(에이전트)이 docket에서 작업할 때 지킬 규칙이다. 목적은 하나다 — **[open-questions.md](docs/open-questions.md)의 유예 항목을 임의로 확정하지 못하게 한다.**

## 비타협 원칙

**P-1. 코어는 소비자를 모른다.** `docket-core`에 worker·topic·item·claim·body·stream·budget 외의 개념(session, repo, ticket, hook, token budget 등 — [glossary.md](docs/glossary.md) 대응표)이 등장하면 안 된다. PR/커밋에서 이를 발견하면 리뷰에서 거부한다.

## 그 외 원칙 (비타협은 아니나 위반 시 짚을 것)

- **P-2. 파일은 정본이 아니다.** `docket-cc`의 파일 표현은 코어 DB의 반영일 뿐이다.
- **P-3. 오케스트레이터가 아니다, pull만.** 중앙이 워커에게 자동으로 작업을 배분하는 코드를 추가하지 않는다. 사람의 수동 배정(콘솔)은 예외.

전체 근거: [principles.md](docs/principles.md).

## 비목표 (구조적으로 하지 않는 것)

- 파일 동기화 서비스
- 오케스트레이터(자동 배분)
- 실시간 채팅
- 다중 사용자 협업 (Later — [ADR-0006](docs/decisions/ADR-0006-single-owner-later.md), 임의로 앞당기지 말 것)

## 현재 품질 레벨

**목표: L1.** 단, 클레임 배타성은 L0/L1에 이미 포함된 것으로 취급한다 — "단순하게 간다"는 이유로 동시 클레임 처리를 대충 구현하지 않는다. 세부: [quality-ramp.md](docs/quality-ramp.md).

## 유예 목록을 다룰 때

[open-questions.md](docs/open-questions.md)에 50개 이상의 `[결정 필요]` 항목이 있다. 이 항목들은 **의도적으로 유예된 것**이지 빠뜨린 것이 아니다. 구현 중 이 항목 중 하나를 결정해야 하는 상황이 오면:

1. 그 항목이 정말 지금 결정해야 하는지 먼저 확인한다(다른 우회로 없이 막혔는가).
2. 결정한 뒤에는 [open-questions.md](docs/open-questions.md)에서 해당 항목을 지우고, Type-1급이면 `docs/decisions/ADR-NNNN-*.md`를 새로 만든다.
3. 임의로 결정하고 문서화하지 않은 채 넘어가지 않는다 — 다음 세션이 같은 질문을 다시 만난다.

## 공개 저장소 규율

이 리포는 전체 공개다([ADR-0005](docs/decisions/ADR-0005-public-scope.md)). 절대경로·개인명·머신명·내부 티켓 ID·발견 경위 서술을 커밋 메시지·코드 주석·문서에 남기지 않는다.
