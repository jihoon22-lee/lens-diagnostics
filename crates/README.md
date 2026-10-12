# Lens 크레이트 레퍼런스 (`crates/`)

Lens는 단일 Rust 워크스페이스다. 각 `lens-*` 크레이트가 하나의 진단 도메인을
담당하고, `lens`(lens-cli) 바이너리와 `lens-mcp` 서버가 공용 진입점이다.

공통 계약:
- **Fail-Closed**: 파싱 불가·증거 부족 시 안전한 것으로 간주하지 않는다.
- **Evidence-Bound**: 출력은 관측된 파일 오프셋/줄/경로와 해시에 근거한다.
- **Deterministic JSON**: `lens_core::to_deterministic_*`로 키를 정렬해 출력한다.
- **3상태 호환성**: `Compatibility::{Compatible, Incompatible, Uncertain}`.

---

## lens-core — 공통 기반

| 모듈 | 역할 |
|---|---|
| `identity` | `SafeInput`(open→fstat→verify)과 `FileIdentity`로 TOCTOU 방어, `mmap()` |
| `hash` | SHA-256 `digest_bytes`/`digest_file`, `IncrementalHasher` |
| `diff` | `Compatibility` 3상태 enum, `SetDiff<T>` |
| `evidence` | `Evidence`, `Source`, 상한 있는 `BoundedCollector` |
| `json` | 결정론적 JSON 직렬화(키 정렬) |
| `glob` | `*`/`?` 미니 글롭(`glob_match`) — `--exclude` 패턴과 테스트 입력 글롭이 공유 |
| `time` | UTC/로컬 ISO-8601 타임스탬프 유틸 |
| `bundle` | `lens.bundle/v2` — tar.gz + `manifest.json`(SHA-256, 크기·항목 상한) 생성/검사/검증, `read_bundle_entry`(단일 엔트리 추출), `extract_bundle`(검증 우선 + 이름 검증 + `--force` 없이 덮어쓰기 거부) |

## lens-disk — 파일시스템 분석

- `ArenaTree`: `u32` 인덱스 연속 아레나. 자식 삽입 O(1)(`last_child`),
  크기 집계는 반복형 post-order(깊은 트리 스택 안전). 디렉터리 집계에서
  `hard_link_count > 1`인 같은 inode는 노드 id 순 첫 등장만 합산한다 —
  파일 노드 자체는 각자 크기를 유지하고, `(0,0)` identity는 절대
  중복 제거하지 않는다.
- `DiskScanner`: 최대 깊이/항목 수, 제외 패턴, 마운트 경계(`one_file_system`),
  심볼링크 루프 감지(링크 **타깃**의 dev/ino 사용). `ScanOptions::parallel`이
  실제 rayon 병렬 stat을 수행. `exclude_patterns`는 `lens-core`의
  `glob_match` 규칙 — `/` 포함은 루트 상대 경로 glob, `*`/`?` 포함은 이름
  glob, 나머지는 이름 정확 일치(`.git`이 `.github`를 제외하지 않음).
  `depth_limit_is_error`(기본 true)가 false면 `max_depth` 도달이 스캔
  오류로 기록되지 않고 `complete`를 깎지 않는다 — 절단된 디렉터리 수는
  `ScanResult::depth_limited`로 양쪽 모드에서 집계된다.
- `DuplicateFinder`: 크기→inode 묶음→4KB 부분 해시→전체 SHA-256 순으로 필터.
  같은 inode의 하드링크는 회수 가능 용량에서 중복 계산하지 않는다.
  `find_in_tree`는 `DuplicateReport`(groups + 해시 실패 `errors` +
  `total_reclaimable_bytes`)를 반환한다.
- `TrashManager`: FreeDesktop Trash v1.0 (`files/` + `.trashinfo` 영수증).
  `list()`/`restore_by_name()`은 `.trashinfo` + `.lens.json` identity
  사이드카로 trashed 파일의 교체 여부를 검증(fail-closed: 대상 점유·
  identity 불일치·안전하지 않은 이름/경로는 거부). EXDEV 시
  `$topdir/.Trash-$uid`(0700)로 폴백.
- 스키마: `diskmap.snapshot/v2`, `SnapshotDiff`.

```rust
use lens_disk::{DiskScanner, ScanOptions, DuplicateFinder, SnapshotV2};
let res = DiskScanner::new(ScanOptions::default()).scan("/path")?;
let report = DuplicateFinder::new(1<<20).find_in_tree(&res.tree, "/path".as_ref())?;
let snap = SnapshotV2::from_tree(&res.tree, res.root_id, res.complete, res.truncated);
```

## lens-abi — ELF/ABI 분석

- `inspect_elf`: ELF 헤더, `.dynamic`(NEEDED/RPATH/RUNPATH/SONAME/PT_INTERP),
  `.gnu.version_r`/`.gnu.version_d`의 심볼 버전 요구사항, dynsym 증거
  (`defined` 여부, 디맹글링 포함)를 수집.
- `dwarf`: `.debug_info`가 있으면 gimli로 선언 타입명을 추출(최대 10,000건,
  stripped 바이너리는 진단 메모만 남김). 타입 그래프(멤버/레이아웃 비교)는 미구현.
- `diff_reports`: 심볼/vtable/타입 집합 차분 + 속성 변경(타입·바인딩·데이터 크기)
  + 헤더 변경을 종합해 3상태 호환성 판정. 경로/의존성 변경은 `Uncertain`.
- 스키마: `abilens.report/v2`, `abilens.diff/v3`(v3에서 `compatibility` 어휘가 `compatible|incompatible|uncertain`으로 통일되고 버전 요구사항 diff가 `abi` 필드에 채워짐).

## lens-log — 로그 분석

- `LogIndexer`: mmap + `memchr`로 라인 오프셋 테이블 구축. `.gz` 입력은
  상한 있는 해제(512 MiB)로, 스트림(`from_reader`, stdin `-` 경로)은
  해제된 바이트를 무익명 임시 파일에 스풀 후 mmap해 처리한다 — 해제
  버퍼가 프로세스 익명 메모리를 차지하지 않는다. invalid UTF-8 줄은 인덱싱되고
  `lossy_lines()`로 집계되며 `get_line_lossy`가 U+FFFD로 노출.
- `parse_line`: JSONL(`level`/`msg`/`ts` 등, 키 대소문자 무시, 나머지 키는
  `fields`에 보존)과 `LEVEL ...` 휴리스틱. `detect_level`은 레벨만 빠르게 반환.
- `LogFilter`: min_level/query(대소문자 무시 substring)/`--regex`
  (regex::Regex)/source/`since_ms`~`until_ms` 타임스탬프 창(양끝 포함,
  미판별 줄은 `include_unknown`으로 복원). `matches_line`은 no-op
  필터와 substring-only 거절을 파싱 없이 단락.
- `timestamp`: `--since/--until` 인자 파서(RFC 3339/오프셋 없는
  ISO/날짜만 — UTC 해석)와 줄 타임스탬프 추출기(ISO·syslog 접두사,
  JSONL `ts`/`time`/`timestamp`). 연도 없는 syslog는 `default_syslog_year()`
  = 현재 UTC 연도, `--year`/`syslog_year`로 재정의.
- 스키마: `loglens.session/v2`.

## lens-test — JUnit 파싱

- `quick-xml` 스트리밍 파서: 속성 엔티티 디코딩, testcase 상태
  (failure/error/skipped) 메타데이터, `<system-out>`/`<system-err>`와
  `<properties>` 수집, 열린 태그 잔존 시 `complete=false`(잘린 XML 감지).
- 케이스 신원은 `suite::classname::name` — 같은 이름의 케이스가 다른
  suite에서 충돌하지 않는다. 중복 identity는 덮어쓰지 않고 diff의
  `diagnostics`로 보고.
- `diff_test_runs`는 구조화된 `CaseChange` 목록(`regressions`,
  `fixes`, `new_failures`, `removed_tests`, `skipped_changes`)을 반환.
  skipped→passed는 fix가 아니다. `merge_test_runs`는 모듈별 JUnit
  파일들을 하나의 run으로 병합.
- run id는 입력 해시 기반 결정적 생성.
- 스키마: `testlens.run/v1`, `testlens.diff/v2`(v2에서 regressions 등이
  문자열에서 `CaseChange` 구조로 변경).

## lens-trace — strace 분석

- strace 라인 파싱, `unfinished`/`resumed` 스티칭, 지연 시간 집계.
  에러는 `syscall:errno` 키로 집계.
- fd 추적은 tid별 테이블이되 `CLONE_FILES` clone은 fd 테이블을 공유하고,
  `O_CLOEXEC` fd는 `execve`에서 해제되며 `close_range`도 처리한다.
  open/openat/socket/accept/dup/pipe2([3,4]) 등 생성 시스템콜과
  `+++ exited`에서 잔여 fd를 `fd_leaks_by_process`에 귀속.
- 스키마: `tracelens.snapshot/v1`, `tracelens.diff/v1`.

## lens-sys — systemd 정적 분석

- `load_units`(공용 로더): 단일 파일 또는 디렉터리의 unit을 로드하고
  `<unit>.d/*.conf`를 이름순으로 병합. 읽기 실패는 진단 스텁으로 보존.
- `load_units_merged(dirs)` + `SYSTEMD_SEARCH_DIRS`: `/etc` → `/run` →
  `/usr/lib` → `/lib` 우선순위 병합 — 상위 디렉터리의 유닛(또는 mask/
  alias)이 하위를 가리고, drop-in은 모든 디렉터리에서 수집해 낮은
  우선순위부터 적용. doctor/TUI/`sys` 기본 경로가 이 로더를 공유한다.
- 파서는 할당 순서를 유지해 `ExecStart=` 등 빈 할당 리셋 의미론을 지원하고
  `%u`/`%h` 등 specifier를 `User=` 기준으로 확장.
- `OrderingGraph`: Before/After DAG + Tarjan SCC 사이클 탐지 — 사이클은
  실제 방향 경로 + 엣지 기원(파일:라인, 디렉티브)으로 보고.
- 파서/로더는 `SYNTAX_GARBAGE_LINE`, `SYNTAX_SECTION_HEADER`,
  `UNIT_REF_MISSING` 진단을 유닛에 붙이고 스냅샷 `diagnostics`에 집계된다.
- 스키마: `servicelens.snapshot/v1`, `servicelens.diff/v1`.
- 지원 범위는 `systemd-255-subset-v1`로 명시 — 전체 systemd 의미론의 부분 집합.

## lens-build — 빌드 영향도

- `compile_commands.json` 파싱: `-I`/`-isystem`, `-D`, `-std`, `-o`,
  `-include`(강제 포함) 추출, 경로 정규화(`normalize_path`).
- `ImpactGraph::add_translation_unit`: 소스 파일의 `#include`를 디스크에서
  해석(`"…"`은 포함 파일 기준 우선, `<…>`은 검색 경로만)하고 헤더 간
  전이 클로저를 구축. include 추출은 파일별 캐시로 중복 I/O를 피하고,
  읽기 실패 소스와 미해결 include는 `missing_sources`/
  `unresolved_includes`로 집계된다. `compute_impact`는 역방향 BFS로
  영향받는 TU를 반환(`ImpactReport`는 `scan_truncated`/`hint` 포함).
- 스키마: `buildscope.snapshot/v4`, `buildscope.diff/v1`, `buildscope.impact/v1`.

## lens-env — Python 환경 감사

- 인터프리터를 실행하지 않는 정적 분석: `pyvenv.cfg`, `*.dist-info/METADATA`
  (PEP 376/503), `Requires-Dist` 검증, 프로젝트 소스의 stdlib/서드파티
  모듈 섀도잉 탐지.
- `markers.rs`: PEP 508 환경 마커 평가(`python_version`, `sys_platform`,
  `extra` 등, `and`/`or`/괄호, 버전 비교). 거짓 마커 요구는 스킵,
  `extra`는 `--extras`로 활성화한 것만 적용, 범위 불일치는
  `version_conflicts`, 평가 불가는 `unevaluated`.
- 섀도잉은 프로젝트 루트/`src/` 1레벨의 `.py`와 `__init__.py` 패키지
  디렉터리만 후보로 하고, 설치 패키지는 `top_level.txt`/`RECORD`의
  모듈명으로 비교한다(dist 이름이 아니라 import 이름 기준).
- 스키마: `envlens.snapshot/v1`, `envlens.diff/v1`.

## lens-net — 소켓 포렌식

- `/proc/net/{tcp,tcp6,udp,udp6,unix}` 파싱(주소는 커널의 호스트 엔디안
  표기 — `to_ne_bytes` 의도적 사용), `/proc/[pid]/fd`로 프로세스 상관.
- UDP는 LISTEN 상태가 없으므로 바인드된 wildcard 소켓(st=07, remote `*:0`)을
  리스너로 집계.
- diff는 리스너를 (kind, 주소, 포트)로, 연결을 5-tuple로 매칭(inode는
  스냅샷 간 불안정). `127.0.0.1→0.0.0.0` 확장을 감지.
- 소유자 불명 소켓은 `owner_state`로 `orphan`(실제 미보유)과
  `owner_unknown`(`/proc/<pid>/fd` EACCES)을 구분하고, summary에
  `uninspectable_processes`를 기록.
- 스키마: `lens.net/v1`(additive 필드만 추가).

## lens-ops — 공유 오퍼레이션 레이어

- `lens-cli`와 `lens-mcp`가 공유하는 수집+판정 로직. 각 오퍼레이션은
  `Result<Report<T>>`를 반환: `data`(도메인 결과) + `findings`
  (`Severity` Info/Warn/Fail + 안정적 `code`) + `complete`(증거 불완전
  여부) + `diagnostics`.
- `Report::is_actionable(threshold)`이 "종료 코드 1" 판정의 단일 근거 —
  불완전 증거는 항상 actionable.
- 모듈: `disk`, `abi`, `log`(`open`/`build_filter`), `test`(파일/디렉터리/
  글롭 입력), `trace`(fail-closed 입력 검증), `sys`, `build`, `env`,
  `net`, `bundle`(수집기+검증/해제 래퍼+번들간 diff), `doctor`, `json`
  (스키마 검증 포함 JSON 로더), `detect`(`lens inspect`/`diff`용 입력
  자동 판별 — 확장자·매직·처음 64 KiB 내용으로 `InputKind` 결정).
- `env::diff`는 스냅샷 JSON과 venv 디렉터리를 혼합해 받는다(디렉터리는
  그 자리에서 inspect). `disk::duplicates`는 `ScanOptions`를 통째로 받는다.
- 모든 도메인 diff는 값 수준 분류기(`diff_values`/`diff_runs`/
  `diff_snapshots`)로 분리돼 있어 파일 diff와 `bundle::diff`가 같은
  findings 코드를 공유한다. `bundle::diff`는 양쪽 번들을 무결성 검증
  후 공통 섹션을 비교하고 `lens.bundle-diff/v1`을 만든다.
- `doctor`는 실마운트별 용량+inode 검사(`/proc/self/mounts`,
  가상 fs 제외·fsid 중복 제거), world-writable PATH 검사, WARN/FAIL
  검사의 `next_command` 제안, `doctor::diff_reports` 리포트 비교를
  제공한다.
- clap·출력 없음 — 프레젠테이션은 소비자 책임.

## lens-cli — `lens` 통합 CLI

- `src/cli.rs`: clap 커맨드 트리. `src/ops.rs`: 렌더러 — `lens-ops`
  결과를 text/JSON으로 출력하고 `is_actionable`을 종료 코드로 변환.
  `main.rs`는 얇은 래퍼.
- 전역 옵션(`--format`, `--json`, `--fail-on warn|fail`, `-q/--quiet`)은
  `Cli`의 global 인자라 서브커맨드 앞·뒤 모두에서 동작한다.
  `-q`는 `output::diagln!`으로 찍는 비오류 stderr 진단만 끈다.
- 최상위 `inspect`/`diff`: `lens_ops::detect`로 입력을 판별해 동등한
  도메인 명령으로 재디스패치한다(출력·종료 코드 동일, `--as`로 강제).
- `doctor`: 스토리지/네트워크/서비스/환경 종합 헬스체크(구현은
  `lens-ops`에 있고 `lens_cli::doctor`로 re-export된다).
- text 모드에서는 도메인 출력 뒤에 `Findings:` 블록([FAIL]/[WARN])
  이 출력된다. JSON 출력에는 findings가 섞이지 않는다.
- `bundle create/inspect/verify/show/extract/diff`: `lens.bundle/v2`
  아카이브(`diff`는 `lens.bundle-diff/v1` 출력). `snapshot`은 호스트
  기준선 번들(host/net/sys/doctor 자동 수집).
- `tui`: `lens_tui::run`으로 진입. `completion`: 셸 자동완성 생성.

## lens-tui — 터미널 대시보드

- 4개 탭 실데이터: Storage(아레나 스캔), Services(merged `load_units_merged`
  결과), Logs(`LogIndexer`로 로그 tail + 레벨 컬러, `--log` 지정 가능),
  Network(리스너 목록).
- 디렉터리 스캔은 백그라운드 스레드(`mpsc`)에서 돌고 스피너로 표시된다.
  Enter/Backspace는 스캔된 아레나 트리 안에서 메모리 탐색한다(재스캔 없음).
- `lens_tui::run(path, log)`가 렌더 루프를 소유해 `lens-tui` 바이너리와
  `lens tui`가 공유한다.

## lens-mcp — MCP 서버

- JSON-RPC 2.0 over stdio로 진단 툴을 노출. `initialize`는 지원 버전
  (`2025-06-18`, `2025-03-26`, `2024-11-05`) 중 요청된 것을 에코하고,
  미지원/누락이면 `2025-06-18`로 응답한다. `id` 없는 요청(노티피케이션)
  에는 응답하지 않는다. 도구 실패는 `result.isError`로 반환한다.
- 도구 결과의 첫 content 아이템은 데이터 JSON, findings/불완전 증거가
  있으면 두 번째 아이템으로 `{"findings":[...], "complete":bool}`을
  추가한다.
- 모든 도구에 `limit`(배열당 최대, 기본 200)/`page_path`/`offset`
  페이지네이션: `offset`은 `page_path`가 가리키는 배열에만 적용되고,
  잘린 배열은 최상위 `_pagination`(`path → {total, returned, offset}`)
  로 보고된다 — 배열 안에 마커 객체를 넣지 않는다. `[]` 경로는 모든
  발생의 합계를 낸다. 응답 상한 64KiB 초과 시 limit 25로 재시도하고
  `_response_clamped: true`. 상대 경로는 서버 cwd 기준.
- `lens_log_filter`는 `tail`(끝 N라인), `regex`, `min_level`,
  `include_unknown`을 지원하고 `matches`를 자체 페이징한다
  (`limit`/`offset`).

| 툴 | 용도 |
|---|---|
| `lens_disk_scan`, `lens_disk_duplicates` | 스토리지 분석/중복 탐지 |
| `lens_abi_inspect`, `lens_abi_diff` | ELF/심볼/버전/DWARF 검사·비교 |
| `lens_log_filter` | 로그 필터(tail/regex/페이지) |
| `lens_trace_analyze` | strace 분석 |
| `lens_sys_cycles`, `lens_sys_diff` | systemd 사이클/스냅샷 비교 |
| `lens_test_diff` | JUnit 회귀 diff |
| `lens_net_diff` | 네트워크 스냅샷 diff |
| `lens_build_impact` | 헤더 영향도 |
| `lens_env_check` | venv 의존성 검사 |
| `lens_net_inspect` | 소켓/리스너 검사 |
| `lens_bundle_verify`, `lens_bundle_diff` | 번들 무결성 검증/섹션 비교 |
| `lens_doctor` | 종합 진단(잘못된 경로 → isError) |

```json
{ "mcpServers": { "lens": { "command": "/path/to/lens-mcp" } } }
```
