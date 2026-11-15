# 2026-11-15 유지보수 릴리스

사용자 승인: Lens와 idk의 정리·검증·버전 태그·공개 릴리스·소유 작업 자원 정리. devbox는 제외.

## 변경

- 종료된 ici 연결 안내와 근거 없는 성능 비교·표현 제거.
- ROADMAP을 현재 14개 crate, 18개 MCP 도구, 스캔 취소·단일 워커 구현에 맞춤.
- 기존 9개 도메인·세 실행파일·JSON/result 계약 유지. TUI와 CLI/MCP의 판정 경계 문서화.
- 설치 버전·폴더명·종료 기록 정리, 현재 작업에 적용할 AGENTS 추가.
- v0.11.1: 동작·schema 변경 없는 문서 유지보수 패치. 의존성 갱신 없음.

## 공개 전 검증

Linux/WSL에서 cargo fmt, locked Clippy(all targets, warnings deny), locked workspace tests 통과.
문서·schema·버전 일치 검사와 release flow 6개·installer 2개 검사 통과.
공개는 PR 필수 CI 통과·merge 후 기존 release workflow가 정확한 merge SHA로 수행한다.
공개 artifact는 새로 다운로드해 checksum·BUILD source/target/version·실행을 확인한다.

## 보존과 정리 범위

기존 태그·release·사용자 데이터는 보존한다. 설치·릴리스 체계 교체, TUI 통합과 배포 진입점 제거는 하지 않는다.
릴리스 완료 후 소유 release branch와 임시 다운로드·검사 도구, 재생성 가능한 저장소 target을 정리한다.
devbox 공유 Cargo cache는 건드리지 않는다. 공개 결과·최종 SHA와 정리 완료는 최종 인계에 기록한다.
