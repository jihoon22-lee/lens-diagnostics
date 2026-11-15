# ROADMAP

Lens는 단일 Rust 워크스페이스다. `lens-*` 크레이트가 각각 하나의 진단 도메인을
담당하고 `lens-cli`의 `lens` 바이너리와 `lens-mcp` 서버가 공용 진입점을 제공한다.
현재 사용법과 지원 경계는 [README](README.md)와 [crates/README.md](crates/README.md)를 기준으로 한다.

| 크레이트 | 현재 구현 |
|---|---|
| lens-core | 공용 오류·file identity·증거 수집·`lens.bundle/v2` 아카이브(생성/검사/검증/show/extract)·결정적 JSON·시간 유틸 |
| lens-disk | 아레나 트리 스캔, 심볼링크/하드링크 보존, 병렬 stat, 중복 해시, Trash(list/restore/topdir 폴백) |
| lens-abi | ELF 심볼/dynamic 섹션 파싱, 정의·미정의 심볼 분리, 버전 요구사항, `Compatibility` diff |
| lens-log | mmap 인덱싱, memchr 라인 스캔, 구조화 필드, 레벨/부분문자열/regex 필터, `.gz`·stdin·lossy UTF-8, 다중 입력·`--follow`·`log stats` |
| lens-test | JUnit XML 파싱(다중 파일·glob 병합), 상태별 집계, 결정적 run id, `testlens.diff/v2` |
| lens-trace | strace 파싱·재개 스티칭, `CLONE_FILES` 공유 fd 테이블, CLOEXEC/`close_range`, fd 누수·I/O 집계, diff |
| lens-sys | systemd unit + `.d/` drop-in 병합, `Key=` 리셋, `%u`/`%h` 확장, 검색 경로 병합 로더, 방향 사이클+엣지 기원, 파서/로더 진단 |
| lens-build | `compile_commands.json` 파싱, include 해석·캐시, 전이 헤더 클로저, impact diff |
| lens-env | venv/site-packages 표준 메타데이터, PEP 508 마커·extras·버전 충돌, shadowing(top_level 매핑) |
| lens-net | `/proc/net` 소켓 파싱, 프로세스 inode 상관, UDP 바인드 감지, owner_state 구분, 5-tuple diff |
| lens-ops | CLI/MCP 공통 수집·판정, findings·complete, doctor·bundle |
| lens-cli | Outcome 0/1/2 종료 코드, `--format` 통일, 포렌식 번들·doctor·셸 자동완성 |
| lens-tui | ratatui 탭 UI(실데이터), 단일 스캔 워커·최신 요청 큐·취소·오래된 결과 차단, 메모리 탐색, `/` 검색, `--log` |
| lens-mcp | MCP 툴 서버(18 툴), `limit`/`page_path`/`offset` 페이지네이션+`_pagination`, log tail, diff 툴, `isError` 계약, 프롬프트·`--allow-root` |

## 유지 범위

현재 9개 진단 도메인과 CLI/TUI/MCP, bundle, `lens.result` v1을 유지한다.
추가 도메인·테스트 형식·lint parser·공통 실행 플랫폼은 계획하지 않는다.
새 범위는 실제 사용에서 확인된 문제와 회귀 사례가 있을 때만 별도 결정한다.
TUI의 `lens-ops` 통합, 독립 `lens-tui` 제거, 설치·릴리스 도구 교체는 이번 정리에 포함하지 않는다.
기존 진입점과 배포 계약을 유지한다.

공통 릴리스는 설치 결과·체크섬·provenance를 검증한 뒤 draft를 게시한다.
태그는 워크스페이스 단일 버전 `vX.Y.Z` 형식을 사용한다.
로컬 검증과 GitHub 호스팅 환경에서의 실행은 구분해서 기록한다.
