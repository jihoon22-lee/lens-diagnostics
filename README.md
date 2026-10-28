# Lens Diagnostics (`lens`)

Linux 시스템과 개발 산출물을 검사하고 두 결과를 비교하는 도구입니다. 로그, JUnit, ELF, 디렉터리 등을 자동 판별하며 `.lens` 번들, TUI와 MCP도 제공합니다. idk나 ici 설치는 필요하지 않습니다.

## 설치

Linux x86_64에서는 버전을 지정한 공식 installer를 권장합니다. Python 3.10 이상이 필요합니다.

```sh
curl -fLO https://github.com/jihoon22-lee/lens-diagnostics/releases/download/v0.10.0/install.py
python3 install.py --version v0.10.0
~/.local/bin/lens --version
```

`~/.local/bin`을 PATH에 추가한 뒤 다음 명령을 사용하세요. Python 없는 설치, 체크섬 확인, 업데이트와 기존 실행파일 충돌은 [설치 가이드](docs/INSTALL.md)에 설명되어 있습니다.

## 첫 검사와 비교

```sh
lens inspect target/junit.xml
lens inspect /var/log/syslog
lens inspect .
lens diff baseline.xml candidate.xml
lens doctor
```

검사 대상을 지정하면 도메인을 자동 판별합니다. 터미널에서는 사람이 읽는 text가 기본이며 파이프에서는 기존 명령별 형식을 유지합니다. 자동화는 `--format json` 또는 `--format envelope`를 명시하세요. 종료 코드 0은 설정된 실패 기준을 넘지 않았다는 뜻이며 모든 대상의 정상이나 전체 수집을 보장하지 않습니다. 불완전 결과는 종료 코드 1입니다.

## 반복 작업만 설정하기

설정 없이 바로 사용할 수 있습니다. 반복하는 정책만 프로젝트의 선택적 `lens.toml`에 기록하세요.

```toml
schema_version = 1

[policy]
fail_on = "warn"
```

`lens config init`은 기본 의미를 바꾸지 않는 최소 파일을 만들고, `lens config explain`은 선택된 설정과 탐색 경계를 보여줍니다. `--no-config`는 파일 설정을 끄며 `--config PATH`는 해당 파일만 사용합니다. 출력 형식은 개인 선호가 프로젝트보다 우선하고 명시 CLI 옵션이 최종 우선입니다. 자세한 규칙과 명명된 수집은 [사용 가이드](docs/GUIDE.md)를 참조하세요.

## 지원 범위

Linux x86_64 musl 배포를 제공합니다. WSL과 컨테이너에서는 노출된 `/proc`, 권한, systemd 유무에 따라 수집 범위가 달라집니다. macOS/native Windows 동등 지원과 PE/Mach-O 분석은 포함하지 않습니다. `build impact`는 정적 include 근사이며 compiler depfile을 대체하지 않습니다.

## 도메인별 사용법 (`lens`)

단일 실행 파일 `lens` 하나로 9개 진단 도메인(disk, abi, log, test, trace, sys, build, env, net)의 기능과 포렌식 번들링을 모두 실행할 수 있습니다.

**가장 쉬운 사용법** — 도메인을 몰라도 자동 판별됩니다:

```bash
$ lens doctor            # 시스템 종합 점검
$ lens inspect <path>    # 무엇인지 자동 감지 후 해당 검사 실행
$ lens diff <a> <b>      # 양쪽 입력을 감지해 호환되는 diff 실행
$ lens snapshot          # 기준선 번들 생성 (snapshot-<host>-<시각>.lens)
$ lens diff old.lens new.lens   # 두 시점의 시스템 상태 비교
```

```bash
# 1. 파일시스템 초고속 스캔 & 중복 파일 탐지 (동일 inode 하드링크 자동 인식)
$ lens disk scan .
$ lens disk duplicates . --min-size 1048576

# 2. 바이너리 ABI 검증 및 3상태 호환성 diff (abilens.report/v2)
$ lens abi inspect /usr/bin/python3
$ lens abi diff lib_v1.so lib_v2.so

# 3. 수 GB 로그 파일 무할당 제로카피 고속 필터링
$ lens log filter /var/log/syslog --query "error" --min-level warn
$ lens log filter /var/log/syslog -f --min-level warn   # tail -f처럼 실시간 팔로우
$ lens log stats /var/log/syslog                       # 레벨 분포 + 시간 히스토그램

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

## 상세 문서

- [결과 envelope 계약](docs/RESULT.md) — 기존 JSON과 선택 `lens.result` v1
- [설치 및 지원 범위](docs/INSTALL.md)
- [사용 가이드](docs/GUIDE.md) — 전체 커맨드·출력 스키마·번들 워크플로
- [아키텍처](docs/ARCHITECTURE.md) — 크레이트 책임, 데이터 흐름, 설계 불변조건
- [크레이트별 아키텍처·스키마·구현 상태](crates/README.md)
- [로드맵](ROADMAP.md)
- [변경 이력](CHANGELOG.md)

- [개발·테스트·릴리스 절차](docs/DEVELOPMENT.md)
