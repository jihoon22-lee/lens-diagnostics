# Lens Diagnostics 개발 규약

Linux 시스템·개발 산출물을 검사하고 비교하는 독립 Rust CLI/TUI/MCP 도구다.
현재 사용법은 README.md와 docs/GUIDE.md, 지원 경계는 docs/INSTALL.md, 구조는 crates/README.md를 따른다.

- 현재 9개 도메인·기존 JSON·lens.result v1·세 실행파일을 유지한다. 새 runner/schema/daemon이나
  언어 lint 플랫폼은 추가하지 않는다. 실제 사용 문제와 회귀 근거 없이 범위를 늘리지 않는다.
- complete, findings, 실제 process exit, MCP pagination은 서로 다른 사실이다. 미관측·취소·실패를 성공으로 바꾸지 않는다.
- 사용자 파일·설정·기존 공개 tag/asset을 보존한다. cleanup은 확인한 생성물과 이번 작업 자원으로 한정한다.
- 변경에 맞는 회귀, cargo fmt/clippy/test와 .github/scripts/check_docs.py를 실행한다.
  문서만 바뀌면 새 동작 테스트를 만들지 않는다. 배포는 기존 CI/release gate와 공개 다운로드 검증을 따른다.
- 버전은 Cargo.toml·Cargo.lock·.release-please-manifest.json·CHANGELOG·설치 예시를 함께 갱신한다.
  docs/DEVELOPMENT.md의 PR·릴리스 절차를 따른다. 이미 승인된 게시를 다시 승인받지 않는다.
- workthrough에 실제 변경·검증·미실행을 기록한다. 역사적 계획을 현재 의무로 취급하지 않는다.
