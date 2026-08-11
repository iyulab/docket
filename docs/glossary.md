상태: v0 정렬 스냅샷 | 2026-08-11 | 이 문서는 구현 중 갱신된다

# Glossary

## 코어 어휘 대응표

계층 누수를 막는 규율이다. 코어 코드에 오른쪽 열의 단어가 등장하면 리뷰에서 거부한다.

| 코어 어휘 | 응용 어휘 (3번 층 등) |
|---|---|
| worker | session, Claude Code 세션 |
| topic | repo, repository, 저장소 |
| item | card, ticket, message, mail |
| claim | assign, 배정 |
| body | markdown, .md 파일 |
| stream | hook, 훅 |
| budget | token budget, 토큰 예산 |

오른쪽 열의 개념은 3번 층에서 왼쪽 열로 번역되어 코어에 전달된다. 번역이 일어나는 지점이 곧 계층 경계다.

## 용어 각주

- **`topic`**: JMS 계열의 topic(pub-sub, 구독자 전원이 같은 메시지를 받는 fan-out)이 아니다. Kafka의 topic + consumer group(경쟁 소비자, 한 명만 받음) 의미다. docket의 아이템은 한 워커만 집으므로 후자에 대응한다.
- **`claim` vs `assign`**: `claim`은 워커가 스스로 집는 것(pull), `assign`(응용 계층 용어, §11.4 "강제 배정")은 관리자가 대신 트리거하는 것(push). 코어 프리미티브는 `claim` 하나이고, `assign`은 그 `claim`을 관리자 권한으로 트리거하는 응용 계층의 진입점이다.
- **`state` vs `resolution`**: `state`는 워크플로 단계(`open/claimed/resolved/closed`), `resolution`은 닫힌 이유(`done/duplicate/wontfix/invalid`). 둘을 분리하는 것은 Bugzilla/Jira의 관행 — 상태가 하나 줄고, 관리자 조작 각각에 의미가 붙는다.
- **`task` vs `question`**: `task`는 상태 기계를 가진 아이템(보드에 남음). `question`은 즉시 실패하는 질의(보드에 안 남음). [vision.md](vision.md) S3.
