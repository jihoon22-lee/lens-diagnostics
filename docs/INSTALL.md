# 설치와 지원 범위

공식 저장소는 [Lens Diagnostics](https://github.com/jihoon22-lee/lens-diagnostics)입니다.
CLI 이름 `lens`, `lens-tui`, `lens-mcp`와 `.lens`, 기존 도메인 스키마는 유지합니다.

## 릴리스 설치

0.10부터 공식 Linux x86_64 아카이브는 `x86_64-unknown-linux-musl` target입니다.
릴리스 과정에서 ELF interpreter 부재를 확인한 정적 실행파일을 제공합니다.
기존 `linux-x86_64` 파일명도 동일 bytes의 호환 alias로 유지합니다.
GNU target으로 소스 빌드한 실행파일에는 해당 빌드 환경과 호환되는 glibc가 필요합니다.

Python 3.10 이상이 있는 환경에서 정확한 버전을 지정해 설치할 수 있습니다.

```sh
curl -fLO https://github.com/jihoon22-lee/lens-diagnostics/releases/download/v0.10.0/install.py
python3 install.py --version v0.10.0
~/.local/bin/lens --version
```

installer는 archive SHA-256, 내부 실행파일 SHA-256, 버전과 target을 확인하고
`~/.local/bin`에 설치합니다. `--prefix /path/to/prefix`로 목적지를 바꾸거나
기존 일반 실행파일을 교체할 때 `--force`를 사용하세요. root 권한은 필요하지 않습니다.
셸 시작 파일과 PATH는 자동 변경하지 않습니다. 저장소 릴리스가 신뢰 경계이며
checksum은 손상 검출용이고, 독립 서명이나 공급자 인증을 의미하지 않습니다.

Python 없이 설치할 때는 target archive와 `SHA256SUMS.txt`를 내려받아 체크섬을
확인한 후 `lens`, `lens-tui`, `lens-mcp`를 PATH의 디렉터리에 복사하세요.
`BUILD.json`에는 빌드 source SHA, target, Rust 버전과 실행파일별 digest가 있습니다.

## 폐쇄망 전달

연결된 환경에서 같은 release의 다음 파일을 받습니다.

- `lens-v0.10.0-x86_64-unknown-linux-musl.tar.gz`
- `SHA256SUMS.txt`
- `install.py`

대상 환경으로 전달한 뒤 네트워크 없이 실행합니다.

```sh
python3 install.py --version v0.10.0 \
  --archive lens-v0.10.0-x86_64-unknown-linux-musl.tar.gz \
  --checksums SHA256SUMS.txt --prefix "$HOME/.local"
```

## 지원표

| 환경/기능 | 범위 |
|---|---|
| Linux x86_64 | 공식 musl artifact, CLI/TUI/MCP 및 Linux 수집 |
| WSL2 Linux x86_64 | Linux CLI 실행; 이번 로컬 검증 환경. Windows-native 실행파일은 아님 |
| Linux container | namespace와 mount/권한 범위 내 수집. systemd/procfs가 없으면 해당 capability만 unavailable |
| Linux GNU source build | 기존 Cargo 빌드 경로 유지, glibc 버전 의존 |
| 다른 CPU, macOS, native Windows | 이번 공개 artifact/동등 지원 대상 아님. POSIX backend 분리만으로 지원을 주장하지 않음 |
| ELF | 기존 ABI 분석 유지. PE/Mach-O는 미지원 |
| systemd | 정적 system/user manager 부분집합. D-Bus나 서비스 activation 제어 없음 |
| journal/docker/podman | 사용자가 선택한 외부 CLI adapter. 해당 실행파일·권한·runtime은 별도 필요 |
| 폐쇄망 | archive/checksum 전달 설치와 파일 분석 가능; 외부 CLI 수집은 현장 capability에 따름 |

실제 시험 결과와 공개 artifact 검증은 [진행 원장](GENERALIZATION.md)에 기록합니다.
미실행 환경은 PASS로 표기하지 않습니다. 다른 OS의 안전한 파일 identity와 nonblocking
open backend, PE/Mach-O 의미론, compiler 전처리의 정확한 영향 분석은 별도 작업입니다.
이번 릴리스에서 그 안전 계약을 약화하는 임시 호환 backend는 추가하지 않았습니다.
