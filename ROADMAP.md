# ROADMAP

Lens는 단일 Rust 워크스페이스다. `lens-*` 크레이트가 각각 하나의 진단 도메인을
담당하고 `lens-cli`의 `lens` 바이너리와 `lens-mcp` 서버가 공용 진입점을 제공한다.
현재 사용법과 지원 경계는 [README](README.md)와 [crates/README.md](crates/README.md)를 기준으로 한다.

| 크레이트 | 구현한 방향 | 이후 검토할 범위 |
|---|---|---|
| lens-core | 공용 오류·file identity·증거 수집·`lens.bundle/v2` 아카이브(생성/검사/검증/show/extract)·결정적 JSON·시간 유틸 | 증거 스키마 버전 협상 |
| lens-disk | 아레나 트리 스캔, 심볼링크/하드링크 보존, 병렬 stat, 중복 해시, Trash(list/restore/topdir 폴백) | 정확성을 보존하는 증분 재스캔, `trash empty` |
| lens-abi | ELF 심볼/dynamic 섹션 파싱, 정의·미정의 심볼 분리, 버전 요구사항, `Compatibility` diff | 타입 멤버/레이아웃 비교(현재는 타입명 표면만 추출) |
| lens-log | mmap 인덱싱, memchr 라인 스캔, 구조화 필드, 레벨/부분문자열/regex 필터, `.gz`·stdin·lossy UTF-8, 다중 입력·`--follow`·`log stats` | 다중 소스 시계 보정 |
| lens-test | JUnit XML 파싱(다중 파일·glob 병합), 상태별 집계, 결정적 run id, `testlens.diff/v2` | 추가 runner 형식 |
| lens-trace | strace 파싱·재개 스티칭, `CLONE_FILES` 공유 fd 테이블, CLOEXEC/`close_range`, fd 누수·I/O 집계, diff | 대규모 스트림 인덱싱 |
| lens-sys | systemd unit + `.d/` drop-in 병합, `Key=` 리셋, `%u`/`%h` 확장, 검색 경로 병합 로더, 방향 사이클+엣지 기원, 파서/로더 진단 | 지원 systemd 의미론의 단계적 확대, `--enabled-only` 사이클 범위 |
| lens-build | `compile_commands.json` 파싱, include 해석·캐시, 전이 헤더 클로저, impact diff | 빌드 시스템별 추가 adapter |
| lens-env | venv/site-packages 표준 메타데이터, PEP 508 마커·extras·버전 충돌, shadowing(top_level 매핑) | 더 다양한 인터프리터 ABI 증거 |
| lens-net | `/proc/net` 소켓 파싱, 프로세스 inode 상관, UDP 바인드 감지, owner_state 구분, 5-tuple diff | 좀 더 넓은 넷링크 커버리지 |
| lens-cli | Outcome 0/1/2 종료 코드, `--format` 통일, 포렌식 번들·doctor·셸 자동완성 | `--no-timestamps` 옵트인 여부 결정, `--include-events` 상세 출력 |
| lens-tui | ratatui 탭 UI(실데이터), 백그라운드 스캔+스피너, 메모리 탐색, `/` 검색, `--log` | 스캔 취소·워커 큐 전면 리팩터링, 스냅샷 탐색 UX |
| lens-mcp | MCP 툴 서버(17 툴), `limit`/`page_path`/`offset` 페이지네이션+`_pagination`, log tail, diff 툴, `isError` 계약, 프롬프트·`--allow-root` | 리소스 확장 |

공통 릴리스는 설치 결과·체크섬·provenance를 검증한 뒤 draft를 게시한다.
태그는 워크스페이스 단일 버전 `vX.Y.Z` 형식을 사용한다.
로컬 검증과 GitHub 호스팅 환경에서의 실행은 구분해서 기록한다.
