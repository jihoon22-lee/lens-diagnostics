# Lens Forensic Platform (`toy-projects`)

리눅스 시스템 및 소프트웨어 생명주기 전 영역을 아우르는 **초고성능 통합 시스템 진단 및 포렌식 플랫폼**.

파편화되어 있던 기존 8개 독립 도구(`diskmap`, `abilens`, `loglens`, `testlens`, `tracelens`, `servicelens`, `buildscope`, `envlens`)의 엄격한 계약(Fail-Closed, Evidence-Bound, Schema 호환)을 온전히 계승하면서, **단일 정적 Rust 바이너리(`lens`), mmap 기반 Zero-Copy I/O, 사고 포렌식 비행기록장치(`.lens` bundle)**로 전면 재개발 및 현대화되었습니다.

---

## 1. 아키텍처 및 워크스페이스 구조 (`crates/`)

플랫폼은 14개의 고성능 모듈식 Rust 크레이트로 구성되어 있습니다.

```text
toy-projects/
├── Cargo.toml          루트 워크스페이스 정의
├── crates/
│   ├── lens-core/      공통 기반 (Zero-Copy I/O, SafeInput TOCTOU 방어, SHA-256, 3상태 Diff, 번들 매니페스트 무결성 검증)
│   ├── lens-disk/      스토리지 분석, 연속 메모리 아레나 트리, 중복 파일 탐지, FreeDesktop Trash
│   ├── lens-abi/       ELF 동적 심볼 검사, C++/Rust 심볼 디맹글링, ABI 3상태 Diff
│   ├── lens-log/       mmap 제로카피 라인 인덱서, 무할당 필터 평가, Session v2
│   ├── lens-test/      quick-xml 초고속 스트리밍 테스트 파서, 회귀 자동 탐지
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

## 2. 레거시 도구 대비 혁신 지표

| 영역 | 기존 레거시 도구 | 차세대 Rust 엔진 (`lens`) |
|---|---|---|
| **배포 형태** | Qt6, Python, libstdc++ 런타임 종속 8개 분열 | 단일 정적 바이너리 (`lens`) |
| **디스크 트리 저장** | 경로별 개체 분산 할당 | `u32` 인덱스 연속 아레나(`ArenaTree`), 캐시 지역성 확보 |
| **로그 검색 핫패스** | 검색 시마다 `toLowerAscii` 힙 할당 (100만회+) | mmap 라인 인덱싱 + 무할당 슬라이딩 윈도우 필터 (`contains_insensitive`) |
| **테스트 XML 파싱** | Python `defusedxml` | `quick-xml` 스트리밍 파서 |
| **strace 다중스레드** | C++ 파서 복잡성, 스레드 중첩 분실 위험 | 비동기 호출 매칭 및 스트리밍 파서 |
| **systemd 정적 분석** | Python AST 파싱 | 무평가 정적 파서 + Tarjan SCC 순환 그래프 탐지 |
| **빌드 영향도 분석** | CMake 캐시 수동 파싱 | 헤더→소스 역방향 영향도 분석 |
| **Python 환경 감사** | Python 인터프리터 구동 위험 | 제로 코드 실행 정적 메타데이터 & 섀도잉 감사 |
| **사고 포렌식 기록** | 도구별 산출물 수동 수집 | 통합 `.lens` 비행기록장치 번들 생성/검사/검증 |

> 정확한 구현 상태와 한계는 [crates/README.md](crates/README.md)와 소스를 참조하세요. 심볼 버전 수집, DWARF 타입 추출, `.d/` drop-in 병합 등이 구현되어 있습니다.

---

## 3. 빌드 및 테스트

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

## 4. 통합 CLI 사용법 (`lens`)

단일 실행 파일 `lens` 하나로 9개 진단 도메인(disk, abi, log, test, trace, sys, build, env, net)의 기능과 포렌식 번들링을 모두 실행할 수 있습니다.

```bash
# 1. 파일시스템 초고속 스캔 & 중복 파일 탐지 (동일 inode 하드링크 자동 인식)
$ lens disk scan .
$ lens disk duplicates . --min-size 1048576

# 2. 바이너리 ABI 검증 및 3상태 호환성 diff (abilens.report/v2)
$ lens abi inspect /usr/bin/python3
$ lens abi diff lib_v1.so lib_v2.so

# 3. 수 GB 로그 파일 무할당 제로카피 고속 필터링
$ lens log filter /var/log/syslog --query "error" --min-level warn

# 4. 스트리밍 테스트 리포트 파싱 및 회귀(Regression) 자동 탐지
$ lens test parse target/junit.xml --project backend
$ lens test diff run_baseline.xml run_candidate.xml

# 5. Syscall 트레이스(strace) 분석 및 지연시간/신규 에러 탐지
$ lens trace analyze /var/log/strace.log
$ lens trace diff baseline_trace.log candidate_trace.log

# 6. systemd 유닛 순환 의존성(Cycle) 오프라인 정적 탐지
$ lens sys cycles /etc/systemd/system

# 7. 헤더 수정 시 재컴파일이 필요한 소스 파일 역방향 영향도 분석
$ lens build impact compile_commands.json --header include/common.h

# 8. Python 가상환경 종속성 및 표준 라이브러리 모듈 섀도잉 정적 검사
$ lens env inspect .venv --project .

# 9. 활성 소켓/포트/네트워크 포렌식 검사 및 Diff
$ lens net inspect
$ lens net diff net_baseline.json net_candidate.json

# 10. 종합 사고 포렌식 비행기록장치(.lens) 번들 생성/검사 및 매니페스트 무결성 검증
$ lens bundle create incident.lens --disk /var/log --trace /tmp/strace.log --test target/junit.xml
$ lens bundle inspect incident.lens
$ lens bundle verify incident.lens

# 11. 시스템 종합 상태 원클릭 점검 (Storage, Network, Services, Security)
$ lens doctor
$ lens doctor --json

# 12. 인터랙티브 TUI 대시보드 (스토리지/서비스/로그/네트워크 실데이터)
$ lens tui .

# 13. 셸 자동완성 스크립트 생성 (Bash, Zsh, Fish)
$ source <(lens completion bash)
```

---

## 5. 상세 문서

- [사용 가이드](docs/GUIDE.md) — 전체 커맨드·출력 스키마·번들 워크플로
- [아키텍처](docs/ARCHITECTURE.md) — 크레이트 책임, 데이터 흐름, 설계 불변조건
- [크레이트별 아키텍처·스키마·구현 상태](crates/README.md)
- [로드맵](ROADMAP.md)
- [변경 이력](CHANGELOG.md)
