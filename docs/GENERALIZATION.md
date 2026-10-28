# Lens Diagnostics 전환 진행 원장

2026-10-28 사용자 승인: 권장 전체 구현, 문서, 버전/태그/공개 릴리스, 소유 작업 자원 정리. 저장소 rename 승인 포함.

## 시작 상태와 보존 대상
- 원본 checkout `/home/jihoon/projects/toy-projects`: main `11c537872473eb38140a7219c7661415c9561dc7`, clean.
- 시작 당시 local/remote branch main만 존재, worktree 원본 하나, 열린 PR 없음. 공개 latest v0.9.0.
- 기존 target/cache 및 사용자 파일은 보존. 기존 공개 태그와 release asset 변경 금지.
- 이번 작업 소유: branch `feat/generalization`, worktree `/home/jihoon/projects/lens-generalization`.
- 공통 heavy lock `/tmp/paseo-generalization-20261028-heavy.lock`은 부모 소유; 삭제 금지.

## 완료
- GitHub 저장소를 `jihoon22-lee/lens-diagnostics`로 rename; origin 변경. 이력/릴리스 유지. 부모에 canonical URL 전달.
- 로컬 원본 폴더 이름은 활성 세션 보호를 위해 유지.

## 구현 및 검증 계획
- [x] P0-A build impact missing/unresolved/truncated complete 회귀 수정, 정적 분석 범위 표시.
- [x] P0-B 제품 정의/지원 경계/정적 링크 설명 정정.
- [x] P1-A 선택 JSON envelope, 기존 JSON 유지, CLI/MCP 판정과 응답 절단 구별.
- [x] P1-B 수집 capability/범위, doctor Skip/Pass 및 diff 범위 변화, bundle provenance.
- [x] P2-A 플랫폼 경계: identity/Linux 수집 격리, 기존 안전 계약 유지.
- [x] P2-B 명시적 systemd user manager와 system 회귀 보호.
- [ ] P3-A 명시 target 배포 및 설치, checksum/다운로드 실행 검증.
- [x] P3-B 스캔 취소/오래된 워커 결과 차단/대규모 탐색.
- [x] P4 로그 비교, 사용자 선택 journald/container 수집 adapter. 기존 stdin/JSONL/JUnit/ABI/env 기능 재사용.
- [ ] docs/README/GUIDE/지원표/CHANGELOG/metadata 갱신, 버전 결정.
- [ ] 필요한 좁은 테스트 및 heavy lock 하 전체 fmt/clippy/test/문서 검사, 실제 MCP 통합, CI/PR 병합.
- [ ] 릴리스 게시/공개 asset 다운로드 digest 및 smoke 확인.
- [ ] main 동기화, 소유 merged branch/worktree/임시자원 정리.

## 지원 범위
Linux/WSL/container 우선. 폐쇄망은 설치 환경으로 유지. macOS/native Windows/PE/Mach-O 및 완전한 compiler 영향도는 조건부 후속 검토이며 이번 릴리스의 동등 지원 약속이 아니다. 미실행 검사와 현장 미검증은 PASS로 기록하지 않는다.

## 계약
CLI `lens`, crate `lens-*`, `.lens`, 기존 schema 및 JSON 기본 출력 유지. 신규 envelope 이름/옵션은 구현 시 이 문서에 확정 기록. 실행파일+argv/표준 입출력 우선; daemon/plugin 플랫폼 추가하지 않음.

## 2026-10-28 재개 기록
- P0-A: impact 및 inspect의 missing/unresolved/truncated를 공통 incomplete 판정으로 통일. 강제 include 누락과 읽지 못한 헤더도 집계. 정적 분석 범위를 diagnostics에 추가. 좁은 회귀 테스트 실행 중.
- P0-B: Lens Diagnostics 이름, canonical metadata/Pages, Linux/WSL/container 및 정적 링크·빌드 분석 한계 README에 반영.
- 다음: P0 테스트 결과 확인 후 commit, P1 envelope/수집 범위 구현.

- P0 검증: lens-ops build 회귀 4/4 통과. 소유 target 경로는 worktree/target.
- P1-A 계약 확정: `--format envelope`, `lens.result` schema_version=1, docs/RESULT.md 및 docs/fixtures/lens-result-v1.json. idk에 전달. 기본 JSON 유지, 실제 프로세스 exit와 보고 판정 분리. 컴파일 검증 진행.

## P1–P3 진행 (2026-10-28)
- P1-A envelope 컴파일 통과, CLI 실제 프로세스 테스트 추가(기본 JSON data 일치, IDK_RUN_ID, incomplete exit 1, streaming 거부).
- P1-B `CollectionScope`/capability를 net 보고서에 추가. optional protocol 부재와 denied/failed 분리, procfs source 명시. doctor check evidence_complete, Skip 전환 scope_changed, bundle collection incomplete 전파. lens-net 8 + lens-ops 57 테스트 통과(추가 변경 전).
- P2-A POSIX identity/nonblocking-open backend를 lens-core/platform.rs로 격리; Linux 상수 하드코딩 제거, inode/시간/모드 및 FIFO-race 방어 유지. native Windows는 backend 부재를 명시적으로 거부하며 지원 확대를 주장하지 않음.
- P2-B system/user manager context를 parser/loader/template/drop-in에 명시 전달. `sys inspect/cycles --user`, account lookup과 XDG search dirs, 스냅샷 manager identity 및 scope 변경 경고. 기존 system tests 28/28 통과, user template/drop-in 회귀 추가.
- P3-B DiskScanner 취소 토큰, TUI c 취소와 receiver 교체로 stale 결과 차단; 큰 디렉터리 enumeration/stat 이전 max_entries 메모리 상한. truncated subset 순서 미보장, incomplete 명시.
- 검증 묶음에서 새 TUI 테스트의 생성자 인자 오류 발견 후 수정. 다음 전체 검증에 재실행. P4 log diff 구현 중; 다음 명령은 cargo test/check (공통 heavy lock).

## P3-A/P4 및 검증 보강
- 배포 target을 x86_64-unknown-linux-musl로 명시, readelf INTERP 부재/버전/help 검사 후 package. BUILD.json에 source SHA/target/rustc/binary SHA256 기록. 기존 linux-x86_64 archive 이름은 동일 bytes alias로 유지. 공개 전 다운로드 후 source SHA/실행 smoke 검증.
- scripts/install.py: 정확한 버전, checksum + 내부 manifest 검증, 제한된 archive 멤버만 읽고 ~/.local/bin 설치. offline archive/checksum 지원. 기존 실행파일 교체는 --force 명시.
- P4 log diff: 기존 파서/stat 재사용, severity delta/입력 시간 구간/불균등 구간 미정규화 명시, ERROR/FATAL 증가 findings. 일반 lens diff 자동 경로 연결.
- P4 collect journal/docker/podman: 명시 target만 argv로 실행. 30s default timeout/max300s, stream별8MiB cap, 독립 process group, bounded nonblocking capture. stdout/stderr/exit/limit/source argv를 .lens에 저장, 실패는 incomplete. shell/sudo/service mutation 없음.
- Release flow 로컬 fake API 테스트 6/6 통과. 전체 Cargo 테스트 재실행 중. 이전 doctor capacity evidence_complete 오설정 수정, denied owner/malformed sys 회귀 기대값을 honest incomplete로 변경, host snapshot 테스트를 독립 procfs fixture로 변경.
- 소유 임시파일: /tmp/lens-generalization-tests.log (종료 후 삭제).

## 교차 검증
- idk 실제 소비자 시험 PASS (담당 세션 통보): HEAD278f0dde + dirty producer0.9.0 개발 바이너리 SHA256 ec2ab8227d38a3ebc416dd4aad4d618df5ac4037451b59e5d55440544d8774c0 전후 동일. idk native task가 임시 missing-source compile DB에 envelope build impact 실행; 실제 exit1/Failed, cleanup=true, complete=false, findings2, run_id==IDK_RUN_ID, merged_pty=false 확인. 증거 /tmp/idk-lens-integration-csm0ghug/evidence.json/run.json는 idk 소유(삭제하지 않음). 공개 release 검증과 구분.
- P0 분리 PR: https://github.com/jihoon22-lee/lens-diagnostics/pull/152 (`fix/build-evidence`, 소유 local/remote branch). 필수 CI 진행 중.
- 추가 소유 임시 파일 /tmp/lens-generalization-p0-pr.txt, /tmp/lens-generalization-clippy.log. 환경 Linux 6.18.33.2-microsoft-standard-WSL2 x86_64.

- 실제 WSL 수집 smoke 소유 경로: /tmp/lens-generalization-smoke-yq4hzd09; summary.json 기록. 임의 미존재 unit/container를 선택해 source invocation/exit/번들 무결성만 검증(실제 populated production 로그 수집 PASS 아님). user manager account identity 실제 조회 검증.

- P0 PR152 필수 Test Workspace/Build Release/Merge Gate 통과 후 merge main f938a12. feat/generalization을 해당 main으로 fast-forward, 기능 dirty 상태 보존.
- 전체 Cargo 406개 테스트 통과(이후 log diff/collector stdio/MCP 및 bundle scope 메타데이터 추가로 재검증 중). clippy 첫 실행은 테스트 뒤 re-export/Drop 위치 규칙 위반 발견, 선언 위치 수정.
- 실제 source smoke 결과: journal user unit(empty observation) source0/lens0/verify0, docker 미존재 container source1/lens1/incomplete/verify0, 실제 계정 user manager name/home 일치. Podman은 로컬 미설치이므로 실제 runtime 시험 미수행(동일 argv/캡처 계약 테스트 적용).
- 자체 코드 검토 보강: 프로세스 열거/FD 읽기 실패를 capability에 반영하고, 관측 중 종료된 PID는 권한 실패로 오분류하지 않음. unavailable/denied/failed를 구별하고 전체 열거 실패 시 orphan 단정 방지. 빈 XDG_*_DIRS 값에는 표준 fallback 적용. 실패 범위 회귀 테스트 추가.
- 최종 기능 보강: finite log inspect/filter도 envelope 지원. follow/TUI/completion/trash는 명시 거부. lossy UTF-8은 기존 표시 데이터를 유지하면서 CLI/MCP incomplete 진단을 일치시킴. 제한한 match 수는 의도한 scope라 incomplete로 오분류하지 않음.
- 기능 PR153: https://github.com/jihoon22-lee/lens-diagnostics/pull/153 (초안). 현재 전체 suite 413개 테스트 통과; 이후 finite filter 추가 회귀는 다음 좁은 검증에서 확인. 이전 '406개' 표기는 집계 오류이며 실제 직전 suite는 408개였음.
