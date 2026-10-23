# Lens 아키텍처

이 문서는 `lens-*` 워크스페이스의 구조, 책임 분리, 데이터 흐름, 그리고 코드
전체를 관통하는 설계 불변조건을 설명합니다.

## 1. 워크스페이스와 의존 방향

14개 크레이트. 의존 방향은 프레젠테이션 → 오퍼레이션 → 도메인 → 코어
단방향입니다.

```
lens-cli ─┐
          ├─> lens-ops ──> lens-disk  lens-abi  lens-log  lens-test
lens-mcp ─┘                  lens-trace lens-sys lens-env lens-net
                             lens-build ──> lens-core

lens-tui ──> (도메인 크레이트 직접 소비) ──> lens-core
```

- **lens-core**: 공유 기반. `LensError`/`Result`, `SafeInput`(TOCTOU 방어),
  SHA-256 유틸, `Compatibility` 3상태 어휘, `to_deterministic_pretty`,
  시간 유틸(`utc_now_iso`/`local_now_iso`), 번들 포맷(create/inspect/
  verify/show/extract — `extract_bundle`은 검증을 먼저 통과해야 하고
  엔트리 이름/덮어쓰기를 fail-closed로 거부).
- **도메인 크레이트** (`lens-disk`, `lens-abi`, `lens-log`, `lens-test`,
  `lens-trace`, `lens-sys`, `lens-build`, `lens-env`, `lens-net`):
  각각 수집·파싱·스냅샷·diff를 담당. 서로 의존하지 않아 독립 테스트 가능.
- **lens-ops**: 공유 오퍼레이션 레이어. 수집 + 판정을 `Report<T>`
  (`findings`, `complete`, `diagnostics`)로 반환하고, 프레젠테이션은
  일절 하지 않는다(clap 없음, 출력 없음). 종합 점검(`doctor`)도 여기에
  있다. 종료 코드 판정의 단일 근거는 `Report::is_actionable(threshold)`.
- **lens-cli**: clap 정의(`cli.rs`)와 렌더러(`ops.rs`). `lens-ops`의
  `Report`를 text/JSON으로 출력하고 `is_actionable` 결과를 종료 코드로
  변환할 뿐 수집·판정 로직을 갖지 않는다.
- **lens-tui** / **lens-mcp**: 같은 오퍼레이션/도메인 API를 다른
  진입점으로 소비. 비즈니스 로직 복제 금지 — systemd 로딩·JUnit 입력
  확장·빌드 영향도 해석 등은 `lens-ops`의 단일 구현을 공유.

## 2. 설계 불변조건

코드 리뷰에서 이 원칙 위반이 곧 버그로 분류됩니다.

1. **Fail-closed / Evidence-bound**: 수집 실패·파싱 불능·경계 도달을
   조용히 삼키지 않는다. `complete=false`, `errors: Vec<String>`,
   `diagnostics`, `scan_truncated` 등으로 *보고서 안에* 남긴다.
2. **결정적 출력**: 사용자에게 보이는 구조는 정렬된 맵(BTreeMap/BTreeSet)으로
   직렬화. `HashMap` 반복 결과가 출력에 도달하면 안 된다.
3. **스키마 버전 명시**: 모든 스냅샷/리포트에 `schema` 문자열
   (`lens.bundle/v2`, `diskmap.snapshot/v2`, `abilens.report/v2`,
   `abilens.diff/v3`, `testlens.run/v1`, `testlens.diff/v2`, `tracelens.snapshot/v1`, `servicelens.snapshot/v1`,
   `envlens.snapshot/v1`, `lens.net/v1`, `buildscope.snapshot/v4` 등).
4. **단일 구현 단일 책임**: 같은 파일 포맷을 파싱하는 코드는 하나
   (예: 번들 create/inspect/verify는 모두 `lens_core::bundle` 한 구현).
5. **런타임 상수 금지**: 버전은 `env!("CARGO_PKG_VERSION")`, 시각은
   `lens_core::time`에서만 생성. 하드코딩된 타임스탬프/버전은 포렌식
   증거를 오염시키므로 금지.
6. **정직한 용어**: 서명 없는 SHA-256 매니페스트는 "무결성"이지
   "인증/진위(authentic)"가 아니다. 성능 수치는 실측값만 기재.

## 3. 도메인별 내부 구조

### lens-disk
- `scanner.rs`: `ArenaTree`(u32 인덱스 + Vec 풀, 캐시 지역성)로 계층 구축.
  `visited_dirs`는 *실제 디렉터리*의 dev/ino만 기록(심볼링크 별칭은 오염
  방지), `parallel` 옵션 시 rayon per-entry stat. 한계 도달 → `complete=false`.
- `duplicates.rs`: 크기 → 부분 해시 → 전체 해시 3단 판정, inode 그룹으로
  하드링크 중복 해시 방지, 결정적 2차 정렬.
- `trash.rs`: FreeDesktop Trash 규격(충돌 회피 파일명, `.trashinfo`).
  `.lens.json` identity 사이드카로 복원 시 교체 검증(fail-closed),
  `list`/`restore_by_name` 제공, EXDEV는 `$topdir/.Trash-$uid`(0700)
  폴백.

### lens-abi
- `elf.rs`: `object` 크레이트 기반. 섹션 헤더 없는 스트립 바이너리는
  program header(PT_DYNAMIC/PT_INTERP) 수동 파싱으로 폴백. DT_NEEDED/
  RPATH/RUNPATH/SONAME/VERNEED/VERDEF 해석, SHF_ALLOC 필터.
- `dwarf.rs`: gimli로 `.debug_info` 타입명 추출(상한 10k), 압축 섹션 해제.
- `diff.rs`: 심볼/버전 요구사항 SetDiff + `Compatibility` 판정. 정의된
  심볼만 제거 검사 대상이며 미정의 import는 `imports` SetDiff로 별도
  비교(import만의 변경은 compatible), weak 심볼은 `binding="weak"`.
  DWARF 타입 크기 변화는 공개 ABI 노출 관계를 입증하지 못하면
  `uncertain`으로 보고하며, 타입명과 심볼명의 부분일치를 근거로 삼지 않는다.

### lens-log
- `indexer.rs`: memmap2 라인 인덱스 + memchr 오프셋 계산. `.gz`와
  stdin 스트림은 해제 상한(512 MiB, 번들 해제 상한과 같은 규약) 아래
  메모리 소스로 전환. invalid UTF-8 줄은 버리지 않고 `lossy_lines`로
  집계·U+FFFD로 노출.
- `parser.rs`/`filter.rs`: 무할당 `contains_insensitive` 슬라이딩 윈도우,
  `LogLevel::parse`는 `eq_ignore_ascii_case`(할당 없음), 구조화 `fields`.
  syslog PRI/커널 printk 우선순위 토큰(`<N>`)에서도 레벨 추출. `--min-level`
  필터는 `Unknown` 레코드를 제외하고 제외 수를 보고(`include_unknown`
  으로 복원).

### lens-test
- `junit.rs`: quick-xml 스트리밍. 선언된 인코딩+엔티티 디코딩,
  `<properties>`·suite 출력·실패 본문 수집, malformed 종료 태그 관대
  처리, 열린 태그 잔존 시 `complete=false`. `run_id`는 내용 해시.

### lens-trace
- `parser.rs`: tid 프리픽스/타임스탬프 구분(숫자+`:`+`.`만),
  `+++` 종결행 전부 종료 처리, `-y` 주석(`fd</path>`) 제거,
  `BTreeMap<tid, BTreeMap<generation, fdset>>` — fork 시 fd 상속,
  `CLONE_FILES` clone은 fd 테이블 공유(한 스레드의 close가 다른
  스레드에도 적용), `O_CLOEXEC` fd는 `execve`에서 해제, `close_range`
  구간 해제 지원. 에러는 `syscall:errno` 키로 집계. exit 시 잔여 fd를
  `fd_leaks_by_process`로 귀속, tid 재사용은 새 세대.

### lens-sys
- `parser.rs`: 할당 순서 보존 파싱. `ExecStart=` 등의 빈 값은 리셋하지만
  `After=`/`Before=`/`Wants=`/`Requires=` 같은 의존성은 삭제하지 않는다.
  시스템 매니저의 `%u`/`%h`는 `User=`와 무관하게 `root`/`/root`이며,
  템플릿의 `%i` 등은 인스턴스 이름이 정해진 뒤 확장한다. 사용자 매니저는
  현재 지원 범위에 포함하지 않는다. `=` 없는 라인(`SYNTAX_GARBAGE_LINE`)과
  닫히지 않은 `[Section` 헤더(`SYNTAX_SECTION_HEADER`)를 진단으로 수집.
- `loader.rs`: 파일/디렉터리 로딩, `.d/` drop-in 스캔(템플릿 포함),
  후순위 override. `/dev/null` masked 유닛과 alias는 엣지 대상에서 정리.
  `load_units_merged`는 `SYSTEMD_SEARCH_DIRS`(/etc→/run→/usr/lib→/lib,
  높은 우선순위 순)를 병합 — 상위 디렉터리의 유닛 파일·mask·alias가
  하위를 가린다. drop-in은 같은 파일명끼리 우선순위를 적용한 뒤 전체를
  파일명순으로 병합한다. 합성 인스턴스에도 인스턴스 전용 drop-in을 적용하고,
  템플릿이 없는 drop-in 전용 유닛은 스텁으로 보존한다. doctor/TUI/`sys`의
  기본 경로가 이 단일 구현을 공유. 로드된 유닛·alias·mask·템플릿
  인스턴스·생성형 suffix에 매칭되지 않는 의존 대상은 `UNIT_REF_MISSING`
  진단이 되고 CLI가 스냅샷 상위 `diagnostics`로 집계한다.
- `resolve.rs`: 작업 큐로 참조된 템플릿 인스턴스를 한 번씩 확장한다.
  유닛 10,000개, 의존 참조 100,000개, 확장 깊이 32, 합성 이름 255바이트,
  확장 메타데이터 64 MiB를 상한으로 두며 초과 시 `LimitExceeded`를 반환한다.
  파일 로더와 `OrderingGraph::build`는 `Result`를 반환하고, 번들 수집은
  실패한 항목을 진단과 `complete=false`로 기록한다.
- `dag.rs`: Before/After로 순서 그래프 + Tarjan SCC.
  사이클은 SCC 멤버 정렬이 아니라 실제 방향 경로로 재구성하고 각 엣지의
  기원(유닛 파일:라인 + 디렉티브)을 함께 보고.
- `diff.rs`: 유닛의 모든 섹션·키 및 `.wants`/`.requires` 링크를 포함한
  유효 의존성을 비교한다. 지원 의미론은 `systemd-255-subset-v2`다.
  `sys diff`는 스냅샷 JSON 외에 유닛 디렉터리도 입력으로 받는다.

### lens-build
- `compiler.rs`: compile_commands.json 엔트리 파싱. `-I` 계열 순서 보존
  dedup, `-include`/`-imacros` 강제 인클루드, `-isysroot` 인자 소비,
  `normalize_path`로 `..`/`./` 정규화.
- `impact.rs`: 소스의 `#include`를 온디스크 해석(인클루딩 파일 디렉터리 →
  `-I` 순서), 전이 헤더 클로저(`header_to_headers`), 역방향
  `header_to_units`. 스냅샷의 `reverse_impact`는 직접 includer만,
  `transitive_impact`는 헤더 체인 전이 includer까지.
  `MAX_SCANNED_FILES` 도달 → `scan_truncated`. 파일별 include 추출은
  캐시되고, 읽기 실패 소스·미해결 include는 `missing_sources`/
  `unresolved_includes`로 리포트에 남는다. CLI의 `--header` 상대 경로는
  compile database 디렉터리와 각 엔트리의 `directory` 기준으로 해석된다.

### lens-net
- `parser.rs`: `/proc/net/{tcp,tcp6,udp,udp6,unix}` — 주소는 호스트 엔디안
  형식으로 파싱. UDP 바인드(st=07, 와일드카드 리모트)를 리스너로 집계,
  unix `St`는 TCP 상태와 분리.
- `/proc/*/fd` inode→프로세스 상관, inode=0 소켓은 고아 판정에서 제외.
- `diff.rs`: 리스너는 (kind,address,port), established는 5-tuple 매칭.

### lens-env
- venv의 `pyvenv.cfg`/`dist-info` 정적 파싱(인터프리터 실행 없음,
  uv의 `version_info` 키 포함), 미충족 의존성 계산, 프로젝트 소스의
  모듈 섀도잉 탐지.
- `markers.rs`: PEP 508 환경 마커 토크나이저/파서/평가자 —
  `and`/`or`/괄호, `==`/`!=`/`<`/`<=`/`>`/`>=`/`~=`/`in`/`not in`,
  버전 비교, `python_version`/`sys_platform`/`platform_system`/
  `os_name`/`extra` 등. 거짓 마커 요구는 스킵, `extra`는 선택된 extra
  집합에서만 참, 평가 불가는 `unevaluated`로 분리 보고, 범위 불일치는
  `version_conflicts`.

## 4. 번들 포맷 (`lens.bundle/v2`)

`tar.gz` 컨테이너. 첫 엔트리 `manifest.json`(정확 경로만 인정):

```json
{
  "schema": "lens.bundle/v2",
  "tool": "lens",
  "version": "0.7.0",
  "created_at": "<RFC3339>",
  "sources": [{"path": "reports/x.json", "size": N, "sha256": "..."}],
  "diagnostics": ["..."]
}
```

검증 계약:
- 매니페스트는 정확히 1개, `lens.bundle/v2` 스키마 필수.
- 아카이브 엔트리 이름 중복/매니페스트 미등재 엔트리 → 거부(스푸핑 차단).
- 항목 수·해제 바이트 상한(압축 폭탄 방어), 실제 해제 바이트 카운트.
- 생성 시 아티팩트 이름 검증, `output.tmp` 쓰기 후 원자적 rename,
  출력 경로가 심볼링크면 거부.

**한계**: 매니페스트는 서명되지 않음 → 변조 감지(무결성)만 제공, 출처
인증은 아웃오브스코프.

## 5. 안전 메커니즘

- **`SafeInput`**: open 후 `fstat`으로 타깃 메타데이터 재검증(TOCTOU).
  심볼링크 경로는 링크가 아닌 타깃과 비교.
- **리소스 상한**: 번들 항목/바이트, DWARF 타입 10k, MCP 응답 64KiB +
  배열 `limit`(기본 200)·`page_path`/`offset` 페이지네이션(최상위
  `_pagination` 메타데이터 — 배열 안에 마커 객체를 넣지 않는다),
  MCP 로그 `tail` 윈도우, 빌드 스캔 파일 수 — 전부 상한 도달을 출력에 표시.
- **에러 분류**: `LensError::{Io{path,source}, LimitExceeded, InvalidInput,
  InputChanged, Json, Unsupported, Usage}` — 경로와 원인을 유지한 채 전파.
  `Usage`는 인자/플래그 오류 전용으로 "Corrupt or invalid input format"
  접두사 없이 메시지를 출력한다.
- **입력 위생**: 아카이브 엔트리 이름, trash 파일명, include 경로는 전부
  경로 이탈(`..`)/절대경로/널바이트 검증.

## 6. 데이터 흐름 예시 — `lens bundle create --disk /var/log`

```
cli.rs (인자) → ops.rs dispatch
  → lens_disk::DiskScanner::scan (evidence: complete/errors)
  → SnapshotV2::from_tree
  → lens_core::bundle::create_bundle_archive
      (아티팩트 SHA-256, diagnostics 병합, 원자적 쓰기)
  → 출력: 성공/실패 + manifest.sources/diagnostics 요약
```

수집 에러는 예외로 죽지 않고 매니페스트 `diagnostics`에 기록되고,
전원 실패 시에는 빈 번들 생성을 거부한다.

## 7. 리팩터링 규칙

- 도메인 로직은 `lens-*` 도메인 크레이트에, 수집+판정 오케스트레이션은
  `lens-ops`에, 입출력·포맷은 `lens-cli`/`lens-tui`/`lens-mcp`에.
- 새 스키마/필드는 명시적으로 추가하고 문서·가이드 동기화.
- 회귀 테스트는 버그의 실패 모드를 재현하는 최소 케이스로 추가.
- 커밋은 작업 단위별 conventional commit.
