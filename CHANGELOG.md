# Changelog

Lens는 워크스페이스 단일 버전으로 릴리스된다. 태그는 `vX.Y.Z` 형식이다.

## Unreleased

기본 검사·비교 흐름과 선택적 반복 작업 설정을 개선한다. CLI/TUI/MCP/bundle과 기존 도메인 JSON을 유지한다.

- 선택적 `lens.toml`, 사용자 설정, `--config`/`--no-config`, `config init`/`explain`을 추가한다. 출력은 사용자 선호, 판정·디스크 범위는 프로젝트 설정을 우선하고 명시 CLI가 최종 우선한다. cwd에서 VCS/home/root 경계까지만 탐색하며 입력 자료에서는 설정을 찾지 않는다.
- `lens run NAME`으로 명명한 journal/docker/podman 수집이나 snapshot을 실행한다. 설정 상대경로는 선언 파일 기준이며 argv 실행·수집 한도·stdout/stderr/provenance 계약을 유지한다.
- **기본 출력 변경:** 설정과 명시 옵션이 없는 터미널에서는 report 명령이 text를 사용한다. 파이프에서는 기존 명령별 기본값을 유지한다. 자동화에는 `--format json` 또는 `--format envelope`를 명시한다.
- JUnit 텍스트에 실패 사례와 원인을 표시한다. 요약 목록과 긴 실패 메시지는 생략 수·상세 조회 안내와 함께 제한하며 JSON 데이터는 보존한다. 테스트 실패는 기존 Warn 판정이므로 `--fail-on warn`에서 종료 1이다.
- finding의 선택적 `evidence`에 실제 도메인 데이터 포인터와 관측 경로/객체를 기록한다. lens.result v1의 추가 필드이며 기존 JSON reader와 원래 argv identity 의미를 유지한다. Rust `Finding` struct literal에는 새 필드를 반영해야 한다.
- TUI 스캔 worker를 하나로 제한하고 최신 대기 요청만 유지한다. 로그 통계는 타임스탬프별 저장 대신 점진적 버킷 집계를 사용한다. 입력 인덱스는 여전히 행 수에 비례한다. 로그 diff는 시간 범위 차이·미확인 기간을 표시하며 rate 회귀를 추정하지 않는다.
- 첫 사용 문서와 Python 없는 native 설치 절차, v1 소비자 corpus 및 재현 가능한 대용량/CLI/TUI/MCP 검증을 추가한다.

## 0.10.0

Lens Diagnostics 전환 릴리스. 기존 `lens` CLI와 도메인 JSON을 유지하면서 수집 범위, 분석 완결성, 자동화 결과 계약 및 검증된 Linux 배포를 추가했다.

### 사용자 기능

- `--format envelope`로 `lens.result` v1을 선택한다. producer/version, IDK_RUN_ID 기반 run_id, 실행 argv/cwd/판정, complete, findings, diagnostics와 기존 data를 함께 제공한다. 기본 JSON 구조는 유지한다. 유한 log inspect/filter/stats/diff도 지원하고 follow/TUI/completion/trash에는 명시적으로 오류를 반환한다.
- build inspect/impact가 missing source, forced include 누락, 읽지 못한 header, unresolved include, scan cap을 incomplete로 판정한다. 매크로/조건부 전처리/builtin 경로를 재현하지 않는 정적 근사라는 설명을 결과와 문서에 남긴다.
- net scope는 procfs origin 및 collected/unavailable/denied/failed/excluded capability를 구별한다. doctor의 evidence_complete와 scope_changed가 미관측 범위를 정상이나 개선으로 오인하지 않게 한다. `.lens`에는 선택한 입력과 생성한 artifact를 기록하고, 명시 procfs의 누락된 identity를 현재 호스트 값으로 채우지 않는다.
- `sys inspect --user` / `sys cycles --user`는 현재 계정과 XDG 검색 경로를 사용한다. manager identity는 Service.User와 독립적이며 템플릿·drop-in·instance 전 과정에서 유지한다. 기존 system manager 기본 동작을 보존한다.
- TUI의 `c`로 스캔을 취소한다. 새 요청은 이전 결과 채널을 버려 늦게 끝난 워커가 화면을 덮지 못한다. 대형 디렉터리는 enumeration/stat 이전에 entry 상한을 적용한다.
- `lens log diff A B`와 일반 `lens diff`, MCP `lens_log_diff`가 severity 수와 관측 시간 구간을 비교한다. ERROR/FATAL 증가는 findings다. 서로 다른 길이의 구간은 자동 정규화하지 않는다.
- `lens collect journal|docker|podman TARGET --output capture.lens`로 지정한 소스만 수집한다. 직접 argv 실행, 기본30초/최대300초 timeout, stream별8MiB 상한, stdout/stderr/source exit 별도 보존을 적용한다. 서비스나 컨테이너를 변경하지 않는다.

### 설치와 이행

- 저장소의 canonical URL은 https://github.com/jihoon22-lee/lens-diagnostics 이다. 제품명만 변경하며 `lens-*` crate, CLI, `.lens`, 기존 schema 문자열을 유지한다.
- 공개 target은 `x86_64-unknown-linux-musl`이다. ELF interpreter가 없는 정적 실행파일을 검증하며, `BUILD.json`에 source SHA/target/Rust 버전/실행파일 digest를 기록한다. 기존 `linux-x86_64` archive URL은 동일 bytes alias로 유지한다. GNU source build에는 호환 glibc가 필요하다.
- `install.py --version v0.10.0`은 체크섬/내부 digest를 확인하여 기본 `~/.local/bin`에 설치한다. `--archive`와 `--checksums`로 폐쇄망 설치를 지원하며 기존 일반 실행파일 교체에는 `--force`가 필요하다.
- 불완전한 build/systemd/수집 evidence 및 lossy UTF-8 필터·통계는 warning 임계값과 무관하게 종료1이다. 이전의 성공 종료에 의존한 자동화는 종료 코드와 envelope의 complete를 함께 처리해야 한다. 실제 process exit와 envelope 판정, MCP 페이지 절단은 별개다.
- Rust 호출자는 새 public struct 필드(`NetReport.scope`, `SystemdSnapshot.manager`, `DoctorCheck.evidence_complete`, `DoctorDiff.scope_changed`) 및 enum variant를 반영해야 한다. 저장된 이전 JSON은 serde 기본값으로 읽으며, 생략된 scope는 관측 범위 미기록을 뜻한다.

### 검증과 지원 한계

- Workspace 테스트, clippy, fmt, CLI/MCP 실제 stdio, 배포 흐름/오프라인 installer 검증을 수행한다. idk 실제 소비자 시험에서 run identity, 종료1, incomplete/findings, 독립 출력 보존을 확인했다.
- WSL2 Linux x86_64에서 계정/user manager 및 journal(empty result)/Docker(source failure) adapter와 생성 bundle 무결성을 확인했다. Podman runtime은 로컬에 없으므로 실제 runtime 실행 PASS를 주장하지 않는다.
- Linux/WSL/container의 실제 수집은 namespace·mount·권한·설치된 source CLI 범위에 한정한다. Native Windows/macOS, 다른 CPU의 공개 artifact, PE/Mach-O, compiler와 동등한 전처리 영향 분석은 이번 지원 범위가 아니다.
- 공개 workflow는 업로드한 bytes를 다시 내려받아 SHA-256, source SHA, 버전 및 실행 smoke를 검증한 후 release를 공개한다. checksum은 무결성 확인이며 독립 서명이 아니다.

## 0.9.0

v0.8.0 후속 리뷰에서 확인한 14건을 수정한 안정화 릴리스: 번들 마스킹·입력 무결성, systemd 자원 제한·의미론, MCP 복구, 도메인 판정 정확도를 보완했다.

### Rust API 호환성 변경

- systemd 로더·그래프 생성과 doctor 검사·보고서 생성이 자원 상한 오류를 전파하도록 `Result`를 반환한다. Rust 호출자는 반환값을 처리해야 한다. 공개 API 변경에 맞춰 0.9.0으로 마이너 버전을 올렸다.
- 기존 JSON 스키마는 유지하며, 강제 include diff의 `forced_include_order`는 선택적 필드다.

### 수정

- 번들 JSON을 디코딩된 값 기준으로 마스킹해 이스케이프된 비밀값 유출과 JSON 손상을 막는다. UTF-8이 잘못된 로그는 마스킹 번들에서 생략하고 불완전 수집으로 보고한다.
- tar 종료 후에도 gzip 디코더를 상한 내에서 끝까지 읽어 CRC 손상과 잘린 트레일러를 거부한다.
- systemd 템플릿 확장에 유닛·참조·깊이·이름·메타데이터 상한을 적용하고 CLI/doctor/MCP/TUI/번들에 오류를 전파한다. 메타데이터는 복제되는 drop-in 경로와 모든 소유 필드를 포함하며, 순서 의존성은 provenance와 평탄화 목록 중 한 번만 집계한다. 합성 인스턴스에도 전용 drop-in과 검색 경로 우선순위를 적용한다.
- systemd 의존성의 빈 할당이 기존 관계를 지우지 않도록 수정하고, `%u`/`%h`를 `User=`와 무관한 시스템 매니저 기준으로 확장한다. `.wants`/`.requires` 링크 변경도 diff에 반영한다(`systemd-255-subset-v2`).
- MCP 요청 한도를 바이트 단위로 판정해 한글·이모지 경계의 초과 요청과 잘못된 UTF-8 요청 이후에도 다음 요청을 처리한다.
- Linux `close_range`의 UNSHARE=2/CLOEXEC=4 플래그를 숫자·기호 표기에서 동일하게 해석한다.
- PEP 440의 복합 pre/post/dev 버전과 epoch/local/wildcard 비교를 수정한다. PyPA 기준 2,376개 비교 fixture를 추가한다.
- 이름 부분일치만으로 DWARF 타입의 공개 ABI 노출을 확정하지 않는다. 노출 관계를 입증하지 못한 크기 변경은 `uncertain`으로 보고한다.
- 강제 include의 순서 변경을 선택적 `forced_include_order.before/after`로 보고한다. `--no-unix` 필터 후에는 소켓 요약과 findings를 다시 계산한다.
- 스냅샷 회귀 테스트에 명시적 로그 fixture를 사용해 호스트 로그 크기에 따른 실패를 제거한다.

## 0.8.0

전수 코드 리뷰 결함 수정·리팩토링 및 포렌식 플랫폼 기능 고도화 릴리스: 제로-클론 결정론적 JSON 직렬화, 제로-할당 UTF-8 글로브 엔진, FIFO/심볼릭링크 샌드박스 보안 강화, PII/시크릿 마스킹 엔진(`--redact`), 도메인 분석기(test, trace, sys, build, env, net) 전수 결함 수정 및 CLI/TUI/MCP 사용성 개선.

### 추가

- `lens-core`:
  - 증거 보존형 PII/시크릿 마스킹 엔진 (`redact_str`, `redact_bytes`): API 키(`sk-`, `ghp_`, `AKIA`, `AIza`), Bearer 토큰, `password`/`secret` 키-값, PEM 개인키를 `<REDACTED:sha256:8자리>` 솔트 해시로 결정론적 치환.
  - `lens snapshot --redact` 및 `lens bundle create --redact` 플래그 지원.
  - 제로-클론 결정론적 JSON 직렬화 (`BTreeMap` 기반) 및 UTF-8 바이트 슬라이스 글로브 매처 (`[0-9]`, `[!a-z]` 지원).
  - `read_policy::check_boundary`: Dangling 심볼릭 링크 타깃 재귀 검증으로 샌드박스 우회 원천 차단.
- `lens-disk` / `lens-abi` / `lens-log`:
  - `lens-disk`: 비재귀 심층 디렉터리 스냅샷 직렬화 (`MAX_INLINE_DEPTH = 32`), dotfile 휴지통 복원, 중복 탐색 부분 해시 재사용 및 FIFO 제외.
  - `lens-abi`: DWARF 구조체 바이트 크기(`DW_AT_byte_size`) 추출 및 비호환 레이아웃 변경 감지, ELF visibility 및 `STT_TLS` 지원.
  - `lens-log`: 0바이트 빈 파일 mmap 방지, Syslog `[ERROR]` 구분자 탐지, 공백 구분 ISO 타임스탬프 타임존 오프셋 보존.
- 도메인 분석기:
  - `lens-test`: quick-xml 0.41 `Event::GeneralRef` 엔티티 복원, `trim_text(false)` 공백 보존.
  - `lens-trace`: 멀티스레드 `strace -f`의 `<unfinished ...>` 파싱 지원, 시스템콜 지연시간 퍼센타일(`p50_ns`, `p95_ns`, `p99_ns`) 산출.
  - `lens-sys`: 유닛 `masked` 및 `alias_of` 변경 감지, 템플릿 인스턴스 DAG 해석.
  - `lens-build`: UTF-8 주석 제거 손상 수정, `#include <...>` 시스템 헤더 오탐 제외.
  - `lens-env`: `Requires-Dist` 괄호 버전 명시자 파싱, PEP 440 Epoch 지원, editable self-shadowing 오탐 제외.
  - `lens-net`: PID 결정론적 정렬, 소켓 지연 읽기(Lazy Read), Unix 소켓 다중 공백 보존.
- CLI / TUI / MCP:
  - `lens inspect`, `lens disk scan`, `lens disk duplicates` 기본 경로 `.` 지원.
  - `lens log filter --follow --format jsonl` 지원 및 로그 로테이션 감지 시 라인 번호 초기화.
  - `lens doctor`: 소형 파티션 용량 평가 개선 및 `remediation` 후속 조치 가이드 추가.
  - `lens-tui`: Logs 탭 `g` 키 탐색, Network 탭 소켓 소유권 구분, 도움말 모달 `Clear` 렌더링.
  - `lens-mcp`: `check_allowed` 경계 검증 일원화, `lens_log_filter` 고속 매칭 및 `_pagination` 메타데이터 제공.

## 0.7.0

코드리뷰 후속 하드닝·사용성 개선 릴리스: 공유 오퍼레이션 레이어(`lens-ops`)와 findings 기반 종료 코드, 자동 판별 `lens inspect`/`lens diff`, `lens snapshot`과 번들 간 diff, 로그 follow/stats, MCP 샌드박스·prompts. 종료 코드 등 Breaking changes는 아래 섹션 참고.

### 추가

- 새 크레이트 `lens-ops`: 수집+판정 로직을 `Report<T>`(`findings`,
  `complete`, `diagnostics`)로 반환하는 공유 오퍼레이션 레이어.
  `lens-cli`와 `lens-mcp`가 더 이상 도메인 로직을 복제하지 않고 같은
  구현을 소비한다. `doctor` 구현도 이 크레이트로 이동했다.
- text 모드 출력에 `Findings:` 블록 — 도메인 출력 뒤에
  `Findings: N fail, M warn`과 `[FAIL]`/`[WARN]` 항목(안정적 `code` +
  메시지)을 출력한다. JSON 출력은 변경되지 않는다.
- MCP 도구 결과에 findings 두 번째 content 아이템 — findings 또는
  불완전 증거가 있으면 `{"findings": [...], "complete": bool}` JSON이
  데이터 아이템 뒤에 추가된다.
- MCP `page_path` 페이지네이션 인자 — `offset`을 적용할 배열을 점
  경로로 지정한다(배열 원소 내부는 `units[].diagnostics`처럼 `[]`).
- diff 입력 스키마 검증 — `env diff`/`sys diff`/`net diff`가 스냅샷
  JSON의 `schema`/`schema_version`을 생산자 값과 비교하고, 다른
  스키마의 파일을 넣으면 `expected schema` 오류(exit 2)로 거부한다.
- MCP `lens_test_diff`가 글롭 입력도 받는다(CLI와 같은 입력 확장).
- 전역 CLI 옵션: `--format text|json|jsonl`, `--json`, `--fail-on
  warn|fail`, `-q/--quiet`가 `Cli`의 global 인자가 됐다 — 서브커맨드
  앞·뒤 어느 쪽에 두어도 동일하게 파싱된다. `-q`는 비오류 stderr
  진단만 끄고 stdout과 실제 오류 출력은 그대로다.
- 최상위 `lens inspect <path>` / `lens diff <a> <b>`: 입력을 자동
  판별(ELF·JUnit·strace·compile_commands·venv·systemd·스냅샷
  JSON·번들·로그·디렉터리)해 해당 도메인 명령과 동일한 출력·종료
  코드를 낸다. `--as`로 판별을 강제할 수 있다.
- `disk duplicates`에 `--exclude`(반복), `-x/--one-file-system`,
  `--max-depth` 추가 — `disk scan`과 같은 스캔 범위 제한을 지원한다.
- `env diff`와 `lens diff`가 venv 디렉터리를 입력으로 받는다 —
  디렉터리는 그 자리에서 inspect돼 스냅샷 JSON과 자유롭게 혼합된다.
- `lens snapshot`: `snapshot-<hostname>-<UTC>.lens` 기준선 번들 —
  호스트/네트워크/systemd/doctor 리포트와 syslog를 항상 수집하고,
  `--disk`/`--env`/`--test`/`--trace`/`--log`로 소스를 추가한다.
  `bundle create`에도 `--doctor`로 doctor 리포트를 넣을 수 있다.
- `lens bundle diff <a> <b>`와 `lens diff a.lens b.lens`: 두 번들을
  무결성 검증 후 공통 섹션(sys/test/trace/env/net/doctor/disk/log)을
  도메인 diff 분류기로 비교한다 — findings 코드는 파일 diff와 동일.
  출력 스키마는 `lens.bundle-diff/v1`, 한쪽에만 있는 엔트리는
  `bundle.only_in_*` Info.
- MCP `lens_bundle_diff` 도구(도구 수 15 → 16).
- `log filter`/`log inspect`/`log stats`가 다중 입력을 받는다 — 각
  항목은 `-`, 파일, `.gz`, `*`/`?` 파일명 glob. 다중 입력 시
  `--limit`은 전체 합산, text는 `[<file>:<line>]` 접두사, jsonl은
  `"file"` 필드, json은 `{"inputs": […], "matched", "total"}`.
- `log filter --follow`(`-f`): 초기 필터 패스 뒤 파일을 폴링해 추가
  줄을 같은 필터로 낸다 — truncate/rotate 시 처음부터 재개하며
  stderr에 안내. 정규 파일 하나, `--context 0`, text/jsonl에서만
  유효.
- `lens log stats`: 필터가 통과시킨 줄의 레벨 분포 + 시간 히스토그램
  (`lens.log-stats/v1`, `--bucket 1m|5m|15m|1h|1d`, 미지정 시 ~60
  버킷이 되도록 자동 선택).
- MCP `lens_log_stats` 도구(도구 수 16 → 17), `lens-mcp --allow-root
  DIR` 경로 샌드박스(반복 가능 — canonicalize된 루트 밖 경로는
  도구 오류로 거부), `prompts` capability(`incident-triage`,
  `compare-snapshots`).
- doctor: 실마운트별 용량 검사(`/proc/self/mounts`의 실파일시스템,
  `/`는 `Root Disk Capacity`) + inode 여유율(<5% Fail, <15% Warn) +
  world-writable PATH 디렉터리 WARN + WARN/FAIL 검사의 `-> Next:`
  명령 제안(`lens disk scan <mount> --top 20` 등). `DoctorCheck`에
  `next_command` 필드 추가(스키마 additive).

### 수정

- `lens` stdout이 이제 64 KiB 버퍼로 출력된다 — 라인마다 write+flush
  하던 것을 없애 `log filter` 등 대량 출력이 빨라졌다. stdout 쓰기
  실패 시 종료 코드가 1에서 2로 변경됐다(1은 findings 전용).
- `bundle create --test`가 디렉터리·glob 입력을 받는다. 이전에는
  디렉터리를 주면 `Is a directory` I/O 오류로 수집이 실패했다.
- `bundle create --log`가 `.gz`와 stdin(`-`)을 올바르게 임베드한다.
  내용은 해제된 바이트로 저장되며 엔트리 이름은 `.gz` 접미사를 뗀다
  (`app.log.gz` → `logs/app.log`, stdin은 `logs/stdin.log`). 이전에는
  압축 바이트의 꼬리를 그대로 넣어 손상된 gzip이 임베드됐고, `-`는
  파일을 못 찾아 내용이 빠졌다.
- `trace diff`와 `bundle create --trace`가 `trace analyze`와 같은
  fail-closed 입력 검증을 적용한다. 비-strace 입력은 diff/analyze에서
  exit 2이고, 번들에는 진단으로 기록된다.
- `doctor --root`가 네트워크·서비스 검사를 루트로 스코프한다. 호스트
  `/proc`와 호스트 systemd 유닛을 더 이상 읽지 않으며, 루트에 없는
  검사는 새 `SKIP` 상태로 보고한다. 명시한 `--systemd-dir` 로드 실패는
  PASS 대신 WARN으로 보고한다.
- MCP `lens_log_filter`가 무효 UTF-8 라인을 lossy로 매칭하고 스캔
  범위의 `lossy_lines`를 보고한다(이전에는 조용히 건너뛰었다). MCP
  `lens_trace_analyze`도 같은 fail-closed 게이트를 적용한다.
- `bundle create`가 verify와 같은 엔트리 수·크기 상한을 사전에
  강제하고, `--force` 없이는 기존 번들을 덮어쓰지 않는다.
- `bundle extract`가 단일 패스로 스테이징·검증한다. 목적지의
  심볼링크는 절대 따라가지 않고(`--force`여도 심볼링크·디렉터리
  덮어쓰기 거부), 검증 실패 시 스테이징된 파일은 남지 않는다.
- `disk scan` 디렉터리 집계에서 `hard_link_count > 1`인 같은 inode를
  한 번만 계산한다 — 하드링크가 있는 트리의 `size`·`allocated_size`
  합계가 실제 사용량으로 줄어든다(파일 노드 자체는 변경 없음).
- 사용자가 지정한 `disk scan --max-depth`는 더 이상 스캔 오류가 아니다
  — `complete`가 true로 유지돼 exit 0이고, 텍스트 출력에 절단된
  디렉터리 수가 표시된다(기본 안전 상한 512 도달은 여전히 불완전).
- MCP `initialize`의 프로토콜 협상이 지원 버전(`2025-06-18`,
  `2025-03-26`, `2024-11-05`)만 에코하고, 미지원·누락 버전에는
  `2025-06-18`로 응답한다(이전에는 임의 버전을 그대로 에코하고
  누락 시 `2024-11-05`였다).
- TUI Logs 탭이 invalid UTF-8 줄도 U+FFFD로 표시한다(이전에는 조용히
  누락). 명시한 `--log` 파일이 열리지 않으면 상태바에 오류 이유를
  표시한다.
- `id` 없는 JSON-RPC 요청(노티피케이션)에는 어떤 메서드에도 응답하지
  않는다(이전에는 `initialize`/`tools/call` 등에도 응답했다). 파싱
  오류는 계속 `id: null` 에러를 반환한다.
- `lens-mcp --allow-root`에서 생략된 경로 인자가 호스트 기본값을 읽지
  않는다 — `lens_net_inspect`는 `proc_dir`, `lens_doctor`는
  `root_path`가 필요하다(기본값 자체가 허용 루트 안이면 예외).
  `lens_build_impact`는 compile DB의 `directory`/`file`/include
  디렉터리/`-include` 경로를 모두 루트 안으로 검증한다.
- `lens log stats`가 연도 없는 syslog 타임스탬프를 `--year`에 지정된
  연도로 집계한다(이전에는 항상 현재 연도로 해석됐다).
- `lens inspect`/`lens diff` 입력 판별이 `{`로 시작하는 비스냅샷
  JSON·JSONL을 usage 오류 대신 Log로 판별한다. 이름이 다른 compile
  DB도 첫 배열 원소를 스트리밍 파싱해 64 KiB를 넘어도 인식한다.
- `log filter --follow`가 개행 없이 끝나는 마지막 줄을 초기 패스에서
  내지 않는다 — append로 완성될 때 한 번만 번호가 매겨진다.
- doctor의 마운트별 용량 검사가 `/`의 파일시스템 ID를 먼저 등록한다 —
  `/`의 바인드 마운트·서브볼륨이 중복 용량 검사를 만들지 않는다.
- JSON-RPC 요청의 `"id": null`은 누락된 `id`와 구분된다 — null id에도
  `id: null`로 응답하고, `id` 멤버가 아예 없을 때만 노티피케이션이다.
- MCP findings content 아이템이 `limit`(기본 200)으로 제한되고
  `findings_total`/`findings_truncated`를 보고한다 — 대량 findings가
  응답 크기 상한을 넘기지 않는다.
- `bundle diff`가 `./`로 시작하는 엔트리 이름을 verify와 같은 규칙으로
  정규화한다 — `./reports/…`로 패킹된 번들이 정상 이름의 번들과
  섹션별로 비교된다(`only_in_*` 오탐 없음).

### Breaking changes

- `doctor` JSON에 `SKIP` 상태와 `summary.skipped` 필드가 추가됐다.
  비systemd 환경의 services 검사는 PASS 대신 SKIP으로 보고한다.
- 라이브러리 `lens_core::create_bundle_archive` 시그니처에 `overwrite`
  매개변수가 추가됐다.
- 새 exit-1 조건(findings): `disk duplicates` 불완전 스캔/해시 오류,
  `test parse` 불완전 JUnit, `trace diff` 신규 에러,
  `sys inspect`/`sys cycles` 사이클, `sys diff` 신규 사이클,
  `env inspect`/`env check` 누락·충돌, `net diff` 신규 리스너,
  `bundle create` 수집 경고(번들은 계속 생성되나 exit 1).
- MCP 페이지네이션 의미론 변경: `offset`은 이제 `page_path`가
  가리키는 배열에만 적용되고, 배열 안의 `_truncated` 마커는 최상위
  `_pagination` 메타데이터로 대체됐다.
- 스키마가 다른 JSON을 diff 입력으로 주면 exit 2다(이전에는 잘못된
  스키마로 역직렬화되거나 모호한 오류가 났다).
- `disk scan --exclude` 의미론: `/` 포함 패턴은 스캔 루트 상대 경로
  글롭, `*`/`?` 포함은 이름 글롭, 그 외는 이름 정확 일치로 바뀌었다
  (이전에는 이름 부분 문자열 매칭이라 `.git`이 `.github`·`.gitignore`도
  제외했다). 같은 규칙이 `disk duplicates --exclude`에도 적용된다.
- `--fail-on`이 전역 옵션이 됐다 — 이제 `doctor`뿐 아니라 모든 명령에
  적용된다(기본 `fail`은 변경 없음). 옵션을 서브커맨드 뒤에 두는 기존
  호출 형태는 계속 유효하다.

## 0.6.1

의존성 보안·릴리스 자동화·문서 정합성 후속 정리 릴리스. 상세 내역은 아래 섹션 참고.

### 인프라

- ratatui 0.30으로 업그레이드해 `paste` 의존성(RUSTSEC-2024-0436,
  unmaintained)을 제거했다.
- 수동 `chore(release):` 범프 머지 시 manifest 버전과 태그를 비교해
  태그가 없으면 Release 워크플로를 자동 호출한다. 이전에는 매번
  `gh workflow run`이 필요했다.

## 0.6.0

사용성·안전성 후속 개선 릴리스. 상세 내역은 아래 섹션 참고.

### Breaking changes

- `lens abi inspect`/`lens abi diff`의 비ELF 입력이 fail-closed로
  바뀌었다. 이전에는 `status: "non-elf"` 리포트를 출력하고 종료 0이었고,
  이제는 `error: <path> is not an ELF file` + 종료 2이며 리포트를
  출력하지 않는다. 라이브러리와 MCP 도구는 계속 `status: "non-elf"`
  리포트를 반환하므로 프로그래밍 호출자는 영향이 없다.

### 추가

- `lens log filter --since/--until <TS>`: 타임스탬프 창 필터(RFC 3339·
  `YYYY-MM-DD HH:MM:SS`·`YYYY-MM-DD`, 오프셋 없으면 UTC, 양끝 포함).
  ISO·syslog 접두사와 JSONL `ts`/`time`/`timestamp`를 인식하고, 연도
  없는 syslog는 현재 UTC 연도(또는 새 `--year`)를 가정. 타임스탬프
  미판별 줄은 제외+stderr 보고, `--include-unknown`으로 복원.
  MCP `lens_log_filter`에도 `since`/`until`/`year` 인자 추가.

### 개선

- `log filter`/`log inspect`의 `.gz`·stdin(`-`) 입력이 해제된 바이트를
  전부 프로세스 힙에 올리지 않고 무익명 임시 파일에 스풀 후 mmap한다.
  512 MiB 해제 상한과 초과 시 오류는 그대로. 큰 `.gz`에서 피크 익명
  메모리가 크게 줄었다.

### 수정

- `lens completion`이 닫힌 파이프에서 패닉(종료 101) 대신 0으로 종료.
- `lens doctor --root`가 호스트의 `/etc/ld.so.preload` 대신
  `<root>/etc/ld.so.preload`를 검사.
- 테스트가 `/tmp/lens-*` 디렉터리를 남기지 않도록 모든 테스트가
  `tempfile::TempDir` 가드를 사용.
- TUI Storage·Services·Network 목록이 선택 항목을 따라 스크롤해
  첫 페이지 밖의 선택(`j/k`, `PgDn`, `g/G`)이 보이지 않던 문제 수정.

## 0.5.0

CLI/MCP 계약·사용성 대수정 릴리스. 상세 내역은 아래 섹션 참고.

### Breaking changes

이번 릴리스는 사용자 가시 동작이 여럿 바뀐다. 마이그레이션 노트:

- **종료 코드 규약이 `diff(1)` 관례로 통일됐다.** `0`=clean,
  `1`=findings 있음, `2`=사용·입력·런타임 오류. 이전에는 잘못된
  입력이 `0`이나 `1`로 빠지는 경로가 있었으므로, 스크립트에서
  "0이 아니면 오류"로 해석하던 로직은 findings(exit 1)를 오류와
  구분하도록 수정해야 한다. 영향받는 명령: `test diff`(회귀/신규
  실패→1), `abi diff`(incompatible·uncertain→1), `sys cycles`
  (사이클→1), `env check`(누락/충돌→1), `doctor`(`--fail-on` 기준
  이상→1), `disk scan`(incomplete→1), `trace analyze`(fd 누수→1),
  `bundle verify`(무결성 불일치→1).
- **`testlens.diff` 스키마가 v1→v2로 변경됐다.** `regressions`/
  `fixes`가 `Vec<String>`에서 `CaseChange{id,before,after,message}`
  구조로 바뀌고 `new_failures`/`removed_tests`/`skipped_changes`/
  `diagnostics`가 추가됐다. 소비자는 `schema` 필드를 확인하고 v2
  파서로 전환해야 한다.
- **`lens log filter --min-level`이 레벨 불명 줄을 제외한다.**
  이전에는 Unknown 레벨 줄도 출력됐다. 기존 동작이 필요하면
  `--include-unknown`을 추가한다.
- **잘못된 입력이 더 엄격하게 거부된다(exit 2).** venv가 아닌 경로,
  읽을 수 없는 `--proc-dir`/`--systemd-dir`/`--root`, JUnit 루트가
  아닌 XML, 대부분 파싱되지 않는 strace 입력, 소스 없는 `bundle
  create`, `--force` 없는 출력 덮어쓰기가 이전의 조용한 성공/빈
  결과 대신 오류가 된다.
- **`lens-mcp` 도구 실패가 `result.isError=true`로 반환된다.**
  알려진 도구의 실행 실패는 더 이상 JSON-RPC error가 아니다.
  클라이언트는 `isError`를 검사해야 한다(알 수 없는 도구/메서드는
  여전히 JSON-RPC error).
- **`lens disk trash`는 `list`/`restore` 서브커맨드가 경로 인자보다
  우선한다.** 이름이 `list`/`restore`인 파일은 `./list`처럼 경로
  접두어를 붙여 trash 한다.

### 안전성 (Phase 0)
- stdout 파이프가 닫혀도 패닉하지 않고 exit 0으로 종료
  (`lens net inspect --json | head -1` 등).
- `lens bundle create`: 소스별 수집 실패를 전체 중단 대신 매니페스트
  `diagnostics`에 기록하고 번들을 계속 생성한다. 출력 파일이 이미 있으면
  거부하며 새 `--force` 플래그로만 덮어쓴다.
- 잘못된 입력이 조용히 성공하던 경로들을 0이 아닌 종료 코드의 명확한 에러로
  변경: venv가 아닌 경로(`env check`/`env inspect`), 읽을 수 없는 `net/tcp`
  (`net inspect --proc-dir`), 존재하지 않는 `doctor` `--root`/`--procfs`/
  `--systemd-dir`, JUnit 루트가 아닌 XML(`test parse`/`test diff`), 대부분
  파싱되지 않는 `trace analyze` 입력.
- `lens disk scan` 텍스트 출력: 권한 오류나 절단이 있으면 "Scan completed
  successfully" 대신 "Scan INCOMPLETE" 배너와 오류 수를 출력.
- `lens-mcp`: `ping` 메서드 지원. 알려진 도구의 실행 실패는 JSON-RPC error
  대신 `result.isError: true` + 텍스트 콘텐츠로 반환(MCP 권장 계약). 알 수
  없는 도구는 `-32602`, 알 수 없는 메서드·JSON 파싱 오류는 기존처럼
  JSON-RPC error.
- TUI: 시작 경로를 canonicalize하여 `lens tui`(경로 ".")에서 Backspace로
  상위 디렉터리 이동이 동작. 패닉 시 터미널(raw mode/alt screen)을 복원하는
  panic hook 추가.

### 정확성 (Phase 1)
- `lens env check`: PEP 508 환경 마커(`python_version`, `sys_platform`,
  `extra`, `and`/`or`/괄호, 버전 비교)를 venv의 실제 버전/플랫폼에 대해
  평가. 거짓 마커 요구는 스킵해 거짓 "missing"을 제거하고, `extra == "x"`
  는 새 `--extras a,b`로 활성화한 extra에서만 적용. 설치됐지만 범위를
  벗어난 버전은 `version_conflicts`, 평가 불가 마커는 `unevaluated`로
  missing과 구분해 보고. uv venv의 `version_info` 키 인식 추가.
- `lens log filter --min-level`: 레벨을 판별할 수 없는 줄을 제외하고
  제외 수를 stderr로 보고(`--include-unknown`으로 복원). syslog PRI와
  커널 printk `<N>` 접두사에서도 레벨 추출.
- `lens trace analyze`: `CLONE_FILES` 스레드가 fd 테이블을 공유하도록
  모델링(스레드의 close가 공유 fd를 해제), `O_CLOEXEC` fd를 `execve`에서
  해제, `close_range` 지원. 에러 집계 키를 `syscall:errno`로 변경.
- `lens abi`: 동적 심볼을 정의(export)/미정의(import)로 구분 — diff가
  정의된 심볼만 제거 판정에 쓰고 import는 `imports` SetDiff로 별도
  비교(import만의 변경은 compatible). weak 심볼은 binding=`weak`로 표기.
  `abilens.diff/v3` 스키마.
- `lens sys cycles`: 사이클을 SCC 멤버 정렬이 아닌 실제 방향 경로로
  보고하고 각 엣지의 기원(유닛 파일:라인, 디렉티브)을 표시. `/dev/null`
  masked 유닛은 엣지 대상에서 제외하고 alias/템플릿 이름 해석.
- `lens sys diff`: 유닛의 모든 섹션·키를 비교(`User=` 추가 등). 입력으로
  스냅샷 JSON 외에 유닛 디렉터리도 허용.
- `lens build inspect`: `reverse_impact`는 직접 includer만 유지하고,
  헤더 체인을 거친 간접 includer까지 포함하는 `transitive_impact` 추가
  (`buildscope.snapshot/v4` 유지, 신규 필드).

### CLI 계약 (Phase 2)

**Changed (breaking)** — 종료 코드 규약을 `diff(1)` 관례로 통일:
`0`=clean, `1`=findings 있음, `2`=사용·입력·런타임 오류. 출력 텍스트는
그대로이고 종료 코드만 바뀐다. findings는 `test diff` 회귀/신규 실패,
`abi diff` incompatible·uncertain(uncertain은 수동 검토가 필요하므로
findings로 분류 — 문서 표 참고), `sys cycles` 사이클 발견, `env check`
누락/충돌 의존성, `doctor`의 `--fail-on` 기준 이상, `disk scan`
incomplete, `trace analyze` fd 누수. Phase 0에서 0이 아닌 값으로
통일했던 잘못된 입력 경로는 이제 **2**로 종료한다.
- `lens doctor --fail-on warn|fail`(기본 `fail`) 추가.
- `--format text|json`을 전 명령에 통일. `text`는 사람이 읽는 요약
  모드(예: `abi inspect`는 SONAME/NEEDED/export·import 수), `json`은
  기존 결정적 스키마. 명령별 기본값은 유지. `net inspect --no-unix` 추가.
  `disk scan`/`net inspect`/`doctor`의 `--json`은 `--format json`
  별칭으로 유지.
- `lens disk scan`: `--max-depth`, `--exclude`(반복), `-x`/
  `--one-file-system`, `--top N`(기본 10) 노출. 텍스트 크기는
  KiB/MiB/GiB로 표시.
- `lens test diff`: 결과를 `testlens.diff/v2`로 구조화 —
  `regressions`/`fixes`가 `CaseChange{id,before,after,message}` 목록이
  되고 `new_failures`, `removed_tests`, `skipped_changes`,
  `diagnostics`(중복 identity) 추가(**스키마 v1→v2**). skipped→passed는
  fix로 집계하지 않는다. 파일/디렉터리/`*.xml` glob 다중 입력 지원.
- `lens net inspect`: 소유자 불명 소켓을 `owner_state`로
  `orphan`과 `owner_unknown`(권한 부족)을 구분하고 summary에
  `owner_unknown_sockets`/`uninspectable_processes` 추가
  (`lens.net/v1` additive). doctor는 권한 부족 소켓을 orphan WARN으로
  세지 않고 "sudo로 재실행" 권고를 단다.
- `lens disk duplicates`: `--format text|json` 추가. 해시 실패를 조용히
  버리지 않고 `errors`로 보고, 스캔 경고도 출력, 크기는 사람 친화적
  단위 + `total_reclaimable_bytes` 요약.

### 워크플로우 (Phase 2b)

- `lens build impact`: 상대 `--header`를 cwd뿐 아니라 compile database
  디렉터리와 각 엔트리의 `directory` 기준으로 해석 — 프로젝트 하위
  디렉터리에서도 동작. 그래프에 없는 헤더는 basename 유사 후보와 경로
  전달 방법을 힌트로 출력(종료 코드는 유지). 리포트에
  `missing_sources`/`unresolved_includes` 카운트와 스캔 절단 경고를
  추가하고, 파일별 include 추출 결과를 캐시해 대형 프로젝트의 중복 I/O를
  제거.
- `lens disk trash`: 다중 경로 입력과 `--dry-run` 지원.
  `trash list`/`trash restore <name>` 추가 — `.trashinfo` 메타데이터와
  파일 옆 identity 사이드카로 복원 시 교체 여부를 검증(fail-closed).
  EXDEV(다른 파일시스템)는 FreeDesktop `$topdir/.Trash-$uid`(0700)로
  폴백. `list`/`restore`는 `--trash-dir`로 topdir trash를 지정 가능.
- `lens log`: invalid UTF-8 줄을 조용히 건너뛰지 않고 U+FFFD로
  표시하며 `lossy_lines` 카운트를 JSON/stderr로 보고. `.gz` 입력은
  상한 있는 해제(512 MiB)로 지원하고 `-`는 stdin을 읽는다.
  `log filter`에 `--regex`, `--limit`, `--context`, `--format jsonl`
  (매치 줄당 JSON 오브젝트) 추가.
- `lens sys`/`doctor`/TUI: systemd 유닛 로더를 검색 경로 병합 방식으로
  공유화 — `/etc`, `/run`, `/usr/lib`, `/lib` 우선순위 병합(/etc가 최상위,
  drop-in은 전 디렉터리에서 수집해 낮은 우선순위부터 적용). `sys
  inspect`/`sys cycles`는 경로 인자를 생략하면 시스템 검색 경로를 사용;
  명시 경로는 여전히 존재하지 않으면 오류.
- `lens bundle`: `bundle show <bundle> <entry>`와 `bundle extract
  <bundle> <dest> [--force]` 추가 — 추출 전 무결성 검증, `..`/절대/역슬래시
  엔트리 이름 거부, `--force` 없이 덮어쓰지 않음. `--log`는 요약 외에
  원본 로그 내용을 `logs/` 아티팩트로 포함(8 MiB 꼬리 상한). 실패 메시지를
  "cryptographic verification failed"에서 "integrity check failed"로 정정
  — 매니페스트는 서명되지 않으며 무결성 확인이다. 무결성 검증 실패는 이제
  findings(exit 1)로 분류 — 검증은 정상 수행됐고 결과가 부정적인 경우다.

### 확장·정리 (Phase 3)

- `lens-mcp`: 응답 크기 제어 — 모든 도구가 `limit`(배열당 기본 200)·
  `offset`을 받아 잘린 배열 끝에 `{"_truncated": true, "total_before_
  truncation", "omitted"}` 마커를 붙이고, 직렬화 결과가 64KiB를 넘으면
  추가 클램프 + `_response_clamped` 표시. `lens_log_filter`는 앞 1000줄
  제한을 폐기하고 `tail`(끝 N라인 스캔)·`regex`·`include_unknown`·
  `limit`/`offset` 매치 페이지를 지원. diff 도구 추가: `lens_abi_diff`,
  `lens_sys_diff`(스냅샷 또는 유닛 디렉터리), `lens_test_diff`(파일/디렉터리),
  `lens_net_diff`(스냅샷 JSON). `initialize`가 클라이언트의
  `protocolVersion`을 에코한다. 모든 도구 설명에 상대 경로가 서버 cwd
  기준임을 명시하고 `lens_build_impact`의 상대 헤더는 컴파일 DB/entry
  디렉터리 기준으로 해석(CLI와 공유 헬퍼). `lens_doctor`의 존재하지 않는
  `root_path`/`proc_dir`/`systemd_dir`은 이제 `isError`를 반환한다.
- `lens tui`: 디렉터리 스캔이 백그라운드 스레드에서 돌고 UI가 즉시
  뜬다(스피너 표시, 스캔 오류/절단은 제목에 INCOMPLETE로 표시).
  Enter/Backspace는 스캔된 아레나 트리 안에서 메모리 탐색한다 — 스캔
  루트 위로 올라가거나 미완료 노드에 들어갈 때만 백그라운드 재스캔.
  `--log FILE`로 Logs 탭을 고정할 수 있고 `/` 검색(Enter 적용/Esc 해제),
  `PgUp/PgDn`, `g/G`, `?` 도움말을 지원한다.
- systemd: `sys inspect`의 스냅샷 `diagnostics`가 비어 있지 않게 됐다 —
  `=` 없는 쓰레기 줄(`SYNTAX_GARBAGE_LINE`), 닫히지 않은 섹션 헤더
  (`SYNTAX_SECTION_HEADER`), 로드된 유닛과 매칭되지 않는 의존 대상
  (`UNIT_REF_MISSING`; `.device`/`.mount` 등 생성형 유닛과 템플릿 인스턴스
  참조는 제외)를 파서·로더가 수집하고 CLI가 스냅샷 상위로 집계한다.
- `lens env`: shadowing 탐지 품질 — 패키지 내부 파일(`pkg/json.py`)은
  더 이상 stdlib `json` 오탐하지 않는다(프로젝트 루트와 `src/`의 1레벨
  후보만). 패키지 디렉터리(`logging/__init__.py`)도 탐지하고, stdlib
  목록을 3.11 `sys.stdlib_module_names` 수준으로 확장(`secrets`, `test`
  등). 설치 패키지 매칭은 dist 이름이 아니라 `top_level.txt`/`RECORD`의
  모듈명 기준(`PyYAML`→`yaml`, `typing-extensions`→`typing_extensions`).
- 메시지·도움말: 모든 위치 인자와 플래그에 설명을 채웠다(빈 `<PATH>`
  칸 제거). 사용 오류(인자 누락, 잘못된 플래그 값, `--force` 누락 등)는
  더 이상 "Corrupt or invalid input format:" 접두사가 붙지 않고
  `LensError::Usage`로 메시지만 출력. 파일 JSON 파싱 오류는 파일 경로를
  포함한다. GUIDE의 결정성 주장을 타임스탬프(`bundle` `created_at`) 예외와
  함께 명시했다.

### 의존성·보안

- `quick-xml` 0.37.5 → 0.41.0: RUSTSEC-2026-0194(중복 네임스페이스
  선언의 비선형 검사)와 RUSTSEC-2026-0195(`NamespaceResolver`의
  상한 없는 힙 할당 — 조작된 XML로 OOM/CPU 소진) 수정. JUnit 속성
  디코딩을 `decoded_and_normalized_value(XmlVersion::Implicit1_0)`로
  이전했고 파싱 동작은 동일하다.
- 미사용 의존성 제거: `serde_json`(lens-test/trace/sys/build/env/net/
  tui), `thiserror`(lens-cli/abi/mcp/test/trace/sys/build/env/net/tui),
  `lens-core`(lens-trace/build/env/tui).

## 0.4.3

무결성·정확성 대수정 릴리스. 상세 내역은 아래 섹션 참고.

### 무결성
- `lens bundle create/inspect/verify`가 단일 `tar.gz` + `manifest.json` 포맷
  (`lens.bundle/v2`)을 공유하도록 통일. 변조된 아티팩트를 fail-closed로 거부.
- 런타임 버전을 `CARGO_PKG_VERSION`에서, 타임스탬프를 `lens_core::time` 유틸에서
  생성하도록 정리. 하드코딩된 버전/시각 상수 제거.
- `SafeInput` 재검증이 심볼릭 링크의 링크 inode가 아닌 실제 타깃 메타데이터를
  비교하도록 수정(TOCTOU 버그).

### 정확성
- lens-abi: 동적 의존성·RPATH/RUNPATH·SONAME·인터프리터·심볼 버전 요구사항을
  실제로 수집. 정의/미정의 심볼 구분, `Compatibility` 어휘 통일,
  `.debug_info`의 선언 타입명 추출(상한 10k).
- lens-build: include 디렉티브 해석(`"`/`<>`/`-include`)과 전이 헤더 클로저 구현.
- lens-sys: `.d/` drop-in 스캔·병합과 `Key=` 리셋 의미론, `%u`/`%h` 확장 추가.
  `lens sys diff` 제공.
- lens-log: memchr 인덱싱, 레벨 파싱 할당 제거, 구조화 `fields` 채움.
- lens-disk: 형제 노드 삽입 O(1)화, 집계 반복문 전환, rayon 병렬 stat 구현.
- lens-net: `/proc/net` 호스트 엔디안 형식 문서화, UDP 바인드 소켓을 리스너로
  집계, diff가 (kind, address, port)/5-tuple로 매칭하도록 수정.
- lens-trace: fd 추적을 프로세스별 테이블로 분리하고 socket/accept/dup/pipe 등
  fd 생성 시스템콜 추적. `fd_leaks_by_process` 추가.
- lens-test: JUnit 속성 엔티티 디코딩, 상태 구분, 불완전 입력 감지.
- TUI: Services/Logs 탭에 실데이터 연결, `lens tui` 서브커맨드 추가.

### 릴리스/CI
- release-please 매니페스트를 0.4.2로 수정하고 `vX.Y.Z` 태그로 전환.
- release.yml에 `workflow_call` 추가, 불변 소스 커밋 해석, 다운로드 검증 후에만
  태그/퍼블리시하도록 시퀀스 재구성. 릴리스 플로우 테스트 6종 CI 연결.

### 정리
- 사용되지 않는 의존성 제거(lens-abi의 flate2, lens-disk의 walkdir,
  lens-log의 regex, lens-cli의 flate2).
- systemd unit 로딩을 `lens_sys::load_units`로 통합(CLI/doctor/MCP 중복 제거).
- README 성능 주장을 실측값으로 정정.

### 2차 다중 리뷰 기반 강화
- 번들 검증: 매니페스트를 `manifest.json` 정확 경로로만 해석(중첩 스푸핑 차단),
  중복·미등재 아카이브 엔트리 거부, 실제 해제 바이트 상한 적용, 번들 생성은
  임시 파일+원자적 rename으로 부분 쓰기·심볼링크 덮어쓰기 방지.
- lens-disk: 심볼링크 별칭이 visited_dirs를 오염시켜 하위 트리를 누락하던
  문제 수정, 깊이 상한 도달 시 `complete=false`, trash가 링크 타깃 대신 링크
  자체를 이동, `(dev=0,ino=0)` 파일은 경로 기반 중복 판정 폴백.
- lens-build: `-I` 검색 순서 보존 dedup, `..`/`./` include 경로 정규화,
  `-include`를 컴파일 디렉터리 기준 해석, 주석 내 `#include` 무시,
  스캔 상한 도달을 `scan_truncated`로 표면화.
- lens-trace: `+++ exited/killed/superseded` 계열 종결행 전부 처리,
  `strace -y` fd 주석(`3</etc/hosts>`) 제거, fork/clone 시 fd 테이블 상속,
  tid 재사용 generation 분리.
- lens-abi: 스트립된 바이너리용 program header(PT_DYNAMIC/PT_INTERP) 폴백,
  SHF_ALLOC 섹션 필터, 압축 DWARF 해제, `abi.versions` diff 비교.
- lens-sys: 유닛 suffix 전면 지원, 템플릿(`foo@.service`) drop-in 병합.
- lens-net: unix 소켓 Type/St 열 의미론 수정, diff 스키마 필드 추가,
  inode=0 고아 소켓 노이즈 제거.
- lens-test: `<properties>` 수집, suite 레벨 출력 보존, 인코딩 인지 디코딩,
  malformed 종료 태그 관대한 처리, 잘린 문서 `complete=false`.
- CLI/MCP/TUI: 표준에러 출력을 Display로, `--min-level`/`min_level` 검증,
  스캔 오류 표면화, `build inspect`가 `buildscope.snapshot/v4` 출력,
  `bundle create`에 최소 1개 소스 요구, TUI 리로드 실패 시 stale 상태 제거.
- CI: release.yml 태그 해석을 `git/ref/tags/<tag>`로 교체(이전 `/commits`
  리스트 엔드포인트는 항상 성공해 비교가 무의미했음), check_docs를 CI에 연결,
  assemble_pages가 `crates/README.md`를 `crates/index.md`로 발행.

## 0.4.2

Lens 통합 워크스페이스의 첫 태그 릴리스.

---

## 이전 이력 (Lens 통합 이전 — 레거시 제품별 버전 시대)

아래 항목들은 8개 독립 도구(diskmap, abilens, loglens, testlens, tracelens,
servicelens, buildscope, envlens) 시절의 기록으로, `{product}/vX.Y.Z` 태그로
버전이 관리됐다.

### 2026-10-03 — portfolio expansion published

- Add TraceLens: saved strace analysis, source evidence, snapshot/diff, Qt GUI.
- Add TestLens: JUnit/CTest collection, diff/history, CI policies and offline HTML.
- Add ServiceLens: offline systemd unit/drop-in provenance, graph, snapshot/diff.
- Correct BuildScope replay/source bounds, LogLens session input protection,
  DiskMap duplicate cleanup evidence, EnvLens runtime/PEP evaluation and
  AbiLens conservative compatibility.
- Extend existing products with versioned evidence/session/snapshot contracts,
  asynchronous desktop workflows and richer inspection controls.
- Gate all eight products independently; package installed layouts, connect
  release creation directly to verified draft assets, and repair Pages links.

Detailed behavior and release history live in each product's README and CHANGELOG.
The expansion was published as 0.2.1 for the five existing products and 0.1.0 for
TraceLens, TestLens and ServiceLens. The planned 0.2.0 tags were not published.

### 2026-10-03 — final review corrections

- Preserve EnvLens runtime input files and bounded pipe cleanup; incomplete
  snapshot collection remains unknown in check/diff.
- Resolve BuildScope vendor include roots correctly and auto-open saved diffs.
- Save/search with LogLens's applied filter and align AbiLens report I/O budgets.
- Validate ServiceLens scalar values and CTest observation completeness.
- Reconcile all product documentation, historical notes, examples and release
  links; add a required documentation consistency gate.

Product versions are owned by their package/build metadata. A local build or a
change recorded here does not itself publish a release.
