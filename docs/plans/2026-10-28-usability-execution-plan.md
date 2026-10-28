# Lens 기본 설정과 진단 사용성 개선 실행 계획

2026-10-28 사용자 결정은 간편한 사용성, 독립 제품 완성도, 프로젝트 간 연계 순이다. 좋은 기본값으로 일반 검사를 수행하고 반복 작업이나 특수 환경만 설정한다. 기능을 줄이거나 자동 판별을 없애는 계획이 아니다. 2026-10-28 후속 실행 지시로 L0–L8 구현·검증·PR/병합·공개·소유 자원 정리가 승인되었으며 구현을 시작했다.

기준은 main 974f866c7288e12fb9facb734a7a592fd3985137, 공개판 v0.10.0이다. [이전 범용화 기록](../GENERALIZATION.md)은 역사적 완료 근거로 유지한다. 새 기능을 시작하기 전에 실제 HEAD와 사용자 변경을 확인한다.

## 1. 완료할 사용자 흐름

1. 권장 설치 경로로 설치하고 lens inspect PATH를 실행한다. 사용자가 도메인을 미리 고르지 않아도 기존 자동 판별을 사용한다.
2. 결과 첫 화면에서 주요 문제·실패 사례·근거와 검사하지 못한 부분을 읽는다. 필요한 경우 상세 결과와 원본 위치로 이동한다.
3. lens diff BASE CURRENT로 같은 종류의 두 입력을 비교한다. 비교 조건 변화와 실제 회귀를 구분한다.
4. 반복하는 검사·수집은 선택적 lens.toml에 저장하고 매번 긴 옵션이나 wrapper를 작성하지 않는다.
5. 큰 파일·디렉터리·손상 입력·권한 부족·취소에서도 상태가 명확하고 원본이 보존된다.

idk와 ici는 이 흐름의 필수 의존성이 아니다. CLI, TUI, MCP, .lens bundle은 유지한다. 첫 문서와 도움말은 검사·비교·선택 수집 중심으로 재구성하고 크레이트 구조와 릴리스 개발 지침은 개발 문서로 연결한다.

## 2. 설정과 출력 계약

### 2.1 선택적 설정 파일

프로젝트 파일 lens.toml과 선택적인 사용자 선호 파일을 읽는 단일 resolver를 도입한다. 일반 사용에 파일을 만들도록 강제하지 않는다. 명시 --config는 해당 파일과 내장 기본값을 사용하고, --no-config는 지속 설정 발견을 끈다. cwd에서 상위로 가장 가까운 lens.toml을 찾되 VCS root·사용자 home·filesystem root 경계에서 중단한다. 검사 대상 경로나 아카이브 내부에서 설정을 발견하지 않는다. 실제 선택한 파일과 탐색 경계는 설명 출력에서 확인할 수 있어야 한다.

제안하는 최소 설정 형태는 다음과 같다. 최종 키는 L0에서 fixture와 함께 고정한다.

    schema_version = 1

    [output]
    format = "text"

    [policy]
    fail_on = "warn"

설정 파일은 출력·판정·도메인별 제한과 명명된 수집 대상만 선언한다. 임의 shell script 실행이나 MCP 접근 권한 확대 기능은 넣지 않는다. 단일 global exclude를 모든 도메인에 적용하지 않는다. 디스크 검사에서는 build·node_modules도 기본 측정 대상이며 코드 분석 도구의 제외 규칙을 복사하지 않는다.

판정·범위·수집 대상은 내장 기본값 → 사용자 기본값 → 프로젝트 선언 → 명시 CLI 순으로 정한다. 출력 형식·TUI 같은 개인 표시는 프로젝트 기본값보다 사용자 선호가 우선하며 명시 CLI가 최종 우선이다. 명시 --config 모드는 선택한 파일과 내장 기본값만 사용한다. MCP allow-root 등 호출자가 설정한 접근 경계는 이 계층 밖의 상한이다. CLI parser의 default_value가 사용자가 명시한 옵션인 것처럼 설정 파일을 덮지 않도록 한다. 목록의 추가·교체·비움, false와 생략, 상대경로 기준을 구분한다. 반복 수집 대상은 이름으로 선택할 수 있게 하되 실제 unit/container는 한 번 명시해야 한다.

### 2.2 사람이 읽는 기본 결과

명시 옵션이나 설정이 없는 report 명령은 사람이 사용하는 TTY에서 text로 통일한다. non-TTY에서는 기존 명령별 기본 형식을 유지해 기존 pipeline을 보존한다. 명시 --format 또는 설정 파일의 format은 이 자동 선택보다 우선한다. L0에서 모든 report 명령의 TTY/non-TTY/명시 format 행렬을 고정한다. --format json/jsonl/envelope의 데이터 의미와 기존 도메인 schema는 보존하며 자동화에는 명시 옵션을 권장한다. 기본 출력 변경은 release note에 기록한다.

사용자 선호가 없고 프로젝트 lens.toml에 format이 있으면 그 값이 TTY 자동 text보다 우선한다. 사용자 선호가 있으면 개인 표시 규칙이 우선하고 명시 CLI가 최종 우선한다. 이 세 경우를 별도 fixture로 고정한다.

- 첫 줄에 검사 종류·대상·범위와 주요 문제 수를 표시한다.
- JUnit은 실패 case 이름과 message를 기본 출력에서 확인할 수 있어야 한다. 테스트 실패 수와 Lens severity 판정의 이름을 혼동시키지 않는다.
- 기본 화면에 상위 항목을 보여주고 생략 수·상세 조회 방법을 함께 표시한다. 원본 기계 결과를 화면 제한 때문에 조용히 자르지 않는다.
- incomplete 이유, 수집하지 못한 기능, 응답 pagination은 구분한다. exit 0을 모든 대상의 정상으로 설명하지 않는다.
- 긴 작업에서는 현재 진행과 취소 상태를 표시한다. 기존 TUI의 취소·stale 결과 차단·entry cap은 재사용한다.

## 3. 작업 묶음과 의존성

| ID | 작업과 주요 영역 | 선행 | 완료 기준 |
|---|---|---|---|
| L0 | 설정·출력·사용자 흐름 계약과 호환 fixture 고정 | 없음 | no-config/minimal/partial 설정, text 기본 변경 범위, 리스트 의미, 프로젝트 경계, MCP 상한, 9개 도메인 완료표 |
| L1 | lens.toml·사용자 선호·CLI resolver와 설정 설명 | L0 | 생략과 부분 override 보존, CLI 명시 우선, typo 위치 안내, init 전후 의미 동등, root·하위 폴더·두 checkout 경로 시험 |
| L2 | inspect/diff/doctor의 사람이 읽는 결과와 도움말 | L1 | 작은 JUnit 실패 입력에서 case·원인 확인, 도메인별 주요 근거, incomplete·생략 수·다음 동작 표시, 기계 출력 계약 유지 |
| L3 | 반복 수집·snapshot 작업의 간편한 선언 | L1 | unit/container/log 묶음 선택, 출력·시간·용량 기본값, argv를 통한 명시 실행, 기존 stdout/stderr/provenance 보존 |
| L4 | finding과 원본 근거 연결, 입력·관측 대상 식별 | L0, L2 | 선택적 위치/객체/evidence 참조, schema v1 확장 호환, argv를 content identity로 오해하지 않음, namespace/time/scope 미확인 보존 |
| L5 | 대용량·취소·비교 품질과 도메인별 오류 보강 | L2, L3 | bounded text 출력·worker 수·메모리 측정, cancellation 실제 관찰, log 비교 조건 표시, 손상/권한/부분 수집 회귀 |
| L6 | 설치·업데이트·첫 사용 문서와 native 배포 검증 | L1, L2 | 기본 설치 경로 한 가지, Python 없는 환경에서 가능한 설치 경로, target·checksum·기존 실행 파일 충돌 안내, 실제 CLI/TUI/MCP smoke |
| L7 | idk/ici가 소비할 fixture·evidence 연결 | L4 | v1 정상/불완전/대형/위치 없음/오류 fixture, 원본과 비교 의미 보존, 독립 동작 유지 |
| L8 | 후보·공개·정리와 사용자 흐름 마감 | L0–L7 | 자체 품질 gate, 지원 환경별 실제 후보 증거, 승인된 공개 자산 재다운로드 확인, 소유 자원 정리 |

L3/L4와 L6의 독립 부분은 L0 계약 뒤 병렬 가능하다. 동일 ops 출력 계층을 서로 다른 작업자가 동시에 편집하지 않는다. L7은 계약 fixture로 먼저 진행할 수 있으며 ici Rust 구현 완료를 기다려 독립 개선을 멈추지 않는다.

## 4. 독립 제품 완성도

9개 도메인 disk, abi, log, test, trace, sys, build, env, net과 doctor, bundle, TUI, MCP를 완료표에 모두 포함한다. 이미 구현한 기능을 다시 미구현으로 분류하지 않는다. 다음은 기존 의미를 유지하면서 보강할 지점이다.

- build impact는 정적 include 근사다. compile depfile이나 모든 전처리 경로를 대체했다고 표시하지 않는다.
- ABI 타입 layout 분석, 새 파일 형식, 새 OS 지원은 이번 기본 사용성 작업의 필수 범위가 아니다. 기존 ELF 분석과 정확도 표기를 보존한다.
- log 비교의 원시 건수 증가와 동일 시간 구간·량의 회귀를 구분한다. 패턴 그룹은 근거를 잃지 않도록 대표 원문 참조를 유지한다. 시각이 부족하면 rate나 인과관계를 만들어내지 않는다.
- 현재 log stats의 timestamp별 보관 메모리, 입력 인덱스, TUI 빠른 재탐색의 worker 수를 측정한다. 알고리즘 변경 전후 결과 동등성과 peak memory·취소 반응을 실제 fixture로 기록한다.
- network/system 관측 대상이 같다는 증거가 없으면 /proc라는 같은 경로만으로 동일 호스트·namespace라고 판단하지 않는다. 얻을 수 없는 식별 정보는 unknown으로 둔다.
- .lens의 무결성 검증과 수집 완료 여부는 다르다. bundle 파일 checksum이 맞아도 원본 수집 실패를 숨기지 않는다.
- 출력에 경로가 필요하면 실제 domain evidence에서 가져온다. message 문자열에서 path/line을 추정하지 않는다.
- 제외한 항목을 읽지 않았다면 그 bytes 합계는 미확인이다. 제외된 경로·규칙·측정 범위를 표시하되 측정하지 않은 제외 용량을 만들어내지 않는다.

## 5. 연계 계약

기존 lens.result v1과 domain JSON을 유지한다. 선택적 위치·객체·증거 참조는 unknown-field 허용 범위의 추가 필드로 설계한다. required field의 의미가 달라지면 consumer와 새 schema 이행을 함께 검토한다. source.kind=command와 argv identity를 실제 입력 bytes hash로 바꾸어 해석하지 않는다.

ici의 Lens importer는 ici가 소유한다. Lens는 원본 envelope와 참조 가능한 evidence를 제공하고 ici의 코드 gate를 대신 판정하지 않는다. idk의 UI·저장·편집기는 idk가 소유한다. Lens에 idk 전용 daemon·설정 파일 의존성을 넣지 않는다.

교차 검증은 Lens 담당이 제공한 고정 fixture와 공개 바이너리 조합으로 한다. 매번 다른 저장소의 latest main을 필수 build 입력으로 삼지 않는다. 큰 결과는 consumer가 paging/파일 참조로 처리하며, 이를 맞추려고 Lens의 findings를 조용히 누락하지 않는다.

## 6. 필수 인수 시나리오

| 시나리오 | 판정 기준 |
|---|---|
| no-config / minimal / 한 값 override | 검사 대상·기본 정책이 의도한 항목 외에는 동일 |
| inspect JUnit·ELF·log·compile DB·directory | 자동 판별·오류 근거·사람용 요약, 명시 JSON/envelope의 의미 일치 |
| diff 조건 변화 | scope·완결성·시간 정보 변화 표시, 미관측을 개선으로 오판하지 않음 |
| 한도와 취소 | 실제 제한·부분 결과·worker 수·응답성과 자원 회수 관찰 |
| 외부 수집 | 선택 대상만 실행, 없음/권한 실패/timeout/양 스트림 절단/exit 구분 |
| MCP | allow-root와 파일 접근 경계 유지, analysis complete와 pagination 분리 |
| package | fresh prefix, 경로 공백, 설치 충돌, checksum 실패, 공개 후보와 bytes 대조 |
| 독립 사용 | ici/idk 없이 검사·비교·bundle·TUI/MCP 기능 완료 |

기존 cargo fmt, Clippy, workspace tests와 문서·release-flow·installer 검사를 변경별로 수행한다. 제품 후보에는 실제 musl 실행과 CLI/TUI/MCP smoke를 연결한다. Docker/Podman/systemd는 실제 사용 가능한 환경만 tested로 표시한다. Linux/WSL x86_64가 이번 공개 기준이며 native Windows·macOS·다른 CPU 동등 지원은 별도 근거 없이 추가하지 않는다.

## 7. 착수와 완료 보고

Lens 책임자는 자신의 저장소를 소유하고 기능·문서·회귀·실행 증거를 같은 작업 단위로 관리한다. 구현은 목적별 branch/PR로 진행한다. 후보 버전 0.11.0은 계획값이며 완료 범위와 호환 영향에 따라 릴리스 단계에서 확정한다. 후속 실행 지시에서 구현·공개 시작 승인을 받았다. 실제 실행 지시에서 받은 승인 범위를 확인하고 같은 승인에 반복 확인을 요구하지 않는다.

현재 준비 branch는 docs/usability-execution-plan이며 로컬 전용이다. 실제 착수 때 이 문서 commit을 포함한 feature branch/worktree를 만들고 main 대상 PR로 진행한다. 무거운 전체 build/test는 다른 제품 책임자와 host 자원을 조정한다. 계약이 고정된 독립 부분만 병렬화한다.

공개가 승인되면 CI 검증된 source/target·후보 bytes를 게시하고 새 다운로드·설치·세 binary 실행을 확인한다. source SHA·PR·실제 검사·digest·미실행 환경을 완료 원장에 기록한다. 작업 소유 branch/worktree/tmp만 정리하고 기존 사용자 cache·결과·자료는 보존한다.

다음은 구현 착수 시 전달할 지시문이다.

> Lens의 기본 설정과 진단 사용성 개선을 이 문서 L0–L8에 따라 수행하라. 일반 사용은 기본값, 반복·특수 작업은 선택적 lens.toml로 처리하고, 기존 9개 도메인과 CLI/TUI/MCP/bundle 기능을 보존하라. 설정 resolver와 출력 호환 계약을 먼저 고정한 뒤 사람용 결과·선택 수집·대용량 처리를 구현하라. JSON/envelope·완결성·권한 경계를 유지하고 실제 데이터 근거 없는 위치·원인·성능을 주장하지 말라. 다른 제품의 구현은 편집하지 말고 고정 계약 fixture를 전달하라. 전체 완료 기준이 남아 있으면 부분 완료로 기록하고, PR·공개·정리는 전달받은 승인 범위와 저장소 절차에 따라 수행하라.

## 8. 진행 기록

| 항목 | 상태 |
|---|---|
| 사용자 방향과 현재 기능 확인 | 반영 |
| 독립 계획 검토 | Claude·Codex 교차 검토 및 조정 완료 |
| L0–L8 구현 | 진행 중; 아래 실행 원장 참고 |
| 제품 검증·PR·릴리스 | 통합 gate 진행 중 |

근거: [CLI](../../crates/lens-cli/src/cli.rs), [출력과 dispatch](../../crates/lens-cli/src/ops.rs), [자동 판별](../../crates/lens-ops/src/detect.rs), [결과 계약](../RESULT.md), [현재 설치](../INSTALL.md), [도메인 안내](../GUIDE.md).

### 실행 원장

기준 HEAD `7fb230aab8257c1511de546d08070e8f2e08981b` (계획 포함), main `974f866c7288e12fb9facb734a7a592fd3985137`. Clean checkout 확인 후 `feat/usability-execution` 생성. 적용되는 상위/저장소 AGENTS.md 없음.

| WP | 구현 | 실제 검증 | 완료 판정 |
|---|---|---|---|
| L0/L1 | 선택적 resolver·경계·우선순위·init/explain·disk 범위 | 7 unit 및 설정 integration 통과; 29 report 명령 TTY/pipe/명시 format 행렬 | 완료; workspace 430 tests·Clippy 통과 |
| L2 | JUnit 사례/원인, 제한된 요약과 근거·전체 조회 안내, 시작 도움말 | 긴 메시지/25개 실패 사례에서 text 제한 및 envelope 25개 보존 | 구현 완료 |
| L3 | 이름 기반 collect/snapshot, 부분 override와 CLI override | stub argv literal·양 스트림·실패 provenance·bundle 무결성 integration | 구현 완료; 실제 외부 runtime은 미실행 |
| L4/L7 | 선택적 evidence와 5종 고정 consumer corpus | schema·실제 case/env path·bundle pointer 및 이전 JSON round-trip | 구현 완료 |
| L5 | 적응형 log histogram·단일 TUI worker·비교 범위 | 100만행 전후 JSON 동일, RSS 80976→65280 KiB, 실제 취소 0.264ms | 로컬 측정 완료; [한계/재현](../USABILITY_VALIDATION.md) |
| L6 | 설치/첫 사용/선택 설정 문서 및 실제 binary smoke | 9 domains·doctor·bundle·TUI PTY·MCP 접근/paging debug 통과 | musl 후보 smoke 통과; 공개 재다운로드 대기 |
| L8 | 통합/후보/PR/공개/정리 | PR #156 병합 a62b51e; fmt/Clippy/430 tests/musl smoke 통과 | 진행 중 |


무거운 검증은 `flock /tmp/paseo-products-20261028-heavy.lock` 및 jobs/thread 상한 아래 수행한다. 기존 `target`은 사용자/기존 공유 산출물로 보존한다.
