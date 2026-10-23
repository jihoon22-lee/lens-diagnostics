# Lens v0.8.1 리뷰 후속 수정 기록

`main@02f742571d77cb11640a3c8f7cf7e24c52996a48`을 대상으로 수행한 v0.8.0
리뷰에서 확인한 14건(P1 3건, P2 11건)을 수정했다. 별도로 발견한 호스트 로그
의존 테스트도 고정 fixture로 바꿨다. 아래 ID는 원래 리뷰의 ID를 유지한다.

구현은 마스킹 → 자원·입력 무결성 → systemd 의미론 → 도메인 판정 → 통합 검증
순서로 진행했다. 각 변경은 원래 재현 입력, 정상 대조군, 호출부, 회귀 테스트,
외부 규격·구현을 대조했다.

## 수정과 회귀 근거

| ID | 기존 문제 | 수정 후 동작과 검증 근거 |
|---|---|---|
| R01 · P1 | 이스케이프된 비밀값 유출 및 JSON 손상 | JSON 값을 디코딩한 상태에서 마스킹하고 배열·객체 구조를 유지한다. [코어 테스트](../crates/lens-core/src/redact.rs)와 [번들 roundtrip](../crates/lens-ops/src/bundle.rs)에서 비밀값 부재·JSON 재파싱·무결성·diff를 확인했다. |
| R02 · P1 | 잘못된 UTF-8이면 원문 로그를 그대로 포함 | 마스킹 번들에서 해당 로그를 생략하고 진단·`complete=false`를 남긴다. [번들 테스트](../crates/lens-ops/src/bundle.rs)에서 원문 누락과 정상 요약 보존을 확인했다. |
| R03 · P1 | 템플릿 확장의 지수적 메모리 증가 | [공통 작업 큐](../crates/lens-sys/src/resolve.rs)에 유닛·참조·깊이·이름·메타데이터 상한을 적용한다. [경계 테스트](../crates/lens-sys/tests/systemd_contracts.rs), [doctor·번들 전파](../crates/lens-ops/src/sys.rs), [MCP 생존 테스트](../crates/lens-mcp/tests/mcp_stdio.rs)를 추가했다. |
| R04 · P2 | MCP 크기 경계가 UTF-8 중간에 걸리면 종료 | 바이트 버퍼에서 한도를 판정한 뒤 JSON을 파싱한다. [stdio 테스트](../crates/lens-mcp/tests/mcp_stdio.rs)에서 한글·이모지·잘못된 UTF-8·정확한 한도 경계 뒤의 ping을 확인했다. |
| R05 · P2 | `close_range` 숫자 플래그 오해석 | UNSHARE=2, CLOEXEC=4로 수정했다. [추적 테스트](../crates/lens-trace/src/parser.rs)가 기호·10진수·16진수·조합·공유 FD·exec 이후 동작을 비교한다. |
| R06 · P2 | 합성 인스턴스의 전용 drop-in 누락 | [로더](../crates/lens-sys/src/loader.rs)에서 인스턴스별 유효 설정을 만든다. [회귀 테스트](../crates/lens-sys/tests/systemd_contracts.rs)는 두 로더, 명시적 링크 대조군, 파일명 순서와 검색 경로 우선순위를 확인한다. |
| R07 · P2 | `.wants`·`.requires` 변경이 diff에서 누락 | [diff](../crates/lens-sys/src/diff.rs)가 유효 의존성도 비교한다. 링크 추가·제거 양방향을 검증했다. |
| R08 · P2 | 복합 PEP 440 접미사를 잘라 버전 충족으로 오판 | `pep440_rs`의 파싱·정규화·정렬을 사용하고 명시자 경계 규칙을 보완했다. [PyPA 기준 fixture](../crates/lens-env/tests/fixtures/pep440-reference.json)의 2,376개 비교와 [실제 venv 메타데이터](../crates/lens-env/src/venv.rs)를 검증했다. |
| R09 · P2 | 이름 부분일치로 비공개 타입을 공개 ABI로 간주 | 노출 관계가 입증되지 않은 타입 크기 변경은 `uncertain`이다. [실제 ELF 테스트](../crates/lens-abi/src/elf.rs)에서 `State`와 `get_state_count` 오탐을 막았다. 공개 심볼·TLS 등의 기존 비호환 판정은 유지한다. |
| R10 · P2 | 강제 include 순서 변경을 집합 비교가 삭제 | 선택적 `forced_include_order.before/after`로 순서를 보존한다. [빌드 diff 테스트](../crates/lens-build/src/diff.rs)가 명령 파싱·순서만 바뀐 경우·JSON roundtrip·기존 입력을 확인한다. |
| R11 · P2 | `--no-unix`로 제외한 소켓이 경고 집계에 잔존 | 필터링 후 [공통 summary 계산](../crates/lens-net/src/parser.rs)을 적용한다. [ops 테스트](../crates/lens-ops/src/net.rs)에서 소켓·리스너·orphan·findings·검사 범위를 검증했다. |
| R12 · P2 | gzip CRC 손상을 정상 번들로 인정 | tar 종료 후에도 같은 해제 상한으로 gzip을 끝까지 읽는다. [번들 테스트](../crates/lens-core/src/bundle.rs)가 CRC 손상·잘린 트레일러를 inspect/verify/show/extract에서 거부하고 미완성 추출을 남기지 않는지 확인한다. |
| R13 · P2 | `%u`·`%h`를 `User=`로 잘못 확장 | 시스템 매니저의 `root`·`/root`를 사용한다. [systemd 테스트](../crates/lens-sys/tests/systemd_contracts.rs)는 `User=` drop-in 변경의 영향을 받지 않는지 확인한다. |
| R14 · P2 | 빈 `After=`가 의존성을 지워 순환 은폐 | 의존성 지시어의 빈 값은 기존 관계를 보존한다. [동일 테스트](../crates/lens-sys/tests/systemd_contracts.rs)에서 순환 유지와 `ExecStart=`의 정상 리셋을 함께 확인했다. |

기존 스냅샷 테스트의 호스트 로그 크기 의존성은
[명시적 로그 fixture](../crates/lens-cli/tests/cli_regression.rs)로 제거했다.
템플릿의 숨은 thread-local 원문 캐시도 제거해 독립 파싱·스냅샷 직렬화 후에도
인스턴스 확장 결과가 유지되도록 했다. 인스턴스 전용 drop-in은 기존 MCP
`--allow-root` 읽기 정책을 그대로 통과해야 한다.

## 검증 결과

| 검증 | 결과 |
|---|---|
| `cargo fmt --all -- --check` | 통과 |
| `cargo clippy --workspace --all-targets --locked --jobs 2 -- -D warnings` | 경고 없이 통과 |
| `cargo test --workspace --locked --jobs 2 --no-fail-fast` | 394 통과, 0 실패, 0 ignored |
| `cargo build --release --workspace --locked --jobs 2` | 통과 |
| `python3 .github/scripts/check_docs.py` | 문서 링크·버전 정합 통과 |
| `python3 .github/scripts/test_release_flow.py` | 6개 통과 |
| 릴리스 바이너리 원래 재현 입력 | R01~R14 모두 기대 결과와 일치 |
| 자원 제한 실행 | R03 가지형 템플릿이 192 MiB 주소공간·CPU 10초 제한 안에서 종료 2와 `LimitExceeded` 반환; 정상 10,000노드 연쇄 그래프는 192 MiB 안에서 종료 0 |
| 외부 구현 대조 | systemd 인스턴스 drop-in·빈 의존성 순환이 `systemd-analyze verify`와 일치; PEP 440은 PyPA 기준 fixture와 일치 |

## 호환성과 지원 범위

- JSON 스키마 식별자는 유지한다. `forced_include_order`는 선택적 필드이며
  기존 diff 입력을 계속 읽는다. systemd 지원 의미론 표기는
  `systemd-255-subset-v2`로 갱신했다.
- Rust 호출부는 `load_units_merged`, `OrderingGraph::build`,
  `snapshot_from_units`, doctor 서비스 검사·보고서 생성의 `Result`를
  처리해야 한다. 저장소 안의 CLI·MCP·TUI·번들 호출부를 함께 변경했다.
- systemd는 시스템 매니저의 정적 부분집합을 지원한다. 사용자 매니저 확장이나
  DWARF의 전체 타입 도달성 분석을 추가한 릴리스는 아니다.
- 새 런타임 의존성은 `pep440_rs`와 그 의존성 `unscanny`다. Python은
  대조 fixture 생성에만 사용하며 제품 실행에서는 호출하지 않는다.

판정 근거는 [systemd 255 유닛 규격](https://www.freedesktop.org/software/systemd/man/255/systemd.unit.html),
[Linux close_range UAPI](https://github.com/torvalds/linux/blob/master/include/uapi/linux/close_range.h),
[PyPA 버전 규격](https://packaging.python.org/en/latest/specifications/version-specifiers/),
[GCC 강제 include 규칙](https://gcc.gnu.org/onlinedocs/gcc/Preprocessor-Options.html#index-include)이다.
사용자용 동작은 [가이드](../docs/GUIDE.md), 구현 계약은
[아키텍처 문서](../docs/ARCHITECTURE.md)와 [크레이트 개요](../crates/README.md)에 반영했다.
