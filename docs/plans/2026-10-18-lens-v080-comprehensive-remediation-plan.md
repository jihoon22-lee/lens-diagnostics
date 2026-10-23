# Lens v0.8.0 전수 코드 리뷰 결함 수정·리팩토링·기능 고도화 상세 실행 계획서 (2026-10-18)

이 문서는 v0.8.0 당시의 실행 기록이다. 후속 리뷰에서 확인한 systemd specifier·
의존성 리셋, 마스킹, 버전·ABI 판정 등의 정정 사항은
[v0.9.0 수정 기록](../../workthrough/2026-10-23-lens-090-review-remediation.md)을 따른다.

- **대상 워크스페이스**: `/home/jihoon/projects/toy-projects` (`lens` v0.7.0, 14개 Rust 크레이트, ~28.4k LOC)
- **목적**: 별도 작업 세션에서 사전 컨텍스트 없이도 **전 항목(버그 86건 + 사용성 개선 + 도메인 기능 고도화 + 아키텍처/의존성 리팩토링)**을 순차적·완결적으로 구현 및 검증할 수 있도록 코드 수준의 수정 명세와 테스트 완료 기준(DoD)을 정의한다.

---

## 🚀 새 작업 세션 시작용 프롬프트 (Copy & Paste)

새 세션에서 작업을 시작할 때 아래 프롬프트를 그대로 입력하면 된다:

```text
docs/plans/2026-10-18-lens-v080-comprehensive-remediation-plan.md 문서를 읽고, Phase 1부터 순서대로 모든 항목을 빠짐없이 구현하고 테스트까지 완료해줘. 각 Phase가 끝날 때마다 필수 검증 게이트(fmt, clippy, test)를 통과시키고 체크리스트를 업데이트하면서 진행해.
```

---

## 0. 전역 설계 불변조건 및 필수 검증 게이트 (Quality Gates)

모든 코드 수정과 리팩토링은 다음 4대 불변조건을 절대 위반해서는 안 된다:

1. **Fail-Closed (안전 실패)**: 손상된 입력, 잘린 스트림, 리소스 한도 초과, 알 수 없는 스키마, 권한 거부 시 조용히 `0`이나 빈 결과를 반환하지 않고 명시적 에러(`LensError`) 또는 `complete: false` + `diagnostics`를 기록한다.
2. **Evidence-Bound (증거 기반)**: 모든 경고/실패 판정에는 파일 경로, 라인 번호, 심볼명, 소켓 inode, 시스템콜 인자 등 검증 가능한 증거(Evidence)가 포함되어야 하며, 추측성 라벨링(예: `OwnerUnknown`을 `<orphan>`으로 표시)을 금지한다.
3. **Deterministic Output (결정론적 출력)**: 동일한 입력에 대해 실행 시각이나 디렉터리/해시맵 순회 순서와 무관하게 바이트 단위로 동일한 JSON 출력과 SHA-256 해시를 보장한다.
4. **Read-Only & Sandbox Safety**: `lens disk trash`를 제외한 모든 명령은 대상 시스템을 변경하지 않으며, `--allow-root`가 설정된 경우 모든 파일/디렉터리/심볼릭 링크 접근은 `lens_core::read_policy`를 통과해야 한다(외부 파일 존재 유무 Oracle 포함 차단).

### 각 Phase 완료 시 필수 실행 명령어
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

---

## 🗺️ 전체 실행 로드맵 (6개 Phase / 독립 PR 단위)

의존성 그래프(`lens-core` → 도메인 크레이트 → `lens-ops` → `lens-cli` / `lens-tui` / `lens-mcp`)의 하위 계층부터 상위 계층 순으로 6개 Phase로 나누어 진행한다.

| Phase | 대상 크레이트 | 핵심 목표 | 포함 항목 |
| :--- | :--- | :--- | :--- |
| **Phase 1** | `lens-core` | 코어 번들/입출력/샌드박스 결함 수정 및 직렬화·글로브 엔진 리팩토링 | `C01`~`C08`, `R01`~`R03` |
| **Phase 2** | `lens-disk`, `lens-abi`, `lens-log` | 스토리지·바이너리·로그 분석기 치명적 버그 수정 및 심화 기능 구현 | `D01`~`D07`, `A01`~`A07`, `L01`~`L08`, `E01`~`E04` |
| **Phase 3** | `lens-test`, `lens-trace`, `lens-sys`, `lens-build`, `lens-env`, `lens-net` | 6대 도메인 파서/분석기 결함 전수 수정 및 의존성(`pep508_rs` 등) 현대화 | `T01`~`T04`, `TR01`~`TR05`, `S01`~`S05`, `B01`~`B05`, `EN01`~`EN05`, `N01`~`N05`, `E05`~`E09` |
| **Phase 4** | `lens-ops`, `lens-cli` | `ops.rs` 모듈 해체, `doctor`/`detect`/`follow` 수정, Zero-Config & 통합 `diff` UX | `O01`~`O08`, `U01`~`U06`, `R04`~`R05` |
| **Phase 5** | `lens-tui`, `lens-mcp` | TUI 비동기 아키텍처 전환·버그 수정, MCP 보안 Oracle 차단·페이지네이션 완성 | `TU01`~`TU06`, `M01`~`M07`, `U07`~`U09`, `R06` |
| **Phase 6** | 전 크레이트 통합 | 번들 PII 마스킹(`--redact`), `bundle diff` 고도화, `.lens.toml` 정책 엔진 | `E10`~`E12`, `R07` |

---

## Phase 1: `lens-core` 기반 안정화 및 코어 리팩토링

### 1.1 버그 및 보안 결함 수정 (`C01` ~ `C08`)

- [x] **`C01` [Critical] `extract_bundle`에서 `dest_dir`이 `"."` 또는 `"/"`일 때 100% 실패하는 버그**
  - **파일**: `crates/lens-core/src/bundle.rs:678-685, 751-767` (`extract_bundle`, `normalize_path_str`)
  - **원인**: `normalize_path_str(Path::new("."))`가 `Component::CurDir`를 건너뛰어 `""`을 반환하고, `format!("{normalized_dest}/")`가 `"/"`가 되어 상대 경로인 `normalized_target`이 `starts_with("/")` 검사에서 무조건 실패함. `dest_dir = "/"`일 때는 `"//"` 프리픽스를 요구하여 실패함. 또한 `fs::create_dir_all(dest_dir)` 전에 `dest_dir` 자체가 심볼릭 링크인지 확인하지 않음.
  - **수정 명세**:
    1. `extract_bundle` 진입 시 `dest_dir`이 이미 존재하면 `fs::symlink_metadata(dest_dir)`로 심볼릭 링크 여부를 먼저 검사하여 심볼릭 링크면 `LensError::InvalidBundle("Destination directory cannot be a symlink")` 반환.
    2. `fs::create_dir_all(dest_dir)` 수행 후 `let canonical_dest = dest_dir.canonicalize()?;`로 절대 경로를 확보한 뒤, 각 엔트리의 상대 경로 컴포넌트(`Component::Normal`만 허용, `ParentDir`/`RootDir`/`Prefix` 거부)를 `canonical_dest`에 결합하여 경로 탈출을 원천 차단.
  - **테스트**: `extract_bundle`에 `Path::new(".")`(임시 작업 디렉터리) 및 하위 상대 경로(`"sub/./dir"`)를 전달하는 단위 테스트 추가.

- [x] **`C02` [Critical] `LimitedReader`와 `stream_entries`의 압축 손상 vs 한도 초과 에러 오분류**
  - **파일**: `crates/lens-core/src/bundle.rs:172-194, 309-320, 510-514`
  - **원인**: `LimitedReader::read`가 바이트 한도 초과 시 `io::ErrorKind::InvalidData`를 반환하고, `stream_entries`는 `flate2`/`tar`가 발생시키는 모든 `InvalidData`(CRC 오류, 잘못된 헤더)를 `LensError::LimitExceeded`("decompression bomb")로 오분류함. 반대로 `on_entry` 내부에서 발생한 `LensError::LimitExceeded`는 `io::Error::other`로 감싸져 `LensError::Io`로 둔갑함.
  - **수정 명세**:
    1. `LimitedReader<R>` 내부에 `exceeded: Arc<AtomicBool>` 필드를 추가하고 한도 초과 시 `self.exceeded.store(true, Ordering::SeqCst)` 설정.
    2. `stream_entries`의 콜백 시그니처를 `FnMut(&tar::Header, &str, &mut tar::Entry<...>) -> Result<()>` (즉 `lens_core::error::Result`)로 변경하여 `LensError`를 손실 없이 그대로 전파.
    3. `io::Error` 변환 시 `exceeded.load(Ordering::SeqCst)`가 `true`일 때만 `LensError::LimitExceeded`를 반환하고, 일반 `InvalidData`/`UnexpectedEof`는 `LensError::InvalidBundle`로 매핑.
  - **테스트**: (a) 깨진 gzip 바이트 입력 시 `InvalidBundle` 반환 확인, (b) 실제 크기 초과 입력 시 `LimitExceeded` 반환 확인.

- [x] **`C03` [High] `stream_entries`, `evaluate_bundle_contents`, `read_bundle_entry`의 무결성 검증 구멍**
  - **파일**: `crates/lens-core/src/bundle.rs:327-330, 416-449, 530-552`
  - **원인**:
    1. `stream_entries`가 `header.entry_type().is_file()`이 아닌 엔트리(tar 내부에 심어둔 심볼릭 링크, 하드링크, 디바이스 노드, FIFO)를 에러 없이 조용히 건너뜀 (`!is_file() && !is_dir()`이면 즉시 `InvalidBundle`로 거부해야 함).
    2. `evaluate_bundle_contents`가 `manifest.sources` 내 중복 `bundle_path`나 `Source.size != actual_bytes_read` 불일치를 검사하지 않음.
    3. `read_bundle_entry`(`bundle show`)가 해시 검증 없이 첫 번째 일치하는 엔트리를 바로 반환하고 `return Ok(())`로 조기 종료하여 뒤에 오는 중복 덮어쓰기 엔트리를 무시함.
  - **수정 명세**:
    1. `stream_entries`에서 `EntryType::Regular`와 `EntryType::Directory` 외의 모든 엔트리 타입 발견 시 즉시 `LensError::InvalidBundle` 반환.
    2. `evaluate_bundle_contents`에서 `manifest.sources`의 `bundle_path` 중복 여부와 `source.size == actual_size` 일치 여부를 검증.
    3. `read_bundle_entry`도 먼저 `manifest.json`의 기대 SHA-256과 크기를 확인하고 전체 아카이브 스트림을 끝까지 스캔하여 중복 엔트리가 없고 해시가 일치할 때만 바이트를 반환.

- [x] **`C04` [High] `place_staged`의 비원자적 파일 교체**
  - **파일**: `crates/lens-core/src/bundle.rs:732-749`
  - **원인**: `target.exists()`일 때 `fs::remove_file(target)`을 먼저 호출한 뒤 `fs::rename(&self.temp_path, target)`을 호출하여 중간 실패 시 기존 파일이 유실됨.
  - **수정 명세**: `target`이 디렉터리가 아닌 경우 `remove_file` 없이 곧바로 `fs::rename(&self.temp_path, target)`을 호출하여 POSIX 원자적 교체 보장을 활용.

- [x] **`C05` [High] `SafeInput::open`의 FIFO(Named Pipe) 무한 블로킹 및 `FileIdentity::is_same_file`의 `(0, 0)` 충돌**
  - **파일**: `crates/lens-core/src/identity.rs:53-82`
  - **원인**: `File::open(path)`을 먼저 호출한 후 `metadata()`를 확인하므로, 대상이 writer가 없는 FIFO일 경우 `open(2)` 시스템콜에서 영구 블로킹됨. 또한 `FileIdentity { dev: 0, ino: 0, .. }`끼리 `is_same_file` 호출 시 `true`를 반환함.
  - **수정 명세**:
    1. `SafeInput::open` 내부 첫 단계에서 `crate::read_policy::check(path)?`를 호출하여 정책 우회를 차단.
    2. Unix 환경(`#[cfg(unix)]`)에서는 `std::os::unix::fs::OpenOptionsExt::custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)`로 파일을 연 뒤 `file.metadata()`로 `is_file()`(`S_IFREG`)을 확인하고, 일반 파일이 아니면 즉시 닫고 에러 반환(일반 파일에서는 `O_NONBLOCK`이 정규 I/O에 영향을 주지 않거나 `fcntl`로 해제 가능).
    3. `FileIdentity::is_same_file`을 `(self.dev != 0 || self.ino != 0) && self.dev == other.dev && self.ino == other.ino`로 수정.
  - **테스트**: `mkfifo`로 생성한 파이프에 `SafeInput::open` 호출 시 블로킹 없이 즉시 `LensError::NotARegularFile` 반환 확인.

- [x] **`C06` [High] `read_policy`의 루트 미정규화, 단일 컴포넌트 상대 경로 `""` 버그, 전역 `DENIAL` 경쟁 조건**
  - **파일**: `crates/lens-core/src/read_policy.rs:11-81`, `crates/lens-core/src/fs.rs:15`
  - **원인**:
    1. `set_roots`가 전달받은 루트 경로들을 `canonicalize`하지 않고 저장함.
    2. `check_ancestor`에서 존재하지 않는 상대 경로 `"missing.txt"`를 `pop()`하면 `cur`가 `Path::new("")`가 되어 `canonicalize("")`가 `ENOENT`로 실패함.
    3. 심볼릭 링크가 허용 루트 외부의 존재하지 않는 파일을 가리킬 때(Dangling Symlink), `check_ancestor`가 심볼릭 링크 자체의 부모 디렉터리만 확인하고 통과시킴.
    4. `DENIAL: Mutex<Option<String>>`이 프로세스 전역이라 멀티스레드 스캔 시 경합 발생.
    5. `digest_file`(`fs.rs:15`)이 `read_policy::check` 없이 `File::open`을 호출함.
  - **수정 명세**:
    1. `set_roots`에서 각 루트를 `canonicalize`(실패 시 `std::path::absolute`)하여 저장.
    2. `check_ancestor`에서 `cur.as_os_str().is_empty()`이면 `Path::new(".")`를 사용하도록 수정.
    3. 대상 경로 또는 조상 경로에 심볼릭 링크가 포함된 경우 `fs::read_link`를 따라가며 최종 타깃(존재하지 않는 타깃의 부모 포함)이 허용 루트 내부인지 재귀적으로 검증(최대 40회 링크 추적, 초과 시 `ELOOP` 거부).
    4. `digest_file` 및 `bundle.rs`의 파일 열기 경로에 `read_policy::check` 적용.
  - **테스트**: 허용 루트 안의 dangling symlink `bad -> /etc/nonexistent_secret`에 대해 `read_policy::check`가 존재하는 외부 파일과 동일한 `PermissionDenied` 에러를 반환하는지 검증.

### 1.2 코어 리팩토링 (`R01` ~ `R03`)

- [x] **`R01` `serde_json`의 `preserve_order` 제거를 통한 Zero-Clone 결정론적 JSON 직렬화**
  - **파일**: `Cargo.toml:25`, `crates/lens-core/src/json.rs:28, 43-70`
  - **수정 명세**:
    1. 루트 `Cargo.toml`의 `serde_json`에서 `features = ["preserve_order"]`를 제거하여 `serde_json::Map`이 기본적으로 `BTreeMap`(알파벳순 정렬)을 사용하도록 변경.
    2. `to_bounded_vec`에서 `Vec::with_capacity(limit)`(예: 1 MiB 선행 할당)를 `Vec::with_capacity(limit.min(4096))`로 변경하여 소형 JSON 직렬화 시 메모리 낭비 제거.
    3. `to_deterministic_pretty`와 `to_bounded_deterministic_pretty`에서 `serde_json::to_value` 후 `sort_json_keys`로 이중 복제하던 과정을 제거하고 곧바로 `serde_json::to_writer_pretty(&mut writer, value)`를 수행하도록 최적화(기존 출력 바이트와 100% 동일한지 테스트로 검증).

- [x] **`R02` 수제 `glob_match`를 `globset` 크레이트로 교체 (또는 제로 할당 바이트 매처로 개선)**
  - **파일**: `crates/lens-core/src/glob.rs:1-45`
  - **수정 명세**:
    1. `glob_match`에서 매 호출마다 `pattern.chars().collect::<Vec<char>>()`를 2개씩 할당하던 코드를 제거하고, UTF-8 바이트 슬라이스 기반 DP/백트래킹 매처 또는 사전 컴파일된 `GlobSet` 구조체를 제공하여 디스크 스캔 시 힙 할당을 `0`으로 만든다.
    2. `[!a-z]` 부정 문자 클래스 및 `**` 경로 매칭을 지원한다.

---

## Phase 2: 스토리지·바이너리·로그 도메인(`lens-disk`, `lens-abi`, `lens-log`) 수정 및 고도화

### 2.1 `lens-disk` 결함 수정 (`D01` ~ `D07`)

- [x] **`D01` [Critical] `extract_metadata`에서 `modified_ns`에 Epoch 초(seconds)가 누락된 버그**
  - **파일**: `crates/lens-disk/src/scanner.rs:346`
  - **원인**: `modified_ns: meta.mtime_nsec()`만 저장하여 `meta.mtime()`(초 단위)이 버려지고 `0..999,999,999` 나노초 소수부만 저장됨.
  - **수정 명세**:
    ```rust
    modified_ns: meta
        .mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec()),
    ```
  - **테스트**: 실제 파일 생성 후 `scan` 결과의 `modified_ns`가 `1_700_000_000_000_000_000` 이상인지 검증.

- [x] **`D02` [Critical] 휴지통 복원(`restore_by_name`)의 Dotfile 거부 및 `.trashinfo` 접미사 손상 버그**
  - **파일**: `crates/lens-disk/src/trash.rs:222, 252, 513`
  - **원인**: `name.starts_with('.')`로 인해 `.env`, `.bashrc` 등 모든 숨김 파일의 복원이 차단됨. 또한 `trim_end_matches(".trashinfo")`가 접미사 문자열이 아닌 문자 집합(`.`, `t`, `r`, `a`, `s`, `h`, `i`, `n`, `f`, `o`)을 끝에서 모두 깎아내어 파일명을 훼손함.
  - **수정 명세**:
    1. `trash.rs:252`의 경로 검증을 `name == "." || name == ".." || name.contains('/') || name.contains('\\') || name.contains('\0')`로 변경하여 정상 dotfile 복원을 허용.
    2. `trash.rs:222` 및 `513`의 `trim_end_matches(".trashinfo")`를 `strip_suffix(".trashinfo")`로 교체.
  - **테스트**: `.env` 및 `report.info` 파일을 휴지통으로 이동 후 `list` 및 `restore_by_name`이 정확히 원래 이름으로 복원하는지 테스트.

- [x] **`D03` [Critical] `SnapshotNodeV2` 재귀 구조로 인한 깊은 디렉터리(>60단계) `serde_json` 재귀 한계 패닉**
  - **파일**: `crates/lens-disk/src/snapshot.rs:20-31, 80-207`
  - **원인**: `SnapshotNodeV2`가 자기 참조 `children: Vec<SnapshotNodeV2>`를 가져 깊이 64 이상의 디렉터리 트리에서 `serde_json`의 128단계 재귀 한계에 걸려 직렬화/역직렬화가 실패함.
  - **수정 명세**:
    1. `SnapshotV2`에 플랫 노드 배열 또는 커스텀 비재귀(Iterative) `Serialize`/`Deserialize` 구현을 적용하거나, `serde_json::Deserializer::disable_recursion_limit`와 비재귀 직렬화기를 사용하여 `max_depth`(512)까지 안전하게 직렬화/역직렬화되도록 수정.
  - **테스트**: 깊이 100단계의 합성 `ScanResult`를 생성하여 `SnapshotV2::from_scan` → JSON 직렬화 → 역직렬화가 에러 없이 통과하는지 검증.

- [x] **`D04` [High] `find_duplicates`에서 `min_size == 0`일 때 FIFO 블로킹 및 불필요한 하드링크/소형 파일 이중 해싱**
  - **파일**: `crates/lens-disk/src/duplicates.rs:50-138`
  - **수정 명세**:
    1. Step 1 필터에서 `if entry.kind != EntryKind::RegularFile || entry.size < self.min_size { continue; }`로 일반 파일만 수집.
    2. Step 1 종료 후 각 크기 버킷에서 고유 `(dev, ino)` 개수가 2개 미만인 버킷(단일 파일의 하드링크들로만 구성된 버킷)은 Step 2(부분 해싱) 전에 즉시 제외.
    3. 파일 크기가 `PARTIAL_HASH_BYTES`(`4096`) 이하인 경우 Step 2의 해시가 곧 전체 파일 해시이므로 Step 3에서 파일을 다시 열지 않고 Step 2 해시를 그대로 재사용.

- [x] **`D05` [Medium] `DiskScanner`의 심볼릭 링크 디렉터리 `is_dir = true` 오분류, 역순(LIFO) 스캔, `max_entries` 절단 시 `complete = true` 누수**
  - **파일**: `crates/lens-disk/src/scanner.rs:227-305, 355`
  - **수정 명세**:
    1. `extract_metadata`에서 `let is_dir = !is_symlink && meta.is_dir();`로 변경하여 따라가지 않는 심볼릭 링크가 `is_dir = true, size = 0`으로 집계되지 않게 수정.
    2. LIFO `stack`에 자식 디렉터리를 넣을 때 `for child in children.into_iter().rev()`로 역순 푸시하여 실제 방문 순서가 사전순(`a -> z`)이 되도록 보장.
    3. 루프 종료 후 `stack`에 아직 방문하지 못한 디렉터리 프레임이 남아 있다면(즉 `max_entries`에 의해 조기 중단된 경우), 스택에 남은 모든 부모 경로들의 `complete` 플래그를 `false`로 마킹하고 `diagnostics`에 `"Scan truncated at max_entries limit"`을 추가.

- [x] **`D06` [Medium] `TrashManager`의 `.Trash-$uid` 심볼릭 링크/소유권 검증 및 `.lens.json` 사이드카 충돌 방지**
  - **파일**: `crates/lens-disk/src/trash.rs:124-193, 297-369`
  - **수정 명세**:
    1. FreeDesktop Trash 스펙에 따라 `.Trash-$uid` 디렉터리가 심볼릭 링크가 아니고 현재 `euid` 소유이며 모드 `0700`인지 확인.
    2. `move_to_trash_inner`에서 충돌 검사 시 `files/<name>`, `info/<name>.trashinfo`뿐만 아니라 `info/<name>.lens.json` 존재 여부도 함께 확인하고, `write_identity` 실패 시 경고를 기록.

---

### 2.2 `lens-abi` 결함 수정 및 DWARF 고도화 (`A01` ~ `A07`, `E01` ~ `E02`)

- [x] **`A01` [Critical] Undefined(임포트) 심볼이 Defined(익스포트)로 바뀔 때 `Incompatible`로 오판하는 버그**
  - **파일**: `crates/lens-abi/src/diff.rs:87-123`
  - **원인**: `left_evidence_map`과 `right_evidence_map` 생성 시 `s.defined` 필터 없이 미정의 임포트 심볼(`defined: false, symbol_type: "NOTYPE"`)까지 포함시켜, 새 버전에서 해당 심볼을 직접 구현/익스포트(`defined: true, symbol_type: "FUNC"`)하면 `symbol_type` 변경으로 오인해 `Verdict::Incompatible`을 반환함.
  - **수정 명세**: `left_evidence_map`과 `right_evidence_map` 모두 `.filter(|s| s.defined)`를 적용하여 정의된 익스포트 심볼끼리만 비교하도록 수정.
  - **테스트**: baseline에서 `defined: false, symbol_type: "NOTYPE"`이던 심볼이 candidate에서 `defined: true, symbol_type: "FUNC"`가 되었을 때 `Added`로 분류되고 `Compatible` 판정이 나오는지 검증.

- [x] **`A02` [High] 기형 ELF 헤더의 정수 오버플로 패닉 방지 및 `.gnu.version` / Visibility / `STT_TLS` 정확화**
  - **파일**: `crates/lens-abi/src/elf.rs:141-156, 195-199, 282-333, 514-556`
  - **수정 명세**:
    1. `phoff + i * phentsize`, `off + size` 등 모든 ELF 오프셋 계산을 `checked_mul` / `checked_add`로 변경하고 오버플로 시 `LensError::ParseError` 반환.
    2. `st_other & 0x03`을 읽어 `0 => "default"`, `1 => "internal"`, `2 => "hidden"`, `3 => "protected"`로 `visibility`를 정확히 설정하고, `exported_functions`에는 `defined && (binding == "GLOBAL" || binding == "WEAK") && (visibility == "default" || visibility == "protected")`인 심볼만 포함.
    3. `STT_TLS`를 `"TLS"` 타입으로 매핑(크기 변경 시 ABI 비호환 감지).
    4. `SHT_GNU_versym`(`.gnu.version`) 섹션을 파싱하여 각 동적 심볼의 실제 버전 이름(`@@GLIBCXX_3.4` 등)과 hidden bit(`0x8000`)를 연결하고, `parse_verdef`에서 `VER_FLG_BASE`(`vd_flags & 1 != 0`) 엔트리와 부모 `Verdaux` 엔트리를 구분.

- [x] **`A03` [High] `diff_reports`에서 `DT_SONAME`, `PT_INTERP` 변경 감지 및 `verdef`(익스포트 버전) vs `verneed`(임포트 버전) 분리**
  - **파일**: `crates/lens-abi/src/diff.rs:46-85, 138-177`
  - **수정 명세**:
    1. `left.soname != right.soname`일 때 `Verdict::Incompatible` 및 `soname_changed` 증거를 기록.
    2. `namespace == "self"`인 자체 익스포트 심볼 버전(`verdef`)이 제거된 경우는 `Verdict::Incompatible`로, 외부 라이브러리 요구 버전(`verneed`)이 상향된 경우는 `Verdict::Uncertain`으로 구분.

- [x] **`A04` & `E01` [High / Enhancement] `.zdebug_info` 폴백 버그 수정 및 DWARF 구조체 크기·필드 오프셋·함수 시그니처 추출**
  - **파일**: `crates/lens-abi/src/dwarf.rs:19-115`, `crates/lens-abi/src/diff.rs`
  - **수정 명세**:
    1. `dwarf.rs:23`의 `if obj_file.section_by_name(".debug_info").is_none()` 가드를 `obj_file.section_by_name(".debug_info").is_none() && obj_file.section_by_name(".zdebug_info").is_none()`으로 수정.
    2. `DwarfSummary`를 확장하여 단순 타입 이름 목록뿐만 아니라 구조체/공용체의 바이트 크기(`DW_AT_byte_size`)와 멤버 필드(`DW_TAG_member`의 이름 및 `DW_AT_data_member_location` 오프셋), 함수(`DW_TAG_subprogram`)의 인자 개수를 추출하고, `diff_reports`에서 익스포트된 타입의 크기나 필드 오프셋이 변경되면 `Verdict::Incompatible`(Layout Breakage)로 보고.

---

### 2.3 `lens-log` 결함 수정 및 멀티라인/클러스터링 고도화 (`L01` ~ `L08`, `E03` ~ `E04`)

- [x] **`L01` [Critical] 0바이트 빈 로그 파일에서 `LogIndexer::open`이 `mmap(EINVAL)`로 실패하는 버그**
  - **파일**: `crates/lens-log/src/indexer.rs:46-56`
  - **수정 명세**: `safe_input.metadata.len() == 0`일 때 `safe_input.mmap()`을 호출하지 않고 빈 버퍼(`MmapOrEmpty::Empty`)와 빈 `lines: Vec::new()`를 가진 `LogIndex`를 즉시 반환.
  - **테스트**: `NamedTempFile::new()`(0바이트)에 대해 `LogIndexer::open`, `compute_stats`, `FilterEngine`이 에러 없이 0건 결과를 반환하는지 테스트.

- [x] **`L02` [Critical] `--source` 필터 사용 시 `record.source`가 빈 문자열인 모든 로그가 100% 통과하는 버그**
  - **파일**: `crates/lens-log/src/filter.rs:218-224`, `crates/lens-log/src/parser.rs:115-160`
  - **수정 명세**:
    1. `filter.rs:220`의 `if !record.source.is_empty() && record.source != *src`에서 `!record.source.is_empty()` 조건을 제거하여 `if record.source != *src { return false; }`로 수정.
    2. `parser.rs`에서 비-JSONL Syslog 라인(`Oct  9 12:00:00 host app[123]: msg` 또는 `2026-10-09T12:00:00Z host app: msg`)을 파싱할 때 호스트명 뒤의 `app`(태그/프로세스명)을 추출하여 `record.source`에 채움.
    3. JSONL 파싱 시 `"source"`, `"logger"`, `"service"`, `"SYSLOG_IDENTIFIER"`, `"_COMM"` 키를 순서대로 확인하여 `source`에 매핑.

- [x] **`L03` [High] RFC 3164 Syslog `[ERROR]` 오프바이원 누락 및 일반 단어 레벨 오탐 수정**
  - **파일**: `crates/lens-log/src/parser.rs:162-201` (`detect_level`)
  - **수정 명세**:
    1. 토큰 스캔 범위를 `i < 5`에서 `i < 8`로 확장하되, 구분자 없는 일반 영단어(`"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"`, `"fatal"`)는 라인 맨 앞(`i == 0`)이거나 타임스탬프 직후 토큰일 때만 레벨로 인정하고, 본문 영역(`i >= 2` in syslog)에서는 `[ERROR]`, `ERROR:`, `<error>`, `level=error`, `lvl=error`처럼 명시적 구분자가 있는 경우만 레벨로 판정.

- [x] **`L04` [High] 공백 구분 ISO 타임스탬프 타임존 유실, `<PRI>` 프리픽스 미처리, Journald `__REALTIME_TIMESTAMP` 지원 및 `Timestamp::now()` 시스템콜 제거**
  - **파일**: `crates/lens-log/src/timestamp.rs:74-143`, `crates/lens-log/src/parser.rs:53-125`
  - **수정 명세**:
    1. `timestamp.rs:96-112`에서 `2026-10-09 12:00:00+09:00` 또는 `2026-10-09 12:00:00.123Z`처럼 공백 구분자 뒤에 타임존 오프셋/소수점 초가 이어지는 경우 인덱스 19에서 자르지 않고 첫 공백을 `'T'`로 치환해 전체 타임스탬프 토큰을 `parse_iso_str`에 전달.
    2. `text_prefix_ms` 시작부에서 `<134>` 형태의 Syslog `<PRI>` 프리픽스(`^<\d{1,3}>`)가 있으면 이를 건너뛴 뒤 타임스탬프를 파싱.
    3. JSONL 타임스탬프 추출 시 systemd journald의 `"__REALTIME_TIMESTAMP"`(문자열 또는 정수 마이크로초)를 지원.
    4. `parse_line`에서 매 라인 호출하던 `jiff::Timestamp::now().to_zoned(...)`를 `std::sync::OnceLock<i16>` 캐시 또는 `FilterEngine`의 `syslog_year` 파라미터로 대체하여 시스템콜을 제거.

- [x] **`E03` [Enhancement] `SessionV2` 멀티라인 스택트레이스 폴딩(Folding) 실제 구현**
  - **파일**: `crates/lens-log/src/session.rs`, `crates/lens-log/src/filter.rs`, `crates/lens-log/src/stats.rs`
  - **수정 명세**: 타임스탬프가 없고 공백/탭이나 `at `, `Caused by:`, `File "`, `Traceback ` 등으로 시작하는 연속 라인들을 직전 로그 레코드의 부속 라인으로 폴딩하는 옵션을 구현하여, Python/Java/Rust 패닉 스택트레이스가 `--min-level error` 필터링 시 첫 줄만 남고 잘리는 문제를 해결.

---

## Phase 3: 6대 도메인 분석기(`test`, `trace`, `sys`, `build`, `env`, `net`) 결함 수정 및 고도화

### 3.1 `lens-test` (`T01` ~ `T04`)

- [x] **`T01` [Critical] `quick-xml` 0.41의 `Event::GeneralRef` 누락으로 인한 XML 엔티티 증발 및 공백 손실 수정**
  - **파일**: `crates/lens-test/src/junit.rs:9, 230-241, 346`
  - **원인**: `quick-xml` 0.41은 `&amp;`, `&lt;` 등을 `Event::Text`에서 분리해 `Event::GeneralRef`로 방출하는데, 이를 처리하지 않고 `trim_text(true)`를 켜두어 엔티티와 주변 공백이 모두 삭제됨.
  - **수정 명세**:
    1. `reader.config_mut().trim_text(false)`로 변경(최종 캡처 완료 시점에만 `.trim()` 수행).
    2. `Event::GeneralRef(e)` 가지를 추가하여 `e.resolve_char_ref()` 및 기본 XML 엔티티(`lt`→`<`, `gt`→`>`, `amp`→`&`, `apos`→`'`, `quot`→`"`)를 `cap.text`에 복원.
  - **테스트**: `<failure message="a &amp;&amp; b">expected x &lt; y &amp; z</failure>` 파싱 시 `"expected x < y & z"`가 정확히 보존되는지 검증.

- [x] **`T02` [High] 잘린(Truncated) XML의 `complete = true` 오판, `NaN` 시간 오염, 중복 테스트케이스 덮어쓰기 수정**
  - **파일**: `crates/lens-test/src/junit.rs:11, 78-80, 252-331`, `crates/lens-test/src/diff.rs:82-146`
  - **수정 명세**:
    1. `check_end_names = true`를 기본 활성화하거나 명시적 요소 스택(`Vec<String>`)으로 태그 짝을 검증하고, `</testcase>` 도달 시 미종료 `capture`가 있으면 강제 플러시 후 `complete = false` 기록.
    2. `parse::<f64>()` 후 `.filter(|v| v.is_finite() && *v >= 0.0)`를 적용하여 `NaN`/`inf`/음수 시간을 차단.
    3. `diff_test_runs`에서 동일 `(suite, classname, name)`을 가진 테스트케이스가 하나의 런 안에 여러 번 등장할 때(재시도/파라미터화 테스트), 실패 상태를 성공이 조용히 덮어쓰지 않도록 상태 전이(예: Pass + Fail 혼재 시 Flaky/Failed 보존)를 명시적으로 처리.

### 3.2 `lens-trace` (`TR01` ~ `TR05`, `E05`)

- [x] **`TR01` [Critical] `<unfinished ...>` 라인 때문에 멀티스레드 `strace -f` 로그가 파싱 거부되는 버그**
  - **파일**: `crates/lens-trace/src/lib.rs:34-49`, `crates/lens-trace/src/parser.rs:98-142`
  - **원인**: `<unfinished ...>`가 `total_events`만 올리고 `total_calls`는 올리지 않아 `unparsed = total_events - total_calls`가 모든 `<unfinished ...>` 라인을 파싱 실패로 계산함.
  - **수정 명세**: `TraceReport` 내부(또는 파서 반환 구조체)에 실제 파싱에 실패한 라인 수를 세는 `unparsed_lines: usize`와 미완료 상태로 끝난 호출 수를 세는 `unfinished_calls: usize`를 분리하여, `analyze_trace_file`은 오직 `unparsed_lines`를 기준으로만 유효성을 판별하도록 수정.
  - **테스트**: 10개의 `<unfinished ...>`와 `<... resumed>` 쌍, 그리고 트레이스 끝에 1개의 미완료 블로킹 `accept4 <unfinished ...>`가 있는 실제 `strace -f` 픽스처가 에러 없이 파싱되는지 검증.

- [x] **`TR02` [High] `parse_strace_body`의 과소 검증 및 `strace -y` 경로 내 `<` 문자에 의한 결과값 절단 수정**
  - **파일**: `crates/lens-trace/src/parser.rs:488-557`
  - **수정 명세**:
    1. `parse_strace_body`에서 시스템콜 이름이 `[a-zA-Z0-9_]+`로만 구성되어 있고, 닫는 괄호 뒤(또는 줄 끝)에 반드시 `= <return>` 패턴이 존재할 때만 유효한 시스템콜로 인정.
    2. `parse_return_and_duration`에서 `rfind('<')`가 가리키는 `<...>` 내부 문자열이 유효한 부동소수점 숫자(`f64`, 예: `<0.000123>`)일 때만 duration으로 분리하고, 그렇지 않으면(`strace -y`의 `3</path/with<angle>>`) 반환값의 일부로 보존.

- [x] **`TR03` [High] `close_range(CLOSE_RANGE_CLOEXEC)`, `dup2` 표준입출력 리다이렉션, `superseded by execve` FD 누수 오탐 수정**
  - **파일**: `crates/lens-trace/src/parser.rs:189-204, 240-275`
  - **수정 명세**:
    1. `close_range`의 세 번째 인자(`flags`)에 `CLOSE_RANGE_CLOEXEC`(`2` 또는 `0x2`)가 포함된 경우 즉시 닫지 않고 해당 범위 FD들의 `cloexec = true`만 설정. `CLOSE_RANGE_UNSHARE`(`1`)가 포함된 경우 공유 `FdTable`을 복제(unshare)한 뒤 닫기 수행.
    2. `dup2`/`dup3`/`fcntl(F_DUPFD)`로 표준 입출력 FD(`0`, `1`, `2`)에 복제된 디스크립터는 프로세스 종료 시 누수(`fd_leaks`)에서 제외.
    3. `+++ superseded by execve +++` 라인은 프로세스 종료가 아니므로 `exited_pids`에 추가하지 않음.

- [x] **`E05` [Enhancement] 시스템콜별 지연시간 퍼센타일(`p50`, `p95`, `p99`, `max`) 및 프로세스 트리 요약 추가**
  - **파일**: `crates/lens-trace/src/model.rs`, `crates/lens-trace/src/parser.rs`
  - **수정 명세**: `strace -T` 로그에서 수집된 시스템콜별 소요시간 분포로부터 `SyscallLatencyStats`(`count`, `total_sec`, `p50_us`, `p95_us`, `p99_us`, `max_us`)를 계산하여 느린 I/O 병목을 즉시 식별할 수 있도록 확장.

### 3.3 `lens-sys` (`S01` ~ `S05`, `E06`)

- [x] **`S01` [Critical] `diff_systemd`에서 유닛의 `masked` 상태 변경 및 `alias_of` 대상 변경 누락 수정**
  - **파일**: `crates/lens-sys/src/diff.rs:24-63`
  - **수정 명세**: `diff_systemd`에서 `sections`와 `drop_ins` 비교 전에 `l_unit.masked != r_unit.masked` 및 `l_unit.alias_of != r_unit.alias_of`를 비교하여, 변경 시 합성 변경 레코드(`section: "[Meta]"`, `key: "Masked"` / `"AliasOf"`)를 `changed_keys`에 추가.
  - **테스트**: 정상 유닛이 `/dev/null` 심볼릭 링크(`masked: true`)로 변경된 두 스냅샷을 `diff_systemd`로 비교했을 때 `modified_units`에 정확히 보고되는지 검증.

- [x] **`S02` [High] Drop-in의 `User=` 오버라이드 시 스페시파이어(`%u`, `%h`) 재확장, `%N` 접미사 제거, `.wants/` 디렉터리 및 템플릿(`foo@bar.service`) 지원**
  - **파일**: `crates/lens-sys/src/parser.rs:284-314, 361-407`, `crates/lens-sys/src/loader.rs:134-181, 353-399`, `crates/lens-sys/src/graph.rs`
  - **수정 명세**:
    1. 기본 유닛과 모든 Drop-in(`*.conf`)의 raw 섹션/키를 먼저 병합한 후, 최종 확정된 `User=` 값을 기준으로 `%u`, `%h`, `%n`, `%N`(확장자 제거), `%i`(인스턴스명) 스페시파이어를 한 번에 확장하도록 순서 변경.
    2. `load_units_merged`에서 `read_dir` 실패 시 에러를 삼키지 않고 `diagnostics`에 기록.
    3. 유닛 디렉터리 내 `*.target.wants/` 및 `*.target.requires/` 서브디렉터리의 심볼릭 링크를 스캔하여 타깃 유닛의 `Wants`/`Requires` 의존성에 반영하고, `foo@bar.service`가 참조될 때 디스크의 `foo@.service` 템플릿 유닛으로 폴백 해석.

### 3.4 `lens-build` (`B01` ~ `B05`)

- [x] **`B01` [Critical] `strip_comments`의 비-ASCII UTF-8 문자 모지바케(Mojibake) 손상 수정**
  - **파일**: `crates/lens-build/src/impact.rs:282-328`
  - **원인**: `out.push(bytes[i] as char)`가 UTF-8 멀티바이트를 1바이트씩 Latin-1 코드포인트로 쪼개 넣음.
  - **수정 명세**: ASCII 구분자(`//`, `/*`, `*/`, `"`, `'`, `\`)는 바이트 인덱스로 스캔하되, 출력 버퍼에는 유효한 UTF-8 슬라이스(`&line[start..end]`) 단위로 `out.push_str(...)`을 수행하도록 재작성.
  - **테스트**: 한글 주석과 유니코드 경로(`#include "한글헤더.h" // 주석`)가 포함된 소스 파일에서 `extract_includes`가 정확한 파일명을 추출하는지 검증.

- [x] **`B02` [Critical] 표준 라이브러리 헤더(`<stdio.h>`, `<vector>`)로 인한 100% `incomplete_graph` 경고 오탐 및 $O(\text{TU} \times \text{Headers})$ 중복 탐색 해결**
  - **파일**: `crates/lens-build/src/impact.rs:52-117, 173-195`, `crates/lens-ops/src/build.rs:157`
  - **수정 명세**:
    1. `extract_includes`가 반환하는 인클루드 정보를 `IncludeDirective { name: String, is_angled: bool }`로 구분.
    2. `#include <...>`(`is_angled: true`)가 로컬 `-I`/`-isystem` 경로에서 발견되지 않는 경우는 시스템 헤더로 간주하여 `unresolved_includes` 대신 `external_system_includes`로 별도 집계하고 `build.incomplete_graph` 경고를 유발하지 않도록 수정(`#include "..."`가 미해결일 때만 경고 발생).
    3. `find_header`에서 `candidate.is_file()` 대신 `lens_core::read_policy::metadata(&candidate)`를 사용하여 `--allow-root` 우회 탐색을 차단하고, `(parent_dir, include_dirs_hash, header_name)`을 키로 하는 메모이제이션 캐시를 도입하여 이미 분석한 헤더의 전이 인클루드 그래프를 재사용.

- [x] **`B03` [High] `diff_compilations`에서 `standard`(`-std=`), `compiler`, `forced_includes`, `-I` 순서 변경 감지 추가**
  - **파일**: `crates/lens-build/src/diff.rs:20-74`
  - **수정 명세**: `CompilationDiff`에 `standard_changed`, `compiler_changed`, `include_order_changed`, `forced_includes_changed`를 반영하여 `-std=c++17` → `-std=c++20` 변경 등이 누락되지 않도록 수정.

### 3.5 `lens-env` (`EN01` ~ `EN05`, `E08`)

- [x] **`EN01` [Critical] `Requires-Dist`의 괄호 감싼 버전 명시자(`urllib3 (>=1.21.1,<3)`) 파싱 실패 및 PEP 508/440 엣지 케이스 해결**
  - **파일**: `crates/lens-env/src/markers.rs:228-299, 403-503, 598-616, 669-692`
  - **수정 명세**:
    1. `parse_requirement` 및 `evaluate_version_specs`에서 `spec_part` 양끝의 소괄호 `(`와 `)`를 `.trim_start_matches('(').trim_end_matches(')').trim()`으로 제거한 뒤 쉼표 분리.
    2. `version_key`에서 PEP 440 Epoch(`1!2.0.0`)를 `(epoch: u64, release_segments: Vec<u64>, pre_release_rank)` 튜플로 파싱하여 Epoch와 프리릴리즈(`a`, `b`, `rc`, `dev`, `post`) 비교 정확도를 확보하고, `~= X.Y` 및 `===` 연산자와 `python_version in '3.10 3.11'` 문자열 포함 연산을 지원.
    3. `TriState::or`/`and`에서 한쪽이 `Error`이면 단락 평가(Short-circuit)로 에러를 삼키지 않고 `TriState::Error`를 전파하여 잘못된 마커 문법이 `unevaluated`로 보고되도록 수정.
  - **테스트**: `Requires-Dist: requests (>=2.25.0,<3.0.0); python_version in '3.10 3.11 3.12'`가 정상 평가되는지 검증.

- [x] **`EN02` [High] Editable Install(`pip install -e .` / `uv sync`) 자기 자신 Shadowing 오탐 및 `RECORD` 폴백의 `__pycache__` / `.so` 버그 수정**
  - **파일**: `crates/lens-env/src/shadowing.rs:244-294`, `crates/lens-env/src/venv.rs:182-220`
  - **수정 명세**:
    1. `read_top_level_modules`에서 `RECORD` 폴백 시 `"__pycache__"` 디렉터리를 제외하고, `foo.cpython-312-x86_64-linux-gnu.so`나 `foo.abi3.so`처럼 루트에 위치한 공유 라이브러리 파일은 첫 번째 `.` 앞의 모듈명(`"foo"`)을 추출하여 등록.
    2. `.dist-info/direct_url.json` 또는 `site-packages/*.pth`를 읽어 현재 `working_dir`에서 editable 설치된 패키지의 모듈들은 `InstalledPackage` shadowing 경고에서 제외.

- [x] **`E08` [Enhancement] `.venv` 내 네이티브 C 확장 모듈(`.so`)의 깨진 동적 라이브러리(`DT_NEEDED`) 감사**
  - **파일**: `crates/lens-env/src/venv.rs`, `crates/lens-ops/src/env.rs`
  - **수정 명세**: `.venv`의 `site-packages` 내 `.so` 파일들의 `DT_NEEDED` 및 `RPATH`/`RUNPATH`를 스캔하여 호스트 라이브러리 경로(`/lib`, `/usr/lib`, `/etc/ld.so.conf.d`, 패키지 내 `.libs/` 디렉터리)에서 찾을 수 없는 네이티브 라이브러리(예: `libpq.so.5`, `libcuda.so`)가 있으면 `broken_native_extensions`로 보고.

### 3.6 `lens-net` (`N01` ~ `N05`)

- [x] **`N01` [High] `/proc/<pid>/comm` 선행 읽기 제거, 공유 소켓 PID 결정론적 정렬, `SOCK_SEQPACKET` 및 Unix 소켓 `owner_state` 지원**
  - **파일**: `crates/lens-net/src/parser.rs:100-145, 156-341`, `crates/lens-net/src/diff.rs:15-89`
  - **수정 명세**:
    1. `scan_process_socket_inodes`에서 PID 목록을 오름차순 정렬(`pids.sort_unstable()`)한 뒤 순회하고, `/proc/<pid>/fd`를 먼저 열어 소켓 inode가 발견된 프로세스에 대해서만 `comm`과 `cmdline`을 지연 읽기(Lazy Read)하도록 변경. 이미 `inode_map`에 등록된 소켓은 가장 낮은 PID(마스터 프로세스)가 유지되도록 `inode_map.entry(inode).or_insert_with(...)` 사용.
    2. `scan_process_socket_inodes` 내부의 `fs::metadata(&pid_path)`와 `fs::read_link(fd_entry.path())`를 `lens_core::read_policy` 경유로 통일.
    3. `parse_proc_net_unix`에서 `0x0005`(`SOCK_SEQPACKET`)를 `SocketKind::UnixSeqPacket`(또는 Unix 계열)으로 처리하고, 라인에서 7번째 공백 컬럼 이후의 원문 슬라이스를 그대로 `unix_path`로 추출하여 다중 공백 보존.
    4. `NetReport`의 `sockets`와 `listening` 배열을 `(kind, local_address, local_port, remote_address, remote_port, inode)` 순으로 정렬하여 결정론적 JSON 출력 보장.

---

## Phase 4: `lens-ops` 및 `lens-cli` 리팩토링·결함 수정·사용성(UX) 혁신

### 4.1 `lens-ops` & `lens-cli` 버그 수정 (`O01` ~ `O08`)

- [x] **`O01` [Critical] `lens log filter --follow --format jsonl` 실행 불가 버그 및 로테이션 시 `line_no` 미초기화 수정**
  - **파일**: `crates/lens-cli/src/ops.rs:836-930`
  - **수정 명세**:
    1. `ops.rs:836`의 `if json_mode`를 `if matches!(format, OutputFormat::Json)`으로 변경하여 `--format jsonl`과 `--follow` 조합이 정상 동작하도록 수정.
    2. 폴링 루프 진입 직전에 `io::stdout().flush()`를 호출하고, 파일 로테이션이나 truncation이 감지되었을 때 `line_no = 0`으로 리셋.

- [x] **`O02` [High] `detect_input_kind`의 심볼릭 링크 디렉터리 `EISDIR` 에러, 자체 스냅샷 `Log` 오분류, 대용량 JSON OOM 방지**
  - **파일**: `crates/lens-ops/src/detect.rs:153-240`
  - **수정 명세**:
    1. `fs::symlink_metadata` 대신 `lens_core::read_policy::metadata(path)`(심볼릭 링크 추적)로 디렉터리 여부를 판별하여 디렉터리 심볼릭 링크가 `InputKind::Directory`로 분류되도록 수정.
    2. JSON 파일 스니핑 시 `serde_json::from_reader`로 파일 전체를 읽지 않고 앞 8 KiB 프리픽스 내에서 `"schema_version"` 또는 `"schema"` 필드를 추출하거나 `Take` 리더(최대 16 MiB)를 사용하며, `lens.disk.snapshot/v2`, `lens.doctor/v1`, `lens.build/v1`, `lens.test/v1`, `lens.trace/v1` 스키마도 각각 정확한 `InputKind`로 분류.

- [x] **`O03` [High] `lens doctor`의 소형 파티션(`/boot/efi`) 무조건 `FAIL`, 읽기 전용 마운트, Inode 메시지 오표시, `/etc/ld.so.preload` 주석/권한 오판 수정**
  - **파일**: `crates/lens-ops/src/doctor.rs:80-125, 214-229, 552, 618`
  - **수정 명세**:
    1. `check_mount_points`에서 마운트 옵션에 `ro`(읽기 전용, 예: `/snap`, 불변 루트)가 있는 파일시스템은 용량/inode 부족 경고에서 제외.
    2. `avail_gb < 1.0` 절대 기준 대신 **`used_pct > 95.0`이면서 `avail_gb < 1.0`(또는 파티션 전체 크기가 2GB 미만일 때는 `used_pct > 95.0`)**일 때만 `Fail`을 반환하여 512MB짜리 정상 `/boot/efi` 파티션에서 오탐이 나지 않도록 수정.
    3. Inode 고갈(`inode_pct > 95.0`)로 실패할 때는 `"Critically high inode usage ({inode_pct:.1}% used)"`로 정확한 원인 메시지를 출력.
    4. `check_ld_preload`에서 파일을 읽을 때 `read_policy::read_to_string`을 사용하고, `Err(e)`(권한 부족 등) 발생 시 `"is empty" (PASS)`가 아니라 `CheckStatus::Warn`(`"Could not read /etc/ld.so.preload: {e}"`)을 반환하며, `#`으로 시작하는 주석 라인은 엔트리에서 제외.
    5. `check_env_path_safety`에서 빈 항목(`""`, `.`)뿐만 아니라 `'/'`로 시작하지 않는 모든 상대 경로(`"bin"`, `"./scripts"`)를 `Warn`으로 탐지.

- [x] **`O04` [Medium] `lens net status --no-unix` 사용 시 `summary.total_sockets` 및 `unix_domain_sockets` 수치 불일치 수정**
  - **파일**: `crates/lens-ops/src/net.rs:11-33`
  - **수정 명세**: `!include_unix`일 때 `sockets` 필터링 후 `summary.total_sockets`와 `summary.unix_domain_sockets`도 필터링된 결과에 맞게 재집계.

### 4.2 CLI 아키텍처 리팩토링 및 사용성(UX) 개선 (`R04`, `U01` ~ `U04`)

- [x] **`R04` `crates/lens-cli/src/ops.rs` (2,175줄) 도메인별 서브모듈 분할**
  - **파일**: `crates/lens-cli/src/ops.rs` → `crates/lens-cli/src/commands/{mod,disk,abi,log,trace,sys,build,env,net,test,doctor,bundle}.rs`
  - **수정 명세**: 공통 출력 헬퍼(`OutputFormat`, `BrokenPipe` 안전 쓰기, `Outcome` 종료 코드 매핑)는 `commands/mod.rs`에 두고 11개 도메인 핸들러를 각각 독립 파일로 분리하여 파일당 150~250줄 이내로 유지.

- [x] **`U01` Zero-Config 스마트 기본값 (자동 탐색)**
  - **파일**: `crates/lens-cli/src/cli.rs`, `crates/lens-ops/src/{env,build,disk}.rs`
  - **수정 명세**:
    1. `lens disk scan [PATH]`: `PATH` 생략 시 현재 디렉터리(`"."`) 기본 사용.
    2. `lens env check [VENV_PATH]`: 경로 생략 시 `$VIRTUAL_ENV` → `./.venv` → `./venv` 순으로 자동 탐색.
    3. `lens build inspect [COMPDB]` / `lens build impact <HEADER> [--compdb COMPDB]`: `--compdb` 생략 시 `./compile_commands.json` → `./build/compile_commands.json` → `./out/compile_commands.json` 자동 탐색.

- [x] **`U02` 모든 도메인 `diff` 명령의 라이브 입력 & 스냅샷 하이브리드 지원 및 최상위 `lens diff <left> <right>` 추가**
  - **파일**: `crates/lens-ops/src/{abi,sys,env,build,net,test,disk}.rs`, `crates/lens-cli/src/cli.rs`
  - **수정 명세**:
    1. `env diff`, `build diff`, `net diff`, `disk diff`(`SnapshotDiff` 연결)가 사전 생성된 JSON 스냅샷뿐만 아니라 라이브 디렉터리(`.venv`, `compile_commands.json`, `/proc`, 스캔 대상 디렉터리)도 직접 인자로 받아 즉석에서 비교하도록 통합 로더 구현.
    2. 최상위 `lens diff <left> <right>` 서브커맨드를 추가하여 `detect_input_kind`로 두 입력의 도메인을 자동 판별해 해당 도메인 Diff를 실행.

- [x] **`U03` `lens doctor`에 실행 가능한 해결 가이드(`remediation_hint`) 추가**
  - **파일**: `crates/lens-ops/src/doctor.rs`
  - **수정 명세**: `DoctorCheck` 구조체에 `#[serde(skip_serializing_if = "Option::is_none")] pub remediation: Option<String>` 필드를 추가하여, `Warn`/`Fail` 항목마다 후속 진단/해결 명령어(예: `"Run 'lens sys analyze <unit>' or 'systemctl status <unit>'"`, `"Run 'lens disk scan <mount> --top 10'"`)를 텍스트 및 JSON 리포트에 함께 출력.

---

## Phase 5: `lens-tui` 비동기 아키텍처 전환 및 `lens-mcp` 보안/프로토콜 완성

### 5.1 `lens-tui` 버그 수정 및 대시보드 고도화 (`TU01` ~ `TU06`, `R06`)

- [x] **`TU01` [Critical] `Logs` 탭 `g`(맨 위로 이동) 시 빈 화면 출력 및 `/` 검색 후 스크롤 먹통 버그 수정**
  - **파일**: `crates/lens-tui/src/app.rs:146-162, 228-246`, `crates/lens-tui/src/term.rs:353-361`
  - **수정 명세**:
    1. `log_scroll`의 의미를 명확히 **표시창 하단 끝 줄 인덱스(`1..=total`)** 또는 **`Option<usize>`(`None` = tail 모드)**로 정의.
    2. `g` 키 입력 시 `self.log_scroll = visible_height.min(total)`(첫 페이지 표시)로 설정하고, `k`/`PgUp` 입력 시 `self.log_scroll = self.log_scroll.min(total).saturating_sub(step).max(1)`로 먼저 `total`에 클램핑한 뒤 감소시켜 `usize::MAX`에서 헛도는 문제를 해결.

- [x] **`TU02` [High] `Network` 탭의 PID/프로세스명 표기 뒤바뀜 및 `OwnerUnknown` 소켓의 `<orphan>` 허위 표시 수정**
  - **파일**: `crates/lens-tui/src/term.rs:486-505`
  - **수정 명세**:
    1. 프로세스 표시 포맷을 `format!("{} (PID: {})", p.name, p.pid)`로 정정.
    2. `s.process`가 `None`일 때 `s.owner_state`를 확인하여:
       - `Some(OwnerState::Orphan)` → `"<orphan>"` (빨간색/노란색 강조)
       - `Some(OwnerState::OwnerUnknown)` → `"<permission-denied>"` (어두운 회색)
       - `None` (예: `TIME_WAIT` 커널 소켓) → `"<kernel>"`

- [x] **`TU03` [High] `Storage` 탭 디렉터리 이동(`enter`/`parent`) 시 `total_size` 재계산 및 `?` 도움말 모달 `Clear` 위젯 적용**
  - **파일**: `crates/lens-tui/src/app.rs:45-75`, `crates/lens-tui/src/term.rs:514-530`
  - **수정 명세**:
    1. `DirBrowser::enter` 및 `DirBrowser::parent`에서 현재 디렉터리의 `self.entries.iter().map(|e| e.size).sum()`으로 `self.total_size`를 즉시 갱신.
    2. `draw_help_modal`에서 `Paragraph` 렌더링 직전에 `f.render_widget( ratatui::widgets::Clear, area );`를 호출하여 배경 텍스트가 비치지 않도록 수정.

- [x] **`R06` & `TU04` `lens-tui`를 `lens-ops` 기반으로 통합하고 백그라운드 스레드에서 `reload_*` 수행 + `Doctor` 오버뷰 탭 추가**
  - **파일**: `crates/lens-tui/Cargo.toml`, `crates/lens-tui/src/app.rs`, `crates/lens-tui/src/term.rs`
  - **수정 명세**:
    1. `lens-tui`가 `lens-ops`를 의존성에 추가하고, `reload_logs`, `reload_services`, `reload_network`, `run_doctor`를 백그라운드 워커 스레드(`mpsc::channel`)에서 실행하여 대용량 파일 로딩 중에도 UI가 멈추지 않도록 개선.
    2. `Doctor` 탭을 추가하여 시스템 건강 검진 결과와 도메인별 바로가기를 제공.

### 5.2 `lens-mcp` 보안·페이지네이션·프로토콜 수정 (`M01` ~ `M07`)

- [x] **`M01` [Critical] `handler::check_allowed`와 `lens_core::read_policy` 일원화 및 Symlink Oracle 차단**
  - **파일**: `crates/lens-mcp/src/handler.rs:37-60`
  - **수정 명세**: `handler::check_allowed`의 불완전한 자체 canonicalize/parent 폴백 로직을 제거하고, Phase 1(`C06`)에서 강화된 `lens_core::read_policy::check(path)`를 직접 호출하여 심볼릭 링크 체인 검증과 에러 메시지를 100% 일원화.

- [x] **`M02` [High] `lens_log_filter`의 기본 25건 클램핑 시 `_pagination.has_more` 누락 및 전수 `parse_line` 성능 병목 수정**
  - **파일**: `crates/lens-mcp/src/handler.rs:276-343`, `crates/lens-mcp/src/protocol.rs:159`, `crates/lens-log/src/filter.rs:206-226`
  - **수정 명세**:
    1. `FilterEngine`에서 `query`(단순 부분 문자열 검색)가 지정된 경우, 무거운 `parse_line`(타임스탬프/JSON 파싱)을 호출하기 **전에** raw 라인에 대해 대소문자 무시 부분 문자열 매칭을 먼저 수행하여 불일치 라인의 99%를 즉시 건너뛰도록 순서 최적화.
    2. `lens_log_filter` 핸들러에서 `effective_limit`(기본 25)을 적용할 때 `effective_limit + 1`개까지 매칭을 시도하여 26번째 매칭이 존재하면 `_pagination: { offset, limit: 25, returned: 25, has_more: true, next_offset: offset + 25 }`를 정확히 반환.
    3. `protocol.rs:159`의 `query` 설명을 `"Substring match on raw line (case-insensitive)"`로 정정.

- [x] **`M03` [High] `lens_sys_cycles` 도구의 `edges` 증거 복원 및 `main.rs`의 `stdin` 라인 크기 상한 적용**
  - **파일**: `crates/lens-mcp/src/handler.rs:486-501`, `crates/lens-mcp/src/main.rs:51-95`
  - **수정 명세**:
    1. `lens_sys_cycles`에서 `c.units`만 뽑아내던 코드를 제거하고 `CycleReport`(`units` + `edges` 출처 파일/라인/디렉티브 포함) 전체를 직렬화.
    2. `main.rs`에서 `stdin.lock()`을 읽을 때 한 줄당 최대 `10 MiB` 상한을 두는 bounded line reader를 사용하여 악의적인 개행 없는 입력으로 인한 OOM 방지.
    3. JSON-RPC 2.0 스펙에 맞춰 구문 오류(broken JSON)는 `-32700`(`Parse error`), 잘못된 요청 구조는 `-32600`(`Invalid Request`, `"id": null` 포함)으로 구분하여 응답.

---

## Phase 6: 플랫폼 차세대 기능 확장 (`E10` ~ `E12`, `R07`)

- [x] **`E10` 증거 번들 생성 시 민감정보 자동 마스킹 (`lens bundle create --redact`)**
  - **파일**: `crates/lens-core/src/redact.rs` (신규), `crates/lens-ops/src/bundle.rs`, `crates/lens-cli/src/cli.rs`
  - **수정 명세**:
    1. API 키(`sk-...`, `AIza...`, `ghp_...`, `AKIA...`), Bearer 토큰, `password=...`, `secret=...`, 개인키 블록(`-----BEGIN ... PRIVATE KEY-----`)을 정규식/패턴으로 탐지하여 결정론적 솔트 해시 토큰(`<REDACTED:sha256:8자리>`)으로 치환하는 `Redactor` 구현(동일한 시크릿이 두 곳에서 쓰였는지는 해시 일치로 알 수 있으면서 원본 값은 노출하지 않는 Evidence-Bound 마스킹).
    2. `lens bundle create --redact` 플래그 추가 및 `manifest.json`에 `"redacted": true` 기록.

- [x] **`E11` `.lens.toml` 선언적 진단 정책 설정 지원**
  - **파일**: `crates/lens-ops/src/config.rs` (신규), `crates/lens-ops/src/doctor.rs`
  - **수정 명세**: 프로젝트 루트나 `/etc/lens/config.toml`에서 `doctor`의 디스크 여유율 임계값(`disk_warn_pct`, `disk_fail_pct`), 무시할 마운트 포인트(`ignore_mounts`), 필수 실행 서비스(`required_units`), 커스텀 제외 글로브(`exclude_globs`)를 오버라이드할 수 있도록 지원.

---

## ✅ 최종 완료 기준 (Definition of Done)

1. 이 계획서의 모든 체크박스 항목(`[ ]`)이 구현되고 `[x]`로 갱신되어야 한다.
2. 각 버그 수정 항목마다 최소 1개 이상의 회귀 단위/통합 테스트가 추가되어 `cargo test --workspace`에서 실행되어야 한다.
3. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release --workspace`가 모두 경고/에러 0건으로 통과해야 한다.
