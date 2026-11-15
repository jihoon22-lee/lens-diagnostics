# 개발과 릴리스

사용자 설치와 첫 검사는 [README](../README.md)를 참조하세요.

## 1. 아키텍처 및 워크스페이스 구조 (`crates/`)

플랫폼은 14개의 Rust 크레이트로 구성되어 있습니다.

```text
lens-diagnostics/
├── Cargo.toml          루트 워크스페이스 정의
├── crates/
│   ├── lens-core/      공통 기반 (Zero-Copy I/O, SafeInput TOCTOU 방어, SHA-256, 3상태 Diff, 번들 매니페스트 무결성 검증)
│   ├── lens-disk/      스토리지 분석, 연속 메모리 아레나 트리, 중복 파일 탐지, FreeDesktop Trash
│   ├── lens-abi/       ELF 동적 심볼 검사, C++/Rust 심볼 디맹글링, ABI 3상태 Diff
│   ├── lens-log/       mmap 라인 인덱서, 필터 평가, Session v2
│   ├── lens-test/      quick-xml 스트리밍 테스트 파서, 회귀 자동 탐지
│   ├── lens-trace/     strace 스트리밍 파서, FD 누수/IO 처리량 추적, 미완료 스레드 복원, 지연/에러 diff
│   ├── lens-sys/       systemd 유닛/드롭인 파서, Specifier 확장, Tarjan SCC 순환 탐지
│   ├── lens-build/     compile_commands.json 파서, 플래그 정규화, 헤더 영향도 역방향 DAG
│   ├── lens-env/       Zero-Code Python venv 분석기, 미충족 패키지 검사, import 섀도잉 탐지
│   ├── lens-net/       네트워크 소켓 포렌식, IPv4/IPv6 /proc/net 무실행 파싱, 프로세스 FD 상관관계, 포트 Diff
│   ├── lens-ops/       공유 오퍼레이션 레이어 — 수집+판정 `Report<T>`/findings, doctor 구현, 번들 수집
│   ├── lens-cli/       단일 통합 CLI (`lens`), 시스템 종합 진단 (`doctor`), 셸 자동완성, 포렌식 번들
│   ├── lens-tui/       대화형 터미널 UI 대시보드 (Storage, Services, Logs, Network 실데이터)
│   └── lens-mcp/       AI 어시스턴트(Claude, Antigravity) 연동용 Model Context Protocol 서버
```

---

## 2. 구현 범위와 측정

[크레이트 안내](../crates/README.md)에 구현과 제약을 기록한다.
[검증 기록](USABILITY_VALIDATION.md)의 특정 환경 측정을 일반적인 성능 보장으로 확대하지 않는다.
CLI와 MCP는 `lens-ops`의 판정을 공유하며 TUI는 도메인 crate를 직접 사용한다.
TUI의 요약 화면을 CLI/MCP의 전체 보고서와 동일한 계약으로 간주하지 않는다.

## 3. 설치, 빌드 및 테스트

[설치 가이드와 지원표](INSTALL.md)에서 공개 target, 폐쇄망 전달, 검증 절차를 확인하세요.
[릴리스](https://github.com/jihoon22-lee/lens-diagnostics/releases)에는 `lens`, `lens-tui`, `lens-mcp`와 체크섬을 제공합니다.


### 전체 테스트 실행 (14개 크레이트 동시 검증)
```bash
cargo test --workspace --jobs 2
```

### 릴리즈 바이너리 컴파일
```bash
cargo build --release -p lens-cli --jobs 2
# 생성 바이너리: target/release/lens
```

### 릴리스 절차

버전 범프는 수동 PR로 진행한다 (release-please가 생성하지 않음).

1. `main`에서 `release/vX.Y.Z` 브랜치를 만들고 `chore(release): vX.Y.Z`
   커밋 하나로 다음을 수정한다:
   - `.release-please-manifest.json` — `"."` 버전
   - `Cargo.toml` — `[workspace.package] version`
   - `Cargo.lock` — `cargo check`로 재생성 (lens-* 크레이트 버전만 바뀌어야 함)
   - `CHANGELOG.md` — `## Unreleased` 헤딩을 `## X.Y.Z`로 바꾸고 한 줄 요약 추가
2. CI Merge Gate 통과 후 머지 커밋으로 병합한다. Merge Gate는
   `main` 대상 PR에서만 동작하므로 스택드 PR 체인은 머지할 수 없다 —
   통합 브랜치를 `main`으로 머지할 것.
3. 머지되면 release-please 워크플로가 manifest 버전과 기존 태그를
   비교해 태그가 없으면 Release 워크플로를 자동으로 호출한다.
   아티팩트 빌드와 `SHA256SUMS` 검증 후 해당 SHA에 태그를 만들어
   릴리스를 발행한다. 자동 호출이 실패했거나 건너뛴 경우에는
   수동으로 실행할 수 있다:
   ```bash
   gh workflow run release.yml -f tag=vX.Y.Z -f sha=<머지 커밋 SHA>
   ```

---
