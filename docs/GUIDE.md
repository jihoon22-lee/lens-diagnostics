# Lens 사용 가이드

단일 바이너리 `lens`가 9개 진단 도메인(disk, abi, log, test, trace, sys, build,
env, net)과 포렌식 번들(`bundle`), 종합 점검(`doctor`), TUI 대시보드를 제공합니다.
모든 JSON 출력은 `to_deterministic_pretty`를 통해 결정적으로 직렬화되며,
각 스키마는 `*.schema` 식별자로 버전이 명시됩니다.

## 빌드 및 설치

```bash
cargo build --release -p lens-cli
# target/release/lens

# 셸 자동완성
source <(lens completion bash)   # zsh, fish도 지원
```

## 공통 계약

### 종료 코드

| 코드 | 의미 | 예시 |
|------|------|------|
| 0 | clean — findings 없음 | 정상 스캔, 회귀 없는 `test diff` |
| 1 | findings 있음 | `test diff` 회귀/신규 실패, `test parse` 불완전 JUnit, `abi diff` incompatible·uncertain(수동 검토 필요), `sys inspect`/`sys cycles` 사이클, `sys diff` 신규 사이클, `env inspect`/`env check` 누락·충돌 의존성, `net diff` 신규 리스너, `trace analyze` fd 누수, `trace diff` 신규 에러, `disk scan`/`disk duplicates` incomplete(권한 오류·절단·해시 실패), `bundle create`/`snapshot` 수집 경고, `bundle verify` 무결성 불일치, `bundle diff` 섹션 findings·섹션 역직렬화 실패, `doctor`의 `--fail-on` 기준 이상 |
| 2 | 사용·입력·런타임 오류 | 존재하지 않는 경로, 잘못된 플래그 값, 파싱 불가 입력, `abi inspect`/`abi diff`의 비ELF 입력, diff 입력의 스키마 불일치(`expected schema`), `bundle diff`의 무결성 검증 실패 번들 |

판정은 공유 레이어(`lens-ops`)의 `Report<T>`에서 나온다: 각 오퍼레이션이
`findings`(Info/Warn/Fail)과 `complete`를 채우고, CLI는
`is_actionable(threshold)`만으로 exit 1을 결정한다. 불완전 증거
(`complete=false`)는 findings가 없어도 항상 exit 1이다. 임계값은
`--fail-on warn|fail`(기본 `fail`)로 조절하며 **모든 명령**에 적용된다 —
`--fail-on warn`이면 Warn findings만 있어도 exit 1이다.

text 모드에서는 도메인 출력 뒤에 findings 요약 블록이 붙는다:

```text
Findings: 1 fail, 0 warn
  [FAIL] net.new_listener: new listener 0.0.0.0:4444
```

JSON 출력은 도메인 스키마를 그대로 유지하며 `findings` 키가 섞이지
않는다(`doctor`는 자체 checks 렌더링이 findings를 대신한다).

### 공통 옵션

다음 플래그는 전역 옵션이다 — 서브커맨드 앞이나 뒤 어디에 두어도
동일하게 동작한다(`lens --format json net diff a b` ≡
`lens net diff a b --format json`).

- `--format text|json|jsonl`: 출력 형식. `text`는 사람이 읽는
  요약(색인·집계·top-N)이고, `json`은 기존 결정적 JSON 스키마를
  출력한다. 기본값은 명령별 기존 동작을 유지한다(예: `abi inspect`는
  json, `disk scan`은 text). `jsonl`은 `log filter`만 실제 줄 단위
  출력에 사용하고, 다른 명령은 `json`과 동일하게 처리한다.
- `--json`: `--format json`의 별칭.
- `--fail-on warn|fail`(기본 `fail`): exit 1로 간주할 최소 finding
  심각도. 모든 명령에 적용된다.
- `-q`/`--quiet`: stderr의 비오류 진단(`scan warning`, `detected:`,
  필터 통계 등)만 끈다. 실제 오류와 `error:` 줄, stdout은 영향 없음.

### 출력 형식

`--format`/`--json`은 위 공통 옵션으로 통일됐다. 기존처럼
서브커맨드 뒤에 쓰는 형태(`lens disk scan . --json`,
`lens doctor --fail-on warn`)도 그대로 유효하다.

### 자동 판별: `lens inspect` / `lens diff`

경로를 주면 입력 종류를 감지해 해당 도메인 명령과 **동일한** 출력·종료
코드를 낸다(내부적으로 감지 결과를 해당 명령으로 디스패치). 감지 결과는
stderr에 `detected: <domain> (<kind>)`로 표시된다(`-q`로 억제).
`--as <kind>`로 감지를 건너뛰고 도메인을 강제할 수 있다.

```bash
lens inspect <path> [--as abi|test|trace|build|sys|env|net|log|disk|bundle]
lens diff <a> <b> [--as ...]
```

판별 규칙(파일은 처음 64 KiB만 읽는다. 단, `compile_commands.json`으로
이름이 지정되지 않은 JSON 배열은 첫 원소를 스트리밍으로 파싱하므로
64 KiB를 넘는 compile DB도 인식된다):

| 입력 | 판별 | inspect 매핑 | diff 매핑 |
|------|------|--------------|-----------|
| `\x7fELF` 매직 | Elf | `abi inspect` | `abi diff` (Elf+Elf) |
| JUnit XML(`<testsuite`/`<testsuites` 루트) | Junit | `test parse` | `test diff` (Junit+Junit) |
| strace 텍스트(처음 ≤200줄이 파싱됨) | Strace | `trace analyze` | `trace diff` (Strace+Strace) |
| JSON 배열 + `compile_commands.json` 이름 또는 첫 원소에 `directory`+`file` | CompileDb | `build inspect` | `build diff` (CompileDb+CompileDb) |
| `pyvenv.cfg`가 있는 디렉터리 | Venv | `env inspect` | `env diff` (Venv/EnvSnapshot 조합) |
| `.service`/`.socket`/`.target`/`.timer`/`.mount`/`.path`/`.slice` 단위 확장자 | SystemdDir(디렉터리)/SystemdUnit(파일) | `sys inspect` | `sys diff` (Dir/Unit/SysSnapshot 조합) |
| `.lens` 또는 gzip 매직 + 유효한 번들 매니페스트 | Bundle | `bundle inspect` | 미지원(usage 오류) |
| 스냅샷 JSON(`schema`/`schema_version` 매칭) | EnvSnapshot/SysSnapshot/NetReport | 오류 — "already a snapshot; use `lens diff`" | `env diff`/`sys diff`/`net diff` (같은 스키마끼리) |
| 그 외 텍스트/`.gz`, JSONL·연결된 객체·스키마 미상 JSON 등 구조화돼 보이지만 인식되지 않는 입력 | Log | `log inspect` | — |
| 그 외 디렉터리 | Directory | `disk scan` | — |

호환되지 않는 쌍은 usage 오류다(`cannot diff <a> with <b>; pass --as`,
exit 2). 스키마를 인식하지 못한 JSON 입력(한 줄 JSONL 포함)은 오류가
아니라 Log로 판별된다.

`lens diff`와 `lens env diff`는 venv 디렉터리와 env 스냅샷 JSON을
자유롭게 섞어 비교할 수 있다(디렉터리는 그 자리에서 inspect된다).

## 도메인별 사용법

### 1. disk — 파일시스템 분석

```bash
lens disk scan <path> [--json|--format json] [--parallel]
               [--max-depth N] [--exclude PATTERN]... [-x|--one-file-system]
               [--top N]
lens disk duplicates <path> [--min-size BYTES] [--format text|json]
               [--exclude PATTERN]... [-x|--one-file-system] [--max-depth N]
lens disk trash <path>... [--dry-run] [--format text|json]
lens disk trash list [--trash-dir DIR] [--format text|json]
lens disk trash restore <name> [--trash-dir DIR]
```

- `scan`: 아레나 트리로 계층 구조 + 논리/할당 크기 집계. `--json`은
  `diskmap.snapshot/v2` 출력. 심볼링크 루프 감지, per-entry 오류는
  stderr로 나옵니다. 하드링크(`hard_link_count > 1`)로 이어진 동일
  inode는 디렉터리 집계에서 한 번만 계산된다 — 파일 노드 자체는 각자
  크기를 유지한다.
- `--max-depth`: 사용자가 명시한 깊이 제한은 의도된 절단이지 증거
  결손이 아니다 — `complete`는 true로 유지되고 exit 0이며, 텍스트
  출력에 `Depth limit N: M directories not descended`로 표시된다
  (기본 안전 상한 512에 걸린 경우는 여전히 `complete=false`, exit 1).
- `--exclude`(반복 가능)는 세 가지 형태를 지원한다: `/` 포함 패턴은
  스캔 루트 기준 **상대 경로**에 대한 glob(`a/b`), `*`/`?` 포함은
  **이름**에 대한 glob(`*.log`), 그 외는 이름 **정확히** 일치 — 따라서
  `.git`은 `.git`만 제외하고 `.github`/`.gitignore`는 남는다.
- `-x`는 마운트 경계를 넘지 않는다(WSL의 `/mnt/c`, `/proc` 등
  제외에 유용).
- `--top N`(기본 10, 0은 비활성): 텍스트 출력에 최상위에서 가장 큰
  항목 N개를 KiB/MiB/GiB 단위로 표시.
- `--parallel`: rayon으로 per-entry stat 병렬화. cold cache의 대형 트리에서
  유효하고, warm cache에서는 이득이 거의 없다.
- `duplicates`: 부분 해시 → 전체 SHA-256 2단계 판정. 하드링크(inode 공유)는
  회수 가능 용량에서 제외, `(dev=0,ino=0)` 파일은 경로 기반 폴백.
  해시 실패는 `errors`로 집계해 출력하고, 총 회수 가능 용량 요약을 표시.
  `--format json`은 `{groups, errors, total_reclaimable_bytes}`를 출력.
  `--exclude`/`-x`/`--max-depth`로 `scan`과 같은 스캔 범위 제한을 지원.
- `trash`: FreeDesktop Trash 규격으로 이동(`.trashinfo` 기록 포함,
  심볼링크는 타깃이 아닌 링크 자체가 이동됨, 복구 영수증 출력).
  다중 경로를 받고, `--dry-run`은 이동 없이 예정 동작만 출력한다.
  파일이 다른 파일시스템에 있으면(EXDEV) 그 마운트의 `$topdir/
  .Trash-$uid`(mode 0700)로 폴백한다. `trash list`는
  `.trashinfo` 기준으로 항목을 나열하고, `trash restore <name>`은
  저장된 identity(파일 크기·inode·mtime)를 검증한 뒤 원래 위치로
  복원한다 — 대상이 이미 존재하거나 trashed 파일의 identity가
  바뀌었으면 거부한다. topdir trash 항목은 `--trash-dir`로 지정한다.

### 2. abi — ELF 바이너리 검사

```bash
lens abi inspect <binary> [--format text|json]
lens abi diff <baseline> <candidate> [--format text|json]
```

- 동적 심볼 표면(정의/미정의 구분), DT_NEEDED, RPATH/RUNPATH, SONAME,
  인터프리터, 심볼 버전 요구사항(VERNEED/VERDEF), `.debug_info` 타입명 수집.
- 스트립된 바이너리도 PT_DYNAMIC/PT_INTERP program header 폴백으로 파싱.
- 비ELF 입력은 CLI에서 fail-closed: `error: <path> is not an ELF file`
  + 종료 2 (리포트 없음). 라이브러리/MCP는 계속 `status: "non-elf"`
  리포트를 반환한다.
- `diff`는 `abilens.diff/v3`: 정의된(exported) 심볼만 제거 판정에 반영,
  미정의 import는 `imports`에 의존성 정보로 별도 집계(import만 바뀌면
  compatible), weak 심볼은 binding=`weak`로 표기. 버전 요구사항 변경,
  `Compatibility`(compatible/incompatible/uncertain) 3상태 판정.

### 3. log — 로그 인덱싱·필터

```bash
lens log inspect <path>... [--format text|json]
lens log filter <path>... [--query TEXT | --regex PATTERN] [--min-level LEVEL]
                [--include-unknown] [--since TS] [--until TS] [--year Y]
                [--limit N] [--context N] [-f|--follow]
                [--format text|json|jsonl]
lens log stats <path>... [--bucket 1m|5m|15m|1h|1d]
                [--query TEXT | --regex PATTERN] [--min-level LEVEL]
                [--include-unknown] [--since TS] [--until TS] [--year Y]
```

- `log inspect`/`filter`/`stats`의 `path`는 여러 개 지정할 수 있고
  각 항목은 `-`(stdin), 파일, `.gz`, `*`/`?` 파일명 glob(부모
  디렉터리에서 확장, 정렬)이다. glob이 아무것도 못 찾으면 사용 오류,
  `-`는 한 번만 허용된다.
- 다중 입력 시 `--limit`은 전체 합산이고 컨텍스트는 파일 경계를
  넘지 않는다. text 출력은 `[<file>:<line>]` 접두사, jsonl 오브젝트는
  `"file"` 필드를 가진다. `--format json`은
  `{"inputs": [파일별 오브젝트…], "matched", "total"}`이다(단일
  입력은 기존 형태 그대로). `log inspect`는 파일별 요약 블록을
  출력하고 json은 배열이 된다.
- `-f`/`--follow`: 초기 필터 패스 뒤 250 ms 간격으로 파일을 폴링해
  추가된 줄에 같은 필터를 적용한다(`tail -f`와 같음 — 줄 번호는
  인덱스 총계에서 이어진다). 정확히 하나의 정규 파일만 허용
  (stdin·`.gz` 불가), `--context 0`, `--format text` 또는 `jsonl`
  필요. 파일이 잘리거나(truncate) 교체되면(rotate) 처음부터 다시
  읽고 stderr에 안내를 출력한다.
- `log stats`는 필터가 통과시킨 줄의 레벨 분포와 시간 히스토그램을
  출력한다(`lens.log-stats/v1`). `--bucket` 미지정 시 첫·끝
  타임스탬프 구간이 60 버킷 이하가 되는 최소 폭을 자동 선택
  (타임스탬프 없으면 1h). text 출력의 버킷 뒤 `!N`은 그 버킷의
  warn+ 줄 수다.

- mmap 라인 인덱서(memchr 기반)로 GB급 로그를 즉시 열람.
- `path`로 `-`를 주면 stdin을 읽는다. `.gz` 파일은 해제하며 읽고,
  두 경우 모두 해제된 바이트를 무익명 임시 파일에 스풀한 뒤 mmap한다
  (프로세스 힙에 남지 않는다). 해제 상한 512 MiB 초과 시 오류로
  거부한다 — 로테이트된 로그를 바로 조사 가능.
- invalid UTF-8 줄은 건너뛰지 않고 U+FFFD로 표시하며, `lossy_lines`
  카운트를 JSON 필드와 stderr로 보고한다.
- `--min-level`: trace|debug|info|warn|error|fatal (오타 시 즉시 거부).
  레벨을 판별할 수 없는 줄은 제외되고 stderr에 제외 수를 보고한다.
  `--include-unknown`으로 복원 가능.
- `--since TS` / `--until TS`: 타임스탬프 창 필터(양끝 포함). 허용
  형식은 RFC 3339(`2026-10-09T12:00:00Z`, 오프셋 포함), `YYYY-MM-DD
  HH:MM:SS`, `YYYY-MM-DD`(오프셋 없는 형식은 UTC 해석). 잘못된 입력은
  종료 2. 인식하는 줄 타임스탬프는 ISO/RFC3339 접두사, syslog
  `Oct  9 12:00:00` 접두사, JSONL의 `ts`/`time`/`timestamp` 필드
  (숫자는 epoch ms, 문자열은 위 형식). 연도 없는 syslog 타임스탬프는
  현재 UTC 연도를 가정하며 `--year`로 지정 가능. 타임스탬프를 판별할
  수 없는 줄은 제외되고 stderr에 제외 수를 보고한다 —
  `--include-unknown`으로 복원 가능.
- `--regex`는 substring `--query` 대신 regex 매칭을 쓴다(둘은
  상호배타). `--limit N`은 매치 N개에서 중단하고, `--context N`은
  매치 전후 N줄을 함께 출력(컨텍스트 줄은 `[N]-`/`"context": true`).
  `--format jsonl`은 출력 줄당 JSON 오브젝트 하나로 낸다.
- 레벨 감지는 텍스트 레벨 + syslog PRI(`<33>`) + 커널 printk(`<3>`)
  접두사까지 인식. 메시지 추출은 무할당 슬라이딩 윈도우로 수행.

### 4. test — JUnit 리포트 파싱·회귀 diff

```bash
lens test parse <file|dir|glob> [--project NAME] [--format text|json]
lens test diff <baseline> <candidate> [--format text|json]
```

- 입력은 파일 1개, `*.xml` 디렉터리, 또는 `*`/`?` 파일명 glob — CI의
  모듈별 JUnit 파일들을 하나의 run으로 병합해 비교한다.
- `testlens.run/v1`: 테스트케이스 신원(`suite::classname::name`), 상태
  (passed/failed/error/skipped), 실패 본문, `system-out`/`system-err`,
  `<properties>`, suite 출력까지 수집.
- 잘린/깨진 XML은 가능한 범위까지 파싱하고 `complete=false`로 표시.
- `run_id`는 내용 기반 결정적 해시(재실행해도 동일).
- `diff`는 `testlens.diff/v2`: 구조화된 `regressions[]{id,before,after,
  message}`, `fixes`, `new_failures`(새로 등장한 실패/에러 — exit 1),
  `removed_tests`, `skipped_changes`. skipped→passed는 fix가 아니라
  skip 변경으로 분류. 중복 identity는 `diagnostics`로 보고.

### 5. trace — strace 분석

```bash
lens trace analyze <trace_file> [--format text|json]
lens trace diff <baseline> <candidate> [--format text|json]
```

- `tracelens.snapshot/v1`: syscall 집계, 에러, 지연, IO 처리량,
  프로세스별 fd 테이블 + `fd_leaks_by_process`.
- `+++ exited/killed/superseded` 계열 종결 처리, `strace -y` 주석
  (`3</etc/hosts>`) 무시, fork/clone 시 fd 상속, tid 재사용 generation 분리.
  `CLONE_FILES` 스레드는 fd 테이블을 공유, `O_CLOEXEC` fd는 `execve`에서
  해제, `close_range` 일괄 해제 처리. 에러는 `syscall:errno` 키로 집계.
- `diff`는 `tracelens.diff/v1`: 지연 변화와 신규 에러.

### 6. sys — systemd 정적 분석

```bash
lens sys inspect [path]          # 유닛 파일/디렉터리; 생략 시 시스템 검색 경로
lens sys cycles [dir] [--format text|json]
lens sys diff <baseline> <candidate>   # 스냅샷 JSON 또는 유닛 디렉터리
```

- `.d/` drop-in 병합(인스턴스 유닛은 템플릿 `foo@.service.d/`도 병합),
  `Key=` 빈 값 리셋 의미론, `%u`/`%h` 등 specifier 확장.
- 전 suffix 지원(.service/.target/.socket/.timer/.mount/...).
- Tarjan SCC로 순환 의존 탐지. 사이클은 실제 방향 경로로 보고하고 각
  엣지의 기원(`--Wants(a.service:12)-->`)을 표시. `/dev/null` masked
  유닛은 엣지 대상에서 제외, alias/템플릿 이름도 해석.
- `diff`는 섹션의 모든 키를 비교(`User=` 추가 등). `servicelens.snapshot|
  diff/v1` 출력.
- `inspect`/`cycles`의 경로를 생략하면 systemd 검색 경로를 우선순위
  병합해 로드한다(`/etc` → `/run` → `/usr/lib` → `/lib`, 상위가 하위를
  가리고 drop-in은 전 디렉터리에서 수집). 같은 병합 로더를 doctor와
  TUI Services 탭도 공유한다. 명시한 경로가 없으면 오류(exit 2).

### 7. build — 컴파일 데이터베이스 분석

```bash
lens build inspect <compile_commands.json> [--format text|json]
lens build impact <compile_commands.json> --header <path> [--format text|json]
lens build diff <baseline.json> <candidate.json> [--format text|json]
```

- `inspect`는 `buildscope.snapshot/v4`: 파싱된 유닛 + 역방향 impact 그래프.
  `reverse_impact`는 직접 includer만, `transitive_impact`는 헤더 체인을
  거친 간접 includer까지 포함.
- `-I`/`-isystem`/`-iquote`/`-idirafter` 순서 보존(첫 일치 우선),
  `-include`/`-imacros` 강제 인클루드, 주석 내 `#include` 무시,
  `..`/`./` 경로 정규화.
- `impact`는 헤더의 전이 의존자(재컴파일 대상)를 계산. 상대 `--header`
  는 compile database 디렉터리와 각 엔트리의 `directory` 기준으로 해석돼
  어느 cwd에서도 동작한다. 그래프에 없는 헤더는 basename 유사 후보와
  경로 전달 방법을 `hint`로 보고(warning, findings 아님). 리포트에
  `missing_sources`/`unresolved_includes` 카운트를 포함하고 온디스크
  스캔 상한 도달 시 `scan_truncated=true`. 파일별 include 추출은
  캐시돼 대형 프로젝트의 중복 디스크 읽기를 줄인다.

### 8. env — Python 가상환경 감사

```bash
lens env inspect <venv_path> [--project <dir>] [--format text|json]
lens env check <venv_path> [--extras a,b] [--format text|json]
lens env diff <baseline> <candidate> [--format text|json]
               # baseline/candidate: 스냅샷 JSON 또는 venv 디렉터리(혼합 가능)
```

- 인터프리터 실행 없이 정적 메타데이터 감사(dist-info/METADATA).
- `check`는 PEP 508 환경 마커(`python_version`, `sys_platform`,
  `extra`, `and`/`or`/괄호, 버전 비교)를 venv의 실제 Python 버전/
  플랫폼에 대해 평가한다. 거짓 마커의 요구는 스킵, `extra == "x"`는
  `--extras`로 활성화한 extra에서만 적용, 설치됐지만 범위를 벗어난
  버전은 `version_conflicts`로 보고, 평가 불가 마커는 `unevaluated`로
  missing과 구분해 보고. uv venv의 `version_info` 키도 인식.
- `--project`: 로컬 소스가 site-packages 모듈을 가리는 import 섀도잉 탐지.

### 9. net — 소켓/포트 포렌식

```bash
lens net inspect [--proc-dir PATH] [--json|--format json] [--no-unix]
lens net diff <baseline.json> <candidate.json> [--format text|json]
```

- `/proc/net/{tcp,tcp6,udp,udp6,unix}` 파싱 + `/proc/*/fd` inode→PID 상관.
- UDP 바인드 소켓도 리스너로 집계. unix 소켓은 Type/St 의미론 분리.
  `--no-unix`는 unix 소켓을 출력에서 제외.
- 소유자 없는 소켓은 `owner_state`로 구분: `orphan`(inode를 검사했으나
  보유 프로세스 없음) vs `owner_unknown`(`/proc/<pid>/fd` 접근 거부로
  확인 불가 — 텍스트에서 `<owner unknown>`으로 표시, sudo 권장).
  summary에 `owner_unknown_sockets`/`uninspectable_processes` 카운터.
- `lens.net/v1` 스키마. `diff`는 (kind, address, port)/5-tuple 매칭으로
  바인드 주소 변경도 closed+new로 표면화.

## 포렌식 번들 (`bundle`)

```bash
lens bundle create incident.lens \
  --disk /var/log --trace strace.log --test junit.xml \
  --sys /etc/systemd/system --env .venv --net --log app.log
lens bundle inspect incident.lens
lens bundle verify incident.lens
lens bundle show incident.lens reports/log_summary.json
lens bundle extract incident.lens out/ [--force]
```

- `tar.gz` + `manifest.json`(`lens.bundle/v2`): 아티팩트별 SHA-256, 크기,
  수집 진단(diagnostics) 보존. 최소 1개 소스 필수.
- `--log`는 요약 JSON 외에 원본 로그 내용을 `logs/<name>` 엔트리로
  포함한다(8 MiB 꼬리 상한, 절단 시 diagnostics에 기록).
- `verify`: 정확 경로의 매니페스트만 신뢰(중첩 스푸핑 차단), 중복/미등재
  엔트리 거부, 해제 바이트 상한 적용. 매니페스트는 서명되지 않으므로
  **무결성 확인이지 출처 인증이 아닙니다**. 무결성 불일치는 findings로
  exit 1(읽기 불가/손상 파일은 exit 2).
- `show`는 `tar` 없이 개별 엔트리 내용을 출력. `extract`는 검증을
  먼저 통과한 번들만 풀고, `..`/절대/역슬래시 엔트리를 거부하며
  `--force` 없이는 기존 파일을 덮어쓰지 않는다.
- 생성은 임시 파일 + 원자적 rename(부분 쓰기·심볼링크 덮어쓰기 방지).
- `bundle diff <a.lens> <b.lens>`는 두 번들을 먼저 무결성 검증하고
  (실패 시 exit 2) 양쪽에 모두 있는 섹션(`sys/test/trace/env/net/
  doctor/disk/log`)을 도메인 diff와 동일한 분류기로 비교한다 — findings
  코드(`sys.new_cycle`, `test.regression`, …)도 동일하다. 한쪽에만
  있는 엔트리는 `bundle.only_in_baseline`/`only_in_candidate` Info다.
  `lens diff a.lens b.lens`도 같은 경로로 디스패치된다. JSON 출력은
  `lens.bundle-diff/v1`이다.

## 기준선 워크플로 (`snapshot` + `lens diff`)

```bash
lens snapshot                          # snapshot-<host>-<UTC>.lens 생성
lens snapshot -o before.lens
# ... 변경/배포/사고 ...
lens snapshot -o after.lens
lens diff before.lens after.lens       # 섹션별 자동 비교
```

- `lens snapshot`은 사고 대응용 기준선 번들이다: 항상 `reports/
  host.json`(호스트명·커널·OS·수집 시각), `reports/net_report.json`,
  `reports/sys_snapshot.json`(`--systemd-dir` 또는 병합 검색 경로),
  `reports/doctor_report.json`을 수집하고, 로그는 `--log` 또는
  `/var/log`의 첫 표준 syslog를 임베드한다. `--disk`/`--env`/`--test`/
  `--trace`로 소스를 추가하고 `--procfs`로 근거 루트를 바꾼다.
- 기본 출력명은 `snapshot-<hostname>-<UTC YYYYMMDDTHHMMSSZ>.lens`.
- 수집 진단이 있으면 번들은 만들어지지만 exit 1이다. doctor 자체의
  Warn/Fail 판정은 스냅샷 종료 코드에 영향을 주지 않는다(번들 안의
  데이터일 뿐이다). `bundle create --doctor`로 일반 번들에도 doctor
  리포트를 넣을 수 있다.

## 시스템 종합 점검 (`doctor`)

```bash
lens doctor [--root PATH] [--procfs PATH] [--systemd-dir PATH]
            [--json|--format json]
```

스토리지 용량(statvfs), 네트워크, 서비스, 환경 검사를 PASS/WARN/FAIL로
집계하고 권장 조치를 출력합니다. `--fail-on`(기본 `fail`) 미만의
심각도는 exit 0을 유지한다 — 권한 부족으로 검사 불가한 소켓은 orphan
WARN이 아니라 PASS + "sudo로 재실행" 권고로 보고한다. WARN/FAIL
검사에는 다음 조치 명령이 `-> Next:`로 인쇄된다.

- 스토리지: `--root` 없이 실행하면 `/proc/self/mounts`(또는 `--procfs`
  지정분)의 실파일시스템마다 용량 검사를 추가한다(가상 fs·snap 패키지
  마운트 제외, `f_fsid` 중복 제거, `/`는 `Root Disk Capacity`, 나머지는
  `Disk Capacity <mountpoint>`). 각 검사는 inode 여유율도 평가한다 —
  여유 inode <5%는 Fail, <15%는 Warn이고 최종 상태는 용량과 중 나쁜 쪽.
- 네트워크: wildcard 민감 리스너는 `-> Next: lens net inspect`, orphan
  소켓은 `-> Next: lens net inspect --format json`.
- 서비스: 사이클은 `-> Next: lens sys cycles`; 유닛 파싱 진단이
  있으면 `Systemd Unit Parsing` WARN 검사를 추가하고 `-> Next: lens
  sys inspect --format json`.
- PATH: `.`/빈 항목 외에 **world-writable 디렉터리**도 WARN이다.
- ld.so.preload에 항목이 있으면 `-> Next: lens abi inspect <첫 라이브러리>`.

`--root PATH`는 검사를 루트 이미지로 스코프한다: 네트워크는 `<root>/proc`,
서비스는 `<root>` 아래로 재배치된 systemd 검색 디렉터리(`/etc/systemd/system`
등), ld.so.preload는 `<root>/etc/ld.so.preload`를 읽는다. 루트에서 확인할
수 없는 검사(네트워크, 서비스, 호스트 프로세스의 PATH)는 호스트 데이터로
채우지 않고 `SKIP`으로 보고한다. `SKIP`은 findings에 포함되지 않아
exit code에도 영향이 없다. `--procfs`/`--systemd-dir`을 명시하면 그
경로가 우선한다.

## TUI 대시보드

```bash
lens tui [path] [--log FILE]
```

4개 탭(Storage/Services/Logs/Network) 모두 실데이터. 디렉터리 스캔은
백그라운드 스레드에서 돌고 UI는 즉시 뜬다 — 스캔 중에는 스피너가 표시된다.
한 번 스캔한 트리 안에서는 Enter/Backspace가 메모리 탐색만 한다(재스캔 없음).
`--log FILE`로 Logs 탭의 로그를 지정할 수 있다. 단축키:
`Tab`/`1-4` 탭 전환, `j/k` 이동, `PgUp/PgDn` 페이지, `g/G` 처음/끝,
`Enter` 디렉터리 진입, `Backspace`/`h` 상위, `/` 로그 검색,
`?` 도움말, `r` 리로드, `q`/`Esc` 종료.

## MCP 서버 (`lens-mcp`)

AI 어시스턴트 연동용 stdio JSON-RPC 서버. 도구: `lens_disk_scan`,
`lens_disk_duplicates`, `lens_abi_inspect`, `lens_abi_diff`,
`lens_log_filter`, `lens_trace_analyze`, `lens_sys_cycles`,
`lens_sys_diff`, `lens_test_diff`, `lens_net_diff`, `lens_build_impact`,
`lens_env_check`, `lens_net_inspect`, `lens_bundle_verify`,
`lens_bundle_diff`, `lens_log_stats`, `lens_doctor`.

```bash
cargo build --release -p lens-mcp
# Claude Desktop 등에서 command로 target/release/lens-mcp 등록
```

- 모든 도구의 상대 경로는 lens-mcp 프로세스의 cwd 기준으로 해석된다
  (`lens_build_impact`의 `header`는 컴파일 DB 디렉터리/entry `directory` 기준).
- 응답 크기 제어: 모든 도구가 `limit`(배열당 최대, 기본 200),
  `page_path`(오프셋을 적용할 배열의 점 경로 — 예: `abi.imports`,
  배열 원소 내부는 `units[].diagnostics`처럼 `[]`로 지정), `offset`을
  받는다. `offset`은 `page_path`가 가리키는 배열에만 적용된다. 잘린
  배열은 데이터에 마커를 넣지 않고 최상위 `_pagination` 객체로 보고된다:
  `{"_pagination": {"<path>": {"total": N, "returned": k, "offset": o}}}`.
  `[]` 경로는 모든 발생의 합계를 낸다. 64KiB를 넘는 응답은 limit 25로
  재시도되고 `_response_clamped: true`가 표시된다.
- `lens_log_filter`는 `tail`로 파일 끝 N라인만 스캔할 수 있다(최근 라인 조사).
  `limit`/`offset`으로 매치를 페이징하며 `truncated`로 표시된다
  (이 결과의 `matches`는 자체 페이징되므로 `page_path` 오프셋이
  중복 적용되지 않는다).
- 도구 결과의 첫 content 아이템은 데이터 JSON이다. findings 또는
  불완전 증거가 있으면 두 번째 아이템으로
  `{"findings": [...], "complete": bool}` JSON이 추가된다.
- 도구 실패는 `result.isError=true` + 텍스트로 반환된다(잘못된 경로 포함).
- `lens-mcp --allow-root DIR`(반복 가능)으로 파일시스템 접근을
  샌드박스한다: 모든 도구 경로 인자(`path`·`baseline`·`proc_dir`·
  `root_path`·`systemd_dir` 등)가 허용 루트 아래로 canonicalize되지
  않으면 in-band 도구 오류 `path <p> is outside the allowed roots`로
  거부된다. 존재하지 않는 경로는 가장 가까운 존재 조상을 기준으로
  판정한다. 미지정 시 기존 동작(제한 없음).
- `prompts` capability: `prompts/list`/`prompts/get`로 두 프롬프트를
  제공한다 — `incident-triage`(선택 인자 `log_path`: doctor →
  net inspect → log filter `min_level:"warn"`, `tail:2000` 순서 지시)와
  `compare-snapshots`(필수 인자 `baseline`/`candidate`:
  `lens_bundle_diff` 호출 후 회귀부터 설명). 알 수 없는 프롬프트 또는
  필수 인자 누락은 -32602다.
- `initialize`는 지원하는 `protocolVersion`(`2025-06-18`, `2025-03-26`,
  `2024-11-05`) 중 요청된 것을 에코하고, 미지원/누락이면 `2025-06-18`로
  응답한다. `id` 없는 요청(JSON-RPC 노티피케이션)에는 응답하지 않는다.

## 출력 계약 (공통)

- **결정적**: 같은 입력 파일/시스템 상태 → 같은 바이트 출력(BTreeMap
  기반 정렬 직렬화). 단, 스냅샷성 타임스탬프는 예외 — `bundle create`의
  매니페스트 `created_at`은 생성 시각을 기록하므로 번들 바이트는 실행마다
  다릅니다. 내용(아티팩트·해시)은 동일합니다.
- **Fail-closed**: 수집 실패는 조용히 삼키지 않고 `complete=false`,
  `errors`, `diagnostics`, `scan_truncated` 등의 필드로 표면화.
- **스키마 버전**: 모든 스냅샷/리포트에 `schema` 문자열 필드.
- **버전 정합**: 런타임 버전은 `env!("CARGO_PKG_VERSION")`에서만 취함.

### MCP evidence and response bounds

MCP findings metadata also includes `diagnostics`, `diagnostics_total` and
`diagnostics_truncated`; collection failures remain visible even with no findings.
The byte budget includes escaped JSON text for both content items. If pagination
cannot fit an individual value, the data item contains `_data_omitted: true` and
`_response_clamped: true`; use narrower inputs to retrieve the evidence. Findings
and diagnostics retain totals and explicit truncation flags when entries are omitted.
`lens_log_stats` supports `include_unknown` just like `lens_log_filter`.

Allowed roots apply to discovered evidence files, including systemd drop-ins,
JUnit directory/glob entries and secondary environment/network/build reads.
This is an application read boundary, not an operating-system sandbox.

Bundle manifests have a 1 MiB metadata limit, checked before buffering or
parsing. Bundle diff bounds imported collection diagnostics to 128 messages,
64 KiB in total and 4 KiB per message, with explicit truncation notices;
the report remains incomplete when collection evidence is missing.
