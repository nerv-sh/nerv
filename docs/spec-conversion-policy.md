# Spec Conversion Policy — `withfig/autocomplete` TS → JSON

> **Status**: 인수 기준 (글이 코드보다 먼저). PLAN.md v0.6 §5.7 + M0-9 + (신규) §0.2 정합.
> **목적**: TS 기반 Fig spec 을 v1.0 의 *런타임 JS 엔진 없는* 정적 JSON 으로 변환할 때의 정책을 확정한다. 무엇을 받고, 무엇을 자르고, 어디에 알리는가.
> **v0.6 핵심 전환**: v0.5 의 자작 `build/spec-transpile/` (swc_core 골격) 폐기 → upstream `aws/amazon-q-developer-cli-autocomplete` 의 `packages/autocomplete-parser/src/loadSpec.ts` 를 `crates/nerv-engine/src/spec_loader.rs` 로 1:1 포팅 (M0-6). Tier 정책 (A/B/C) + classifier 알고리즘 (§2) 은 그대로 유지. Tier C 회복은 v1.0 외 — **M1 rquickjs opt-in** (§4.4 신설).

---

## 1. 배경

`withfig/autocomplete` 는 600+ CLI 의 spec 을 TypeScript 로 제공한다 (MIT). 각 spec 의 형태:

```ts
const completionSpec: Fig.Spec = {
  name: "git",
  subcommands: [
    { name: "checkout", args: { name: "branch", generators: gitGenerators.branches } },
    { name: "commit", options: [ { name: ["-m", "--message"], args: { name: "msg" } } ] },
    // ...
  ],
};
```

**문제**: `generators` 필드는 *실행 시점* 에 자식 프로세스 (예: `git branch --list`) 를 호출해 후보를 가져오는 코드. 정적 추출 불가.

**v1.0 결정**: 정적으로 추출 가능한 부분 (서브커맨드 / 플래그) 은 JSON 으로 직렬화. 동적 generator 는 **런타임에 실행** — Tier B (정적 shell command, 예 `git branch --list`) 는 엔진이 직접 spawn (200ms timeout + TTL 5s LRU 64 cache), well-known Tier C (kubectl/docker/aws/npm scripts/filepaths) 는 signature recognizer 로 Rust-native 회복. closure-only tail 만 `--features quickjs` opt-in (출시 미동봉).

> **갱신 (v1.4)**: 초기 v0.5.1 설계는 동적 인자를 manifest `limited_args` 로 *마킹* 만 하고 사용자에겐 §5.1 "직접 입력하세요" 힌트를 띄울 계획이었다. M1 에서 Tier B 직접 실행 + recognizer 회복이 동적완성을 실제로 작동시키면서 이 마킹/힌트 메커니즘 (`limited_args` / `Response::DynamicHint` / `LimitedArg`) 은 **폐기 + 코드 삭제** 됐다. 아래 본문의 `limited_args` / §5.1 힌트 언급은 역사적 설계 기록 — 현 manifest 스키마 v2 에는 `limited_args` 필드가 없다.

---

## 2. 변환 분류 (Tier)

각 spec 은 변환 결과에 따라 3등급으로 분류.

### Tier A — 완전 정적 (Full)

- 서브커맨드, 옵션/플래그, 옵션 인자 placeholder 까지 *모두* 정적 추출 가능.
- generator 자체가 없거나, 정적 enum (예: `["yes", "no"]`) 뿐.
- 예상 후보 풀 (50개 중): `brew`, `cargo`, `make`, `tar`, `zip`, `curl`, `wget`, `jq`, `yq`, `bat`, `eza`, `ssh-keygen`, `terraform` (일부), …

**처리**: `specs-prebuilt/<name>.json` 에 `"tier": "A"`. v1.0 에서 *제한 없는* 자동완성.

### Tier B — 부분 정적 (Limited)

- 서브커맨드 + 플래그는 정적 추출 가능.
- 일부 인자가 동적 generator (`git checkout <branch>`, `kubectl -n <namespace>`, `docker run <image>:<tag>` 등).
- 예상 후보 풀 (50개 중): `git`, `docker`, `kubectl`, `npm`, `yarn`, `gh`, `aws`, `gcloud`, `helm`, `ansible`, …

**처리**:

- `"tier": "B"` + `"limited_args": [{"path": "checkout/<arg>", "reason": "dynamic-branch-list"}, …]`
- 사용자가 동적 인자 위치에 도달하면 §5.1 힌트.
- `nerv spec list` 에서 `git (limited)` 표시.

### Tier C — 변환 불가 (Excluded)

- spec 의 구조 자체가 정적 추출 어려움 (예: 스펙 전체가 `(context) => {...}` 함수).
- 또는 변환 후 JSON 크기가 비합리적 (>5MB 단일 spec) — 메모리 / 로드 시간 압박.

**처리**:

- `specs-prebuilt/` 에 *포함하지 않음*.
- 빌드 로그에 `[skip] <name>: tier C — <reason>`.
- 50개 후보 풀에서 *대체 후보* 로 교체. 교체 결정은 다음 §3.

### 분류 알고리즘 (개념)

```
def classify(spec):
    if not parse_succeeded(spec):
        return "C", "parse-fail"
    if has_dynamic_root(spec):  # spec = (ctx) => {...}
        return "C", "dynamic-root"
    static_part = strip_generators(spec)
    if size(static_part) > 5_000_000:
        return "C", "size"
    if any_dynamic_args(spec):
        return "B", list_limited_args(spec)
    return "A", None
```

---

## 3. 50개 후보 풀 운용

PLAN.md §4 의 50개 후보 풀은 **목표** 가 아니라 *우선순위 큐*. 변환 가능성에 따라 자동 조정.

### 3.1 M0-2 검증 (3개 spec)

`git`, `docker`, `kubectl` — 모두 Tier B 예상.

M0-2 산출물:

- 3개 spec 의 분류 결과
- Tier B 의 `limited_args` 정확성 검증 (각 spec 에서 동적 인자 누락 / 오류 식별)
- 이 결과를 50개 풀에 **외삽** 하여 Tier 분포 추정 (예: A 20% / B 70% / C 10%)

### 3.2 50개 풀 → 실제 50개 spec 확정 (M1 0–6주차)

```
1. 모든 50개 후보를 Tier 분류.
2. Tier C 인 spec 개수 카운트.
3. Tier C 만큼 후보 풀에서 다음 우선순위 spec 으로 교체:
   대체 큐: [direnv, mise, asdf, just, bazel, buf, op, doppler, vault, k9s,
              tig, lazygit, glab, devbox, taskfile, gum, kustomize, ko, dive, ctop]
4. 최종 50개 스냅샷 = `specs-prebuilt/manifest.json` 의 spec 목록.
```

**6주차 중간 체크포인트** (PLAN §10 M1) 의 차단 요건 중 하나: *50개 중 80% 이상이 Tier A 또는 B*. 미달 시 v1.0 spec 30 축소 (최후 수단).

### 3.3 사용자 시점

- `nerv spec list` 출력:
  ```
  $ nerv spec list
  git           limited (3 dynamic args, see ?)
  docker        limited (2 dynamic args)
  kubectl       limited (5 dynamic args)
  brew          full
  cargo         full
  jq            full
  ...
  Total: 50 specs (full: 22, limited: 28)
  ```
- `?` 키로 limited spec 의 동적 인자 목록 펼침.

---

## 4. 빌드 파이프라인 (v0.6 재정의)

v0.5 의 자작 `build/spec-transpile/` (swc_core 골격) 은 **폐기** —
upstream Fig 가 검증한 `packages/autocomplete-parser/src/loadSpec.ts`
+ `packages/shell-parser/src/parser.ts` 를 Rust 로 1:1 포팅한
`crates/nerv-engine/src/spec_loader.rs` (M0-6) + `shell_parser.rs`
(M0-4) + `spec_parser.rs` (M0-5) 가 대체. 다음 모듈 구조:

```
crates/nerv-engine/
├─ src/
│  ├─ shell_parser.rs       # ← shell-parser/parser.ts (M0-4)
│  ├─ spec_parser.rs        # ← autocomplete-parser/parseArguments.ts (M0-5)
│  ├─ spec_loader.rs        # ← autocomplete-parser/loadSpec.ts (M0-6)
│  ├─ classifier.rs         # Tier 분류 (자작, 본 문서 §2 알고리즘)
│  ├─ ranker.rs             # 기존 보존 (prefix-only M0, fuzzy M1)
│  └─ paths.rs              # 기존 보존
└─ Cargo.toml

# 빌드타임 CLI (M0-6):
cargo run -p nerv-engine --bin build-specs -- \
    --input vendor/withfig-autocomplete/src/ \
    --output ~/Library/Caches/nerv/specs/
```

`build-specs` 바이너리는 `spec_loader::load_spec()` + `classifier::classify()`
+ `serde_json::to_writer_pretty()` 의 직선 조합.

### 4.1 단계

1. `vendor/withfig-autocomplete/src/<name>.ts` 입력.
2. `spec_loader::load_spec()` 가 TS 의 정적 식별자 / 객체 리터럴
   추출 (loadSpec.ts 포팅분의 path 해석 + AST 워크). 동적 generator
   클로저는 메타로만 표시 + 본문 무시.
3. `classifier::classify()` 가 Tier 결정 (§2 알고리즘 그대로).
4. Tier A/B → AST 의 정적 부분 직렬화 → JSON.
5. Tier C → 빌드 로그만, 출력 X (M1 rquickjs opt-in 도입 시 §4.4 회복).
6. 모든 Tier A/B JSON 의 SHA256 + 메타를 `manifest.json` 에 기록:

```json
{
  "schema_version": 2,
  "nerv_version": "1.0.0",
  "withfig_commit": "<pinned sha>",
  "specs": [
    {"name": "git", "tier": "B", "sha256": "<hex>"},
    {"name": "brew", "tier": "A", "sha256": "<hex>"}
  ]
}
```

> 스키마 v2 `SpecMeta` = `{name, tier, sha256}`. (v0.5.1 의 `limited_args` 필드는 폐기 — 위 §1 갱신 참조. 현재 `build-specs` 는 `specs: []` 로 쓰고, per-spec tier/sha256 채우기는 error-states §3.6.2 spec-age 진단과 함께 예약.)

### 4.2 결정성 (deterministic build)

같은 입력 → 같은 출력. 키 정렬, 공백 정규화, timestamp 미포함.

이유: 재현 가능한 빌드 → CI 회귀 검증 + 사용자 신뢰.

### 4.3 CI 통합

GitHub Actions 매트릭스:

```yaml
- run: cargo run -p nerv-engine --bin build-specs -- \
       --input vendor/withfig-autocomplete/src/ \
       --output target/specs-build/
- run: ./scripts/check-spec-regression.sh target/specs-build/
```

`check-spec-regression.sh` 가 검증:

- Tier 분포가 이전 릴리즈 대비 후퇴 없음 (B → C 또는 A → C 신규 발생 시 PR fail).
- `manifest.json` 의 50개 spec 모두 존재.
- 새 spec 의 SHA 가 변경되었으면 changelog 갱신 강제.

### 4.4 Tier C 회복 — rquickjs opt-in (M1, PRD §5.7) ★ 신설

v1.0 (M0–M1) 의 Tier C 는 *빌드 산출 0* — `generator.custom!(ctx)`
같은 클로저는 정적으로 추론 불가. PRD v0.6 §0.2 의 비목표 (deno_core
임베드) 정책상 V8/JIT 임베드는 금지.

**M1 opt-in 경로**: `rquickjs` (~1MB, sandboxed, pure-C QuickJS 바인딩)
를 `crates/nerv-engine` 의 optional dep 으로 추가. 사용자가
`~/.config/nerv/nerv.toml` 에서 다음을 설정 시 활성:

```toml
[runtime]
dynamic_specs = true   # 기본 false — M1 opt-in
js_engine = "rquickjs" # 명시. deno_core 등 다른 값 금지 (CLAUDE.md §4)
```

활성 시 `nerv-engine::spec_runtime` 모듈 (M1 5-10주차 산출물) 이
Tier C 의 generator 함수를 QuickJS context 에 평가 → 결과 후보 반환.
호출 시점만 spin-up, idle 시 메모리 영점.

**비목표** (rquickjs opt-in 이 *하지 않는* 것):
- Tier C spec 의 빌드타임 JSON 변환 (런타임에만 평가).
- 네트워크 호출 / 파일 시스템 쓰기 (QuickJS context 권한 격리).
- Node.js / 웹 API (fetch, fs, process 등 미제공).
- Worker / async generator 의 완전한 호환 (best-effort).

성공 신호 — M1 dogfooding 결과로 Tier C 회복률 측정 후 v1.1 기본
활성화 (`dynamic_specs = true`) 검토.

---

## 5. Vendor 전략 (2개 upstream)

본 프로젝트는 **두 개의 git subtree** 로 upstream 코드를 흡수한다.
양쪽 모두 vendor 디렉터리 직접 편집 금지 (CLAUDE.md §4 + §8 함정).

### 5.0 두 upstream 의 역할 ★ 신설 (v1.3)

| upstream | 경로 | 라이선스 | 역할 | 핀 |
|----------|------|----------|------|---|
| `withfig/autocomplete` | `vendor/withfig-autocomplete/` | **ISC** (Hercules Labs Inc., Fig) | TS spec 1,484개 — 본 문서의 변환 대상 | `aef52acf…` (M0-9) |
| `aws/amazon-q-developer-cli-autocomplete` | `vendor/aws-autocomplete/` | **Apache-2.0 + MIT** dual (Amazon.com, Inc.) | Fig Rust 엔진 보존본 — `loadSpec.ts` / `parseArguments.ts` / `shell-parser/parser.ts` 의 포팅 원본 + `figterm` / `fig_ipc` / `fig_proto` / `fig_integrations` 등 흡수 crate 원본 | `b654a1be…` (M0-1, v0.6) |

본 문서 §5.1 / §5.2 / §5.3 은 **`withfig/autocomplete`** (spec 데이터)
에만 적용. `aws/amazon-q-developer-cli-autocomplete` (엔진 코드) 는
`PLAN.md v0.6 §8` 의 crate 인벤토리 + `CLAUDE.md §6` 의 저장소 구조
+ `NOTICE` 의 라이선스 명시로 거버넌스. 모니터링은 두 upstream 모두
`.github/workflows/upstream-monitor.yml` 의 matrix 가 매 2주 자동
점검 (per-upstream dedup 라벨 `fork:trigger:{withfig,aws}`).

### 5.1 Subtree 기준선

- `vendor/withfig-autocomplete/` 는 **git subtree** (단일 클론으로 빌드 가능).
- **현재 핀 (M0-9 결과)**: `aef52acff84c45edde61ae610cc2c964802b9a38`
  - vendor 크기: ~102 MB (1,484 TS spec)
  - 라이선스: ISC (Hercules Labs Inc., Fig) — v1.2 의 "MIT" 표기는 오류, package.json 확인 결과 ISC 가 정확 (NOTICE v0.6 정정)
  - subtree 추가 명령:
    ```
    git subtree add --prefix=vendor/withfig-autocomplete \
        https://github.com/withfig/autocomplete.git \
        aef52acff84c45edde61ae610cc2c964802b9a38 --squash
    ```
- 핀 변경은 별도 PR (`subtree update: <date> <commit>`) — 매 변경 시 §4.3 회귀 검증 필수.
- 모니터링: `.github/workflows/upstream-monitor.yml` 가 매 2주 (1·15일) 자동 점검.

### 5.2 패치 통합 정책

#### 5.2.A — 자체 spec 개선 (Nerv 측 발의)

- 자체 spec 개선이나 버그 수정은 *upstream 우선* — `withfig/autocomplete` 에 PR 후 머지 시 subtree pull.
- upstream 머지가 지연되거나 차단되면 `vendor-patches/self/<name>.patch` 로 보관, 빌드 시 적용.
- patch 보관 기준: PR 링크 동봉 + 6개월 이내 upstream 결판 시한.

#### 5.2.B — Upstream 커뮤니티 PR 흡수 ★ 신설 (v1.2)

> **배경**: upstream 의 commit 활동은 둔화됐지만 **issue / PR 은 계속 유입** 됨. Fig 사용자들이 새 CLI 플래그·도구 spec 을 PR 로 올리지만 머지가 사실상 정체. 이 콘텐츠는 MIT 이므로 Nerv 가 cherry-pick 합법. 1,484 → 200+ 점진 확장 (`v1.2` 로드맵) 의 가장 싼 노동력 풀.

**모니터링** (자동, 저비용):

- 신규 워크플로 `.github/workflows/upstream-prs.yml` — `upstream-monitor` 와 동일 cron (매 2주, 1·15일).
- `withfig/autocomplete` 의 open PR 중 다음 조건 만족하는 것 디지스트:
  1. `manifest.json` 의 50개 spec 풀 (또는 §3.2 대체 큐) 의 파일을 변경
  2. 30일 이상 머지 / 거부 답변 없음 (정체 신호)
  3. CI 가 있다면 green
- 결과를 단일 추적 이슈 `tracking: upstream-prs` 에 *갱신* (이슈 N개 양산 X). 신규 후보 / 자취 감춘 후보 / cherry-pick 완료 모두 표 형식.

**Cherry-pick 게이트** (수동 인간 판단):

- 추적 이슈에서 후보 PR 검토.
- 합격 기준: (a) 우리가 ship 하는 spec 의 변경, (b) `nerv-spec-build` 회귀 테스트 통과, (c) 라이선스 호환 자명 (MIT 통째 vendor 의 일부).
- 방법: `git format-patch -1 <upstream-pr-commit>` → `vendor-patches/upstream/upstream-pr-<num>.patch` 로 저장. **subtree 자체는 건드리지 않음** (M0-9 핀 보존 원칙).
- 빌드 적용: `nerv-spec-build` 가 transpile 직전 `vendor-patches/upstream/*.patch` 차례로 적용 (실패 시 해당 patch 만 skip + 빌드 로그 경고).

**라이선스 / 출처**:

- 패치 파일 첫 줄 `From: <원작자>` 가 `git format-patch` 기본 동작으로 보존.
- `vendor-patches/AUTHORS.md` 에 *PR 번호 / 원작자 / 적용 일자 / 영향 spec* 누적 기록.
- NOTICE 갱신 불필요 (MIT 통째 vendor 의 부분 — 이미 포함).
- upstream 이 추후 머지하면: 다음 subtree pull 시 patch 제거 + AUTHORS.md 에 *"merged upstream"* 표기.

**Cadence**:

- M1 0–6주차: 워크플로 활성화 + 첫 cherry-pick 1건 시연 (M1 산출물).
- M1 6–16주차: 매 2주 디지스트 → 1시간 검토 / 사이클.
- v1.0 이후: 월 1회.
- 시간 부담 ≥ 가치 발생 시 즉시 일시 중단 가능 — *옵션 정책*, 차단 요건 아님.

**비목표** (이 정책이 *하지 않는* 것):

- upstream 의 closed/머지된 PR 회수 — 이미 vendor pin 갱신으로 흡수됨.
- 후보 PR 의 자동 cherry-pick — 항상 수동 게이트.
- upstream PR 작성자에게 직접 컨택 — 정책 외 (커뮤니티 매너 영역).

### 5.3 Upstream Archived 시 fork 트리거

> **갱신 (v1.1)**: trigger 기간을 6개월 → **3개월** 로 단축. Fig 가 Amazon Q 로 흡수된 후 `withfig/autocomplete` 의 commit 활동이 실질적으로 둔화되어 *이미 archived 에 가까운* 상태이기 때문.

`withfig/autocomplete` 가 archived 되거나 **3개월 이상 commit 부재** 시:

1. 즉시 `nerv-sh/autocomplete-specs` 로 forward-only fork.
2. 본 문서의 vendor 경로를 fork 로 교체.
3. NOTICE 갱신 — 원본 MIT 보존 + fork 메타.
4. 커뮤니티 PR 채널을 nerv 측으로 이행 (자체 spec authoring 가이드 `docs/spec-authoring.md` 작성).
5. 사용자 영향 없음 (정책 내부 변화).

**판단 기준** (자동 모니터링):

- 매 **2주** 마다 (기존: 30일) upstream 의 마지막 commit 날짜 점검.
- archived 플래그 점검.
- 둘 중 하나 충족 시 GitHub Issue 자동 생성 (`fork: trigger`).
- M0-9 산출물에 본 모니터링 GitHub Actions workflow 포함 (cron `0 0 1,15 * *`).

---

## 6. 라이선스 / NOTICE (v0.6 정정)

| 출처 | 라이선스 | NOTICE 명시 |
|------|----------|-------------|
| `withfig/autocomplete` (spec 데이터) | **ISC** | ✓ v0.6 정정 (v0.5 의 "MIT" 표기 오류 — package.json 확인) |
| `aws/amazon-q-developer-cli-autocomplete` (Rust 엔진 + TS 포팅 원본) | **Apache-2.0 + MIT** dual | ✓ v0.6 신규 (M0-1) |
| 본 프로젝트 | **Apache-2.0** | LICENSE |
| 흡수 crate (`crates/nerv-{pty,term,ipc,proto,integrations,os,util,settings,log,diag,test-macro,test-utils}/`) | 원본 Apache-2.0 + MIT 라이선스 헤더 보존 | `AUTHORS.md` (M1 0–4주차 작성) |

양립성: ISC → Apache-2.0 prebuilt artifact OK, Apache+MIT → Apache-2.0 OK.

`NOTICE` 의 정확한 형식은 `/NOTICE` 파일 참조 (M0-1 commit aec0e3c8 기준).
빌드 산출물은 ISC 라이선스 헤더 포함.

**배포 채널 (2026-07-20 확정)**: `release.yml` 이 bun `convert:all` +
`build-specs --compress` 를 CI 에서 실행해 release tarball 에 `specs/`
(~10MB gzip + manifest) 를 동봉하고, Homebrew Formula 가 `pkgshare` 로
`share/nerv/specs/` 에 설치한다. 엔진의 `paths::resolve_specs_dir()` 가
user cache (`~/Library/Caches/nerv/specs/`) 에 spec 이 없을 때 이 번들을
읽는다 — 로컬 `build-specs` 실행 (user cache) 은 번들을 *통째로* override.

### 6.1 사용자 overlay — `~/.config/nerv/specs/` (2026-08-25)

upstream `withfig/autocomplete` 는 2025-05 이후 커밋이 없다. 거기 없는 도구
(`claude`, 사내 CLI, 개인 스크립트) 의 spec 을 번들 전체를 다시 빌드하지 않고
**한 파일씩** 얹는 경로가 이 디렉터리다. 계약:

| 항목 | 규칙 |
|------|------|
| 경로 | `~/.config/nerv/specs/<name>.json` 또는 `<name>.json.gz` (loader 가 둘 다 읽음, manifest 불필요) |
| 해석 순서 | `paths::resolve_spec_layers()` = `[overlay (spec 이 1개 이상 있을 때), primary]`. primary = 기존 `resolve_specs_dir()` 체인 (user cache → bundled → user 경로). **`NERV_SPECS_DIR` 가 설정되면 그 dir 하나만** — 테스트 격리는 그대로 airtight |
| 충돌 | 같은 stem 이 양쪽에 있으면 overlay **파일이 통째로** 이긴다. subcommand/option 단위 merge 없음 — 번들 spec 을 손보려면 복사해서 전체를 둔다 |
| 파손 | overlay JSON 이 깨지면 그 stem 만 완성 없음 (negative cache, E2 와 동일). 다른 spec 은 영향 없음. `nerv doctor` 가 `user specs` 행을 red 로 표시 |
| schema 게이트 (E5) | primary dir 의 `manifest.json` 만 검사. overlay 에는 manifest 를 두지 않는다 |
| hot-reload | overlay dir 이 **데몬 부팅 시 존재**하면 FSEvents 로 파일 편집·추가·삭제가 다음 키스트로크에 반영. dir 을 부팅 *후* 만들었으면 `nerv stop && nerv start` 1회 |
| uninstall | `~/.config/nerv/` 와 함께 삭제. 보존하려면 `nerv uninstall --keep-config` (uninstall-spec §2 행 6) |
| 포맷 | `crates/nerv-engine/tests/fixtures/specs/*.json` 과 같은 JSON (`name` / `description` / `subcommands` / `options[].names` / `args`). 살아있는 예제 = `examples/specs/claude.json` |

소비자 3곳 (daemon / `nerv doctor` / `nerv spec list`) 은 전부 `resolve_spec_layers()` 를
쓴다 — 한 곳이라도 `resolve_specs_dir()` 단독으로 남으면 doctor 가 데몬과 다른 spec 을 진단한다.

---

## 7. 회귀 정책

### 7.1 Tier 후퇴 차단

- 같은 spec 이 새 vendor commit 에서 **Tier 등급 후퇴** (A→B, A→C, B→C) 시 PR fail.
- 후퇴를 감수해야 한다면 PR 에 `breaking: spec-regression` 라벨 + changelog 명시.

### 7.2 Limited args 변동

- B → B 안에서 `limited_args` 가 *증가* 한 경우 (정적이던 인자가 동적으로 변경) → 경고만, fail X.
- *감소* 한 경우 (동적이던 인자가 정적으로 추출 가능해짐) → 환영, 자동 적용.

### 7.3 SHA 변경 추적

- 모든 spec 의 SHA256 변경은 `manifest.json` 에 기록 (build artifact 의 일부).
- *v1.0 비목표*: 사용자에게 직전 버전 대비 spec 변경을 보여주는 명령 (예: `nerv spec list --changes`). manifest 자체는 사용자가 검사 가능하지만, diff 명령은 빌드 파이프라인 복잡도를 키우므로 v1.1+ 로 이연.

---

## 8. 사용자 시점 정합성

본 문서의 정책이 사용자에게 노출되는 6개 지점:

| 지점 | 정책 항목 |
|------|----------|
| `nerv spec list` 의 `(limited)` 라벨 | §3.3 |
| `?` 키로 limited 인자 목록 | §3.3 |
| §5.1 동적 힌트 메시지 (PLAN.md, error-states.md) | §2 Tier B |
| `nerv doctor` 의 "specs" 섹션 (full / limited / disabled 카운트) | §3.3 |
| 릴리즈 노트의 spec 변경 항목 | §7.3 |
| `vendor-patches/AUTHORS.md` 의 흡수 PR 출처 표기 | §5.2.B |

---

## 9. 비목표

- TS 의 *임의의 동적 코드* 를 정적 추론으로 풀어내는 것 (불가능 / 비합리).
- 빌드타임에 generator 명령을 *실제 실행* 해 결과를 캐시 (보안 + 비결정성).
- 사용자의 로컬 환경에서 spec 을 직접 편집하게 하는 것 (v1.0; v2.x 의 spec dev 모드).
- spec 의 i18n / 다국어 description (영문만).
- spec 자동 생성 (CLI `--help` 파싱) — 별도 프로젝트 영역.

---

## 10. M0 산출물 체크리스트 (v0.6 재정의)

v0.5 의 M0-9 산출물 5개 중 3개는 v0.5 에서 완료, 2개는 v0.6 폐기
(자작 transpile / manifest 자작 정의 → loadSpec.ts 포팅이 대체).
새 M0 산출물 (PRD v0.6 §10) 정합:

- [x] M0-9 (v0.5): `vendor/withfig-autocomplete/` subtree + 핀 (`aef52acff8…`)
- [x] M0-9 (v0.5): NOTICE 1차 작성
- [x] M0-9 (v0.5): upstream-monitor.yml (v0.6 = matrix 로 확장, M0-1)
- [x] M0-1 (v0.6): `vendor/aws-autocomplete/` subtree + 핀 (`b654a1be…`) + NOTICE 갱신 (commit aec0e3c8)
- [x] M0-2 (v0.6): filter-repo 12 crate 추출 + workspace 통합 + 4/4 pipeline green (commits 3cdd8adb / 7926e823 / cf6356d4 / 2f2c2b45 / 8aace014)
- [x] M0-3 (v0.6): Rust edition 2024 bump (commit 74ea844c)
- [ ] M0-4 (v0.6): `shell-parser/parser.ts` → `nerv-engine::shell_parser` Rust 포팅
- [ ] M0-5 (v0.6): `parseArguments.ts` → `nerv-engine::spec_parser` Rust 포팅 (상위 50 spec 분량)
- [ ] M0-6 (v0.6): `loadSpec.ts` → `nerv-engine::spec_loader` 포팅 + 빌드타임 50 spec → JSON serialize
- [ ] §3.2 대체 큐 20개 검토 / 확정 (M0-2 변환 결과 기반 외삽 후)
- [ ] `manifest.json` 스키마 v2 정의 (M0-6 의 일부)

## 10.B M1 0–6주차 산출물 체크리스트 (§5.2.B 활성화)

- [ ] `.github/workflows/upstream-prs.yml` — 매 2주 open PR 디지스트
- [ ] `vendor-patches/upstream/` 디렉터리 + `vendor-patches/self/` 디렉터리 분리
- [ ] `vendor-patches/AUTHORS.md` 양식 정의 + 첫 entry
- [ ] `nerv-spec-build` 의 transpile 직전 `vendor-patches/upstream/*.patch` 적용 단계
- [ ] *첫 cherry-pick 1건* — 50개 spec 풀에 영향 주는 upstream PR 1개 흡수 시연 (정책 정합성 검증)

---

## 11. 변경 트리거

- 두 vendor subtree (`withfig-autocomplete` / `aws-autocomplete`) 의 핀 갱신 시 §5.0 표 + §5.1 항목 갱신
- spec schema 의 `schema_version` bump (v2 → v3) 시 §4.1 manifest 형식 갱신
- 어느 한 upstream archived → §5.3 fork 절차 실행 + per-upstream §5.0 표 갱신
- M1 rquickjs opt-in 도입 commit 시 §4.4 의 "M1 산출물" 표기 → "활성 기능" 으로 승격
- v1.1 의 동적 generator (rquickjs) 기본 활성화 검토 시 §2 Tier C 정책 본문 갱신

---

*문서 v1.2 — PLAN.md v0.5 §5.7 + M0-9 의 정밀 명세.*
*v1.0 → v1.1: §5.3 fork 트리거 6개월 → 3개월 단축 + 모니터링 주기 30일 → 2주 (CEO v0.4 리뷰), §7.3 `nerv spec list --changes` v1.1+ 이연.*
*v1.1 → v1.2: §5.1 실제 pin (`aef52acff8…`) 기록, §5.2 를 5.2.A (자체 PR) + **5.2.B (upstream 커뮤니티 PR 흡수)** 로 분할 — `upstream-prs.yml` + `vendor-patches/{upstream,self}/` + `AUTHORS.md`. M1 0–6주차 산출물 체크리스트 §10.B 신설. 사용자 제안 (upstream issue/PR 활용) 반영.*
*v1.3 — PLAN.md v0.6 §0.2 / §5.7 정합. v1.2 → v1.3 변경: §0 헤더에 자작 transpile 폐기 + loadSpec.ts 포팅 명시, §4 빌드 파이프라인 전면 재정의 (build/spec-transpile/ → nerv-engine::{shell_parser, spec_parser, spec_loader}), **§4.4 rquickjs opt-in 신설** (M1 Tier C 회복, deno_core 금지), §5.0 신설 (두 upstream 의 역할 + matrix 모니터링), §5.1 라이선스 ISC 정정, §6 라이선스 표 정정 + 흡수 crate 라이선스 처리, §10 체크리스트를 v0.6 M0-1~6 산출물로 재구성. Tier A/B/C 분류 알고리즘 / classifier 자체 / §3 대체 큐 / §5.2 / §5.3 / §7 회귀 정책은 모두 무변경. 변경 트리거: M1 rquickjs 활성, withfig→fork 트리거 발동, aws-autocomplete EOL 신호 발견 시.*
*v1.4 — `limited_args` / §5.1 힌트 UX 폐기 반영. v1.3 → v1.4 변경: §1 v1.0 결정을 "동적 generator 런타임 실행" (Tier B 직접 spawn + recognizer 회복) 으로 갱신 + v1.4 갱신 박스 추가, §4.1 manifest 예제에서 `limited_args` 제거 (스키마 v2 `SpecMeta = {name, tier, sha256}`). 폐기 근거: M1 에서 Tier B 실행 + well-known recognizer 가 동적완성을 실제로 작동시켜 마킹/힌트 메커니즘 (`Response::DynamicHint` / `LimitedArg`) 이 불필요해짐 → 코드 삭제 (CLAUDE.md §3, first-5-min §8, PLAN §5.1). §2 Tier 분류 / §3~§11 정책 본문은 무변경 (본문의 `limited_args` 언급은 역사적 설계 기록).*
