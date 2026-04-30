# Spec Conversion Policy — `withfig/autocomplete` TS → JSON

> **Status**: 인수 기준 (글이 코드보다 먼저). PLAN.md v0.4 §5.7 + M0-9 정합.
> **목적**: TS 기반 Fig spec 을 v1.0 의 *런타임 JS 엔진 없는* 정적 JSON 으로 변환할 때의 정책을 확정한다. 무엇을 받고, 무엇을 자르고, 어디에 알리는가.

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

**v1.0 결정**: 정적으로 추출 가능한 부분만 JSON 으로 직렬화. 동적 generator 부분은 *마킹* 하고 사용자에겐 §5.1 힌트.

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

## 4. 빌드 파이프라인

```
crates/build/spec-transpile/
├─ src/
│  ├─ main.rs              # CLI: nerv-spec-build
│  ├─ classifier.rs        # Tier 분류
│  ├─ extract.rs           # TS AST → JSON (swc_core)
│  └─ manifest.rs          # specs-prebuilt/manifest.json 생성
└─ Cargo.toml
```

### 4.1 단계

1. `vendor/withfig-autocomplete/src/<name>.ts` 입력.
2. `swc_core` 로 TS → AST.
3. Classifier 가 Tier 결정.
4. Tier A/B → AST 의 정적 부분 직렬화 → JSON.
5. Tier C → 빌드 로그만, 출력 X.
6. 모든 Tier A/B JSON 의 SHA256 + 메타를 `manifest.json` 에 기록:

```json
{
  "schema_version": 2,
  "nerv_version": "1.0.0",
  "withfig_commit": "<pinned sha>",
  "specs": [
    {
      "name": "git",
      "tier": "B",
      "limited_args": [
        {"path": "checkout/<arg>", "reason": "dynamic-branch-list", "hint": "git branch --list"},
        {"path": "merge/<arg>", "reason": "dynamic-branch-list", "hint": "git branch --list"}
      ],
      "sha256": "<hex>"
    },
    {"name": "brew", "tier": "A", "sha256": "<hex>"}
  ]
}
```

### 4.2 결정성 (deterministic build)

같은 입력 → 같은 출력. 키 정렬, 공백 정규화, timestamp 미포함.

이유: 재현 가능한 빌드 → CI 회귀 검증 + 사용자 신뢰.

### 4.3 CI 통합

GitHub Actions 매트릭스:

```yaml
- run: cargo run -p nerv-spec-build -- vendor/withfig-autocomplete/src/ specs-prebuilt/
- run: ./scripts/check-spec-regression.sh
```

`check-spec-regression.sh` 가 검증:

- Tier 분포가 이전 릴리즈 대비 후퇴 없음 (B → C 또는 A → C 신규 발생 시 PR fail).
- `manifest.json` 의 50개 spec 모두 존재.
- 새 spec 의 SHA 가 변경되었으면 changelog 갱신 강제.

---

## 5. `withfig/autocomplete` Vendor 전략

### 5.1 Subtree 기준선

- `vendor/withfig-autocomplete/` 는 **git subtree** (단일 클론으로 빌드 가능).
- **현재 핀 (M0-9 결과)**: `aef52acff84c45edde61ae610cc2c964802b9a38`
  - vendor 크기: ~102 MB (1,484 TS spec)
  - 라이선스: MIT (Hercules Labs Inc., Fig)
  - subtree 추가 명령:
    ```
    git subtree add --prefix=vendor/withfig-autocomplete \
        https://github.com/withfig/autocomplete.git \
        aef52acff84c45edde61ae610cc2c964802b9a38 --squash
    ```
- 핀 변경은 별도 PR (`subtree update: <date> <commit>`) — 매 변경 시 §4.3 회귀 검증 필수.
- 모니터링: `.github/workflows/upstream-monitor.yml` 가 매 2주 (1·15일) 자동 점검.

### 5.2 자체 PR / 커뮤니티 패치

- 자체 spec 개선이나 버그 수정은 *upstream 우선* — `withfig/autocomplete` 에 PR 후 머지 시 subtree pull.
- upstream 머지가 지연되거나 차단되면 `vendor-patches/<name>.patch` 로 별도 디렉터리에 보관, 빌드 시 적용.
- patch 보관 기준: PR 링크 동봉 + 6개월 이내 upstream 결판 시한.

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

## 6. 라이선스 / NOTICE

- `withfig/autocomplete`: MIT.
- 본 프로젝트: Apache-2.0.
- 양립 가능 (MIT → Apache-2.0 prebuilt artifact).

`NOTICE` 파일에 명시:

```
This product includes software developed by Fig (now part of AWS).
Source: https://github.com/withfig/autocomplete (MIT)
Vendor commit: <pinned sha>
```

빌드 산출물 `specs-prebuilt/` 도 MIT 라이선스 헤더 포함 (배포 시).

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

본 문서의 정책이 사용자에게 노출되는 5개 지점:

| 지점 | 정책 항목 |
|------|----------|
| `nerv spec list` 의 `(limited)` 라벨 | §3.3 |
| `?` 키로 limited 인자 목록 | §3.3 |
| §5.1 동적 힌트 메시지 (PLAN.md, error-states.md) | §2 Tier B |
| `nerv doctor` 의 "specs" 섹션 (full / limited / disabled 카운트) | §3.3 |
| 릴리즈 노트의 spec 변경 항목 | §7.3 |

---

## 9. 비목표

- TS 의 *임의의 동적 코드* 를 정적 추론으로 풀어내는 것 (불가능 / 비합리).
- 빌드타임에 generator 명령을 *실제 실행* 해 결과를 캐시 (보안 + 비결정성).
- 사용자의 로컬 환경에서 spec 을 직접 편집하게 하는 것 (v1.0; v2.x 의 spec dev 모드).
- spec 의 i18n / 다국어 description (영문만).
- spec 자동 생성 (CLI `--help` 파싱) — 별도 프로젝트 영역.

---

## 10. M0-9 산출물 체크리스트

- [ ] `vendor/withfig-autocomplete/` subtree 생성 + 핀 commit 결정
- [ ] NOTICE 파일 작성
- [ ] `crates/build/spec-transpile/` 골격 + classifier 함수 시그니처
- [ ] `manifest.json` 스키마 v2 정의
- [ ] git/docker/kubectl 3종 변환 결과로 Tier 분포 외삽 보고
- [ ] upstream 모니터링 GitHub Action workflow
- [ ] 본 문서 (현 파일) 의 §3.2 대체 큐 20개 검토 / 확정

---

## 11. 변경 트리거

- vendor commit 핀 갱신 시 본 문서의 §5.1 항목 갱신
- spec schema 의 `schema_version` bump (v2 → v3) 시 §4.1 manifest 형식 갱신
- upstream archived → §5.3 fork 절차 실행 + 본 문서 §5 전면 갱신
- v1.1 의 동적 generator (deno_core) 도입 시 본 문서의 §2 Tier B 정책 deprecate, 새 정책 문서로 이행

---

*문서 v1.1 — PLAN.md v0.5 §5.7 + M0-9 의 정밀 명세. v1.0 → v1.1 변경: §5.3 fork 트리거 6개월 → 3개월 단축 + 모니터링 주기 30일 → 2주 (CEO v0.4 리뷰), §7.3 `nerv spec list --changes` 명시적 v1.1+ 이연. 본 정책이 v1.0 의 spec 호환성 약속의 권위 있는 기준.*
