# CLAUDE.md — Nerv 프로젝트 컨텍스트

> 이 파일은 Claude Code 가 프로젝트 디렉터리 진입 시 자동으로 읽습니다. **수정·확장은 환영하되 삭제 금지** — 협업의 단일 출처(SSOT)입니다.

## 1. 한 줄 요약

**Nerv** = 사라진 [Fig](https://fig.io) 의 인라인 셸 자동완성을 **AWS 가 보존한 Fig Rust 코드 (`aws/amazon-q-developer-cli-autocomplete`, Apache+MIT)** 위에 macOS + zsh 단일 정적 바이너리로 재구성. 자작 엔진 폐기, Fig 엔진 흡수 + TS 부분 Rust 포팅. 로그인 / AI / 텔레메트리 / Electron 없음.

## 2. 권위 문서 (이 순서로 읽으세요)

| # | 파일 | 역할 |
|---|------|------|
| 1 | `PLAN.md` (v0.6) | 제품 정책 / 스코프 / 로드맵 — **모든 결정의 근거**. v0.5.1 = `docs/archive/PLAN.v0.5.1.md` 보존 |
| 2 | `docs/uninstall-spec.md` (v1.1, v1.3 갱신 예정) | `nerv uninstall` 인수 기준 (출시 차단 요건) |
| 3 | `docs/error-states.md` (v1.1, v1.3 갱신 예정) | 5종 에러 UX + `nerv doctor` 자동 실행 (`fig_diagnostic` 흡수로 80% 완성) |
| 4 | `docs/terminal-compat.md` (v1.1, v1.2 갱신 예정) | 보장/베스트에포트 매트릭스 + ANSI whitelist/blacklist + figterm opt-in |
| 5 | `docs/first-5-min.md` (v1.1, v1.2 갱신 예정) | 12+0.5단계 사용자 시나리오 |
| 6 | `docs/spec-conversion-policy.md` (v1.2, v1.3 갱신 예정) | TS spec → JSON Tier A/B/C 정책 + Fig `loadSpec.ts` 포팅 + rquickjs Tier C opt-in (M1) |

> **원칙**: *"글이 코드보다 먼저"*. 어떤 동작을 바꾸기 전에 위 문서 중 해당 절을 먼저 갱신하고 PR 에 그 변경을 함께 커밋하세요. 코드와 문서가 어긋난 PR 은 리뷰 거부 사유.

## 3. 현재 단계

**M0 흡수 스파이크 (v0.6 재정의)** — 산출물 8개 중 7개 완료:

- ✅ M0-9 (v0.5 산출물): `withfig/autocomplete` subtree pin (`aef52acf…`, 1,484 TS spec, ISC)
- ✅ cargo workspace 스캐폴딩 (16 active crates, 450 workspace test 통과)
- ✅ NOTICE / LICENSE / `.github/workflows/{ci,upstream-monitor}.yml`
- ✅ M0-1: `vendor/aws-autocomplete/` subtree add + NOTICE Apache+MIT
- ✅ M0-2: `git filter-repo` 로 10개 crate 추출 → `crates/nerv-{pty,term,ipc,proto,integrations,util,settings,os,log,diag}/`. chunk 3d 완료로 nerv-pty 도 workspace 합류 (런타임 opt-in 은 NERV_PTY=1, M1)
- ✅ M0-3: Rust edition 2024 bump (workspace + 모든 crate + rust-toolchain.toml)
- ✅ M0-4: `shell-parser/parser.ts` (20 KB) → `nerv-engine::shell_parser` Rust 포팅 (124 test)
- ✅ M0-5: `parseArguments.ts` → `nerv-engine::spec_parser` Rust 포팅 (chunks 1-5, 174 test) — types + static helpers + state machine + token classifier + matcher
- ✅ M0-6: `loadSpec.ts` → `nerv-engine::spec_loader` + JSON 직렬화 + `build-specs` 바이너리 + `nerv-engine::complete` 파이프라인 + daemon wire-up. TS→JSON 변환 자체는 M1 (또는 외부 node 스크립트). hand-rolled fixture (git, echo, docker, kubectl) + 24 integration test 통과
- ✅ M0-7: ZLE → CLI → UDS → 실엔진 wire-up + latency bench. IPC p95 0.052 ms, CLI cold-start p95 4.07 ms (25 ms 예산 대비 16%). `_nerv.zsh` widget 포맷 호환 확인
- ⏳ M0-8: Apple Developer ID 서명/공증 빈 바이너리 e2e (**No-Go 차단 요건** — 인프라 의존)

**보너스 진척 (M0 산출물 외 — M1 0-4주차 작업의 ~70% 선행 완료)**:
- ✅ CLI 5/5 표면 완성: `nerv init` / `start` / `stop` / `spec list` / `doctor` / `uninstall` (uninstall-spec.md §4 8-step atomic 포함)
- ✅ nerv-engine::complete cursor-context override 2종: flag prefix → options 우선, word prefix + node has subs → subcommands 우선 (`git ` 같은 root with positional fallback 처리)
- ✅ fixture pack 9종 hand-rolled (git/echo/docker/kubectl/npm/cargo/gh/brew/make) + 43 integration test
- ✅ TS→JSON 변환 파이프라인 `tools/ts-to-json/` (bun 기반, 715 spec 변환, 0 failure)
  - Tier A 440 / B 6 / C 246 자동 분류
  - **loadSpec depth=1 활성**: `aws ec2 <verb>`, `aws s3 <verb>`, `gcloud compute instances <verb>` 등 nested 자동완성 동작
  - cycle-safe (visited Set + MAX_DEPTH gate)
- ✅ **SpecRegistry lazy load**: at_dir → 디스크 접근은 lookup() 시점. 715 spec 캐시 환경에서도 daemon 즉시 기동. negative cache 로 누락 binary 재시도 방지.
- ✅ **gzip 압축 cache** (`flate2`): `*.json.gz` 자동 감지 + decompress. 45MB→4.5MB plain, 176MB→10MB at depth=1 (10×). `build-specs --compress` 플래그.
- ✅ 에러 UX shell-side: E1 widget hint, E2 doctor table, E3 zsh<5.8 check, E4 widget conflict 감지 (E5 manifest 도입 후)
- ✅ widget UX: popup auto-size + description 잘림 수정
- ✅ SIGPIPE → SIG_DFL: `nerv spec list | head` panic 제거

**폐기된 v0.5 산출물**: M0-2 자작 transpile, `build/spec-transpile/` (loadSpec 포팅이 대체).

**진행중 옵션**:
- M0-8: 서명/공증 (Apple Developer 계정 + 인프라 필요)
- M1 본격: rquickjs Tier C (246 spec 회복) / spec cache 압축 + lazy load / loadSpec depth 활성 / E5 manifest

## 4. 절대 깨면 안 되는 불변식

코드 / 인프라 변경 시 다음을 어긋나면 즉시 차단:

| 영역 | 불변식 | 근거 |
|------|--------|------|
| 매칭 알고리즘 | **기본 prefix** — `git co` ≠ `checkout` (`c-h-` 시작). **fuzzy 는 M1 opt-in** (`~/.config/nerv/nerv.toml` 의 `[matching] mode = "fuzzy"`). v1.0 코드 자체는 prefix only, fuzzy 코드 경로는 M1 도입 시 비활성 분기로 추가 | PLAN §5.1 / `nerv-engine/src/ranker.rs` 회귀 테스트 |
| 매칭 알고리즘 | 빈 prefix 는 모두 매치 (`git ⎵` 케이스) | first-5-min §1단계 |
| 마커 블록 | `# >>> nerv >>>` ~ `# <<< nerv <<<` 는 **고정 문자열**. `fig_integrations` 흡수 시 marker 교체 필수 (Q 의 `# Fig pre block` 잔존 금지) | uninstall-spec §3 / `nerv-shell::MARKER_*` |
| 경로 | `~/Library/Caches/nerv/`, `~/Library/Logs/nerv/`, `~/.config/nerv/` — `directories` 크레이트 사용 X (docs 가 contract). `fig_util` / `fig_log` / `fig_settings` 흡수 시 Q 기본 경로 (`~/.config/q/`, `~/Library/Caches/amzn/`) 전부 nerv 경로로 재배선 | uninstall-spec §2 / `nerv-engine/src/paths.rs` |
| ANSI | alternate screen 진입 X, true color X, OSC 8/52 X — raw cursor save/restore + line clearing 만. **`fig_desktop` webview UI 흡수 금지** | terminal-compat §3 / §6 |
| 에러 톤 | `[nerv] <문제> — <조치>` 영문 한 줄, 사과/완곡어구 금지, 회색만 사용 | error-states §4 |
| CLI 표면 | v1.0 명령은 5개 (`init / doctor / start / stop / spec list / uninstall`) — 추가 금지. `q_cli` 흡수 금지 (chat/login/translate 잔존 금지) | PLAN §9 |
| 비목표 | AI / 텔레메트리 / 자체업데이트 / `nerv config` / `spec list --changes` 코드 자체를 두지 않음. **흡수 시 `fig_api_client`/`fig_auth`/`fig_telemetry*`/`amzn-*`/`semantic_search_client` 의존 0** | PLAN §4 비목표 |
| JS 엔진 | **deno_core 임베드 금지**. Tier C 회복은 M1 = `rquickjs` (~1 MB) opt-in 만 | PLAN §0.2 / §7 |
| PTY shim | `figterm` (`nerv-pty`) 는 **M1 opt-in only**. M0 ZLE widget 과 상호 배타. `NERV_PTY=1` 환경변수로 분기 | PLAN §5.8 / §6.2 |
| Rust | toolchain 1.85, **edition 2024** (upstream 정합). v0.5.1 의 edition 2021 폐기. 변경 시 PLAN §7 + `rust-toolchain.toml` + 본 §4 동시 갱신 | `rust-toolchain.toml` |
| vendor 편집 | `vendor/withfig-autocomplete/` 와 `vendor/aws-autocomplete/` **양쪽 모두 직접 편집 금지**. 변경은 `vendor-patches/{upstream,self}/` 또는 upstream PR | spec-conversion-policy §5.2 |

## 5. 자주 쓰는 명령

```bash
# 변경 후 항상 4개 모두 통과시켜야 PR 가능
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check     # auto-fix: cargo fmt --all
cargo test --workspace

# 단일 crate 만 빠르게 테스트
cargo test -p nerv-engine
cargo test -p nerv-shell

# nerv-cli / nervd 로컬 실행 (CLI 5/5 모두 구현됨)
cargo run -p nerv-cli -- init zsh           # ~/.zshrc 에 추가할 블록 출력
NERV_SPECS_DIR=$(pwd)/crates/nerv-engine/tests/fixtures/specs \
    cargo run -p nerv-cli -- start          # daemon 백그라운드 기동 + PID 파일
cargo run -p nerv-cli -- doctor             # 환경 진단 (zsh / hook / daemon / specs)
cargo run -p nerv-cli -- spec list          # 로드된 spec 표 (NAME/SUBS/OPTS/TIER)
cargo run -p nerv-cli -- _complete "git co" 6   # IPC bridge 수동 테스트
cargo run -p nerv-cli -- stop               # daemon 종료 (SIGTERM)
cargo run -p nerv-cli -- uninstall          # 마커블록 + 캐시/로그/설정 atomic 제거
                                             # --keep-config 로 ~/.config/nerv/ 보존

# TS→JSON 변환 (M1, bun 필요) — vendor/withfig-autocomplete/ → JSON
cd tools/ts-to-json
bun install
bun run convert:all                            # 715 spec → fixtures/converted/
cd -

# gzip 압축 + 설치 (10× 압축, 10MB 디스크)
cargo run --release -p nerv-engine --bin build-specs -- \
    --input crates/nerv-engine/tests/fixtures/converted/ \
    --output ~/Library/Caches/nerv/specs/ \
    --compress                                  # *.json.gz 출력

# 비압축 설치 (45MB 디스크, 인간 검사 용이)
cp crates/nerv-engine/tests/fixtures/converted/*.json ~/Library/Caches/nerv/specs/

# 일부만
cargo run -p nerv-engine --bin build-specs -- \
    --input crates/nerv-engine/tests/fixtures/converted/ \
    --output ~/Library/Caches/nerv/specs/ \
    --only git --only aws --only kubectl --compress

# Latency bench (M0-7 acceptance)
cargo test --release -p nerv-daemon --test bench_latency \
    -- --ignored --nocapture

# 흡수 crate 추출 (M0-2)
git subtree add --prefix vendor/aws-autocomplete \
    https://github.com/aws/amazon-q-developer-cli-autocomplete.git main --squash
# 그 후 git filter-repo 로 9개 crate 만 추출 — PLAN §10 M0-2 절차 참조
```

## 6. 저장소 구조 (요약)

```
crates/
  # 기존 보존
  nerv-cli/        # `nerv` 바이너리 (clap, 5 cmd + hidden _complete IPC bridge)
  nerv-daemon/     # `nervd` (tokio + UDS, SpecRegistry 로드 → nerv-engine::complete 위임)
  nerv-engine/     # 자작 + TS 포팅분 (shell_parser / spec_parser / spec_loader (gzip 자동감지) / complete (lazy registry) / ipc / paths / ranker)
                   #   + bin/build_specs.rs (M0-6 JSON validator/canonicalizer, --compress 플래그)
                   #   + tests/fixtures/specs/{git,echo,docker,kubectl,npm,cargo,gh,brew,make}.json (9 hand-rolled)
                   #   + tests/fixtures/converted/ (.gitignore; bun 변환 결과 715 spec; depth=1, 176MB plain or 10MB gzipped)
  nerv-shell/      # 마커 블록 init_block / strip_blocks (테스트 4종)

  # M0-2 신규 (filter-repo 흡수)
  nerv-pty/        # ← figterm (M1 opt-in)
  nerv-term/       # ← alacritty_terminal
  nerv-ipc/        # ← fig_ipc
  nerv-proto/      # ← fig_proto (strip — figterm + local 메시지만)
  nerv-integrations/ # ← fig_integrations (marker 교체)
  nerv-os/         # ← fig_os_shim
  nerv-util/       # ← fig_util (Q_→NERV_)
  nerv-settings/   # ← fig_settings (경로 재배선)
  nerv-log/        # ← fig_log
  nerv-diag/       # ← fig_diagnostic

shell-integrations/zsh/_nerv.zsh     # ZLE widget (M0 유지, M1 figterm 도입 시 deprecate)
tools/ts-to-json/                    # bun-based TS→JSON 변환 (M1 entry; 715 spec 자동 변환)
vendor/withfig-autocomplete/         # subtree, ISC, pin = aef52acf… (TS specs 1,484)
vendor/aws-autocomplete/             # M0-1 subtree, Apache+MIT, 미수정 mirror (drift 감지)
vendor-patches/{upstream,self}/      # cherry-pick 보관소 (M1)
docs/                                # 위 §2 6종 + reference/ (TS 포팅 참조본)
docs/archive/PLAN.v0.5.1.md          # 이전 PRD 보존
.github/workflows/{ci,upstream-monitor,upstream-prs}.yml  # upstream-monitor 에 aws-autocomplete 추가
```

**폐기된 v0.5 디렉터리**: `build/spec-transpile/` (loadSpec.ts 포팅이 대체), `specs-prebuilt/` (`~/Library/Caches/nerv/specs/` 로 이동).

## 7. 작업 가이드라인

### 7.1 PR 단위

- **인수 기준 변경 PR** — `docs/*.md` + 관련 코드 + 테스트가 한 PR.
- **구현 PR** — 인수 기준이 *이미* 글로 박혀 있어야 시작. 새 영역이면 docs 갱신 PR 을 먼저.
- **회귀 테스트** — 새 기능에는 단위 테스트 1개 이상. 외부 관찰 가능한 동작은 e2e 1개 이상 (M1).

### 7.2 Commit 메시지

DCO 필수 (`git commit -s`). 형식:

```
<type>(<scope>): <subject>

- bullet 1
- bullet 2

Refs: PLAN.md §<section>  또는  Refs: docs/<file>.md §<section>
```

### 7.3 4주차 / 10주차 / M0 종료 체크포인트 (v0.6)

PLAN §10 에 명시된 차단 요건을 *직접* 점검하기 전엔 다음 단계 진입 금지:

- **M0 종료**: 산출물 8개 중 1+2+3+7+8 충족. 4+5+6 에서 상위 50 spec 의 `git status / log / checkout` + `docker ps / build / run` + `kubectl get / describe / logs` 시나리오 통과. (현재 1-7 완료; git fixture 시나리오 11개 integration test 통과. docker / kubectl fixture + 상위 50 spec 확장은 M1 진입과 함께.)
- **M1 4주차**: 50개 spec 시나리오 통과 / latency p95 < 25 ms / tmux+2터미널 회귀 / uninstall 흔적 0
- **M1 10주차**: 내부 dogfooding 2주

미달 시 PLAN §10 M0-2 흡수 의사결정 트리 또는 wrapper crate 격리 전략.

## 8. 자주 빠지는 함정

- ❌ "checkout" 이 "co" 로 시작한다고 믿기 — `c-h-e-c-k…` 입니다. 회귀 테스트 박혀있음.
- ❌ `directories` 크레이트로 `~/Library/Caches/<bundle-id>/` 만들기 — docs 가 `~/Library/Caches/nerv/` 만 인정.
- ❌ `cargo fmt --all` 안 돌리고 PR — CI 가 `--check` 로 거부.
- ❌ 새 의존성을 워크스페이스 deps 에 안 넣고 직접 추가 — 일관성 깨짐.
- ❌ `vendor/withfig-autocomplete/` 또는 `vendor/aws-autocomplete/` 직접 편집 — 항상 upstream PR 먼저, 막히면 `vendor-patches/self/`. upstream 의 제3자 PR 흡수는 `vendor-patches/upstream/` (`spec-conversion-policy.md` §5.2.B).
- ❌ alternate screen / 24-bit color 사용 — terminal-compat §3 blacklist.
- ❌ `nerv` CLI 에 명령 추가 — 5개로 고정 (PLAN §9).
- ❌ 흡수 crate 의 Q 경로 (`~/.config/q/`, `~/Library/Caches/amzn/`) 잔존 — `nerv-util` / `nerv-log` / `nerv-settings` 포팅 시 grep 으로 전수 검증.
- ❌ `fig_desktop` / `fig_api_client` / `fig_auth` / `fig_telemetry*` / `amzn-*` 의 transitive dep 가 `Cargo.lock` 에 들어옴 — strip 후 `cargo tree | grep -E 'amzn|aws-sdk|tao|wry'` 0 줄 검증.
- ❌ `parseArguments.ts` Rust 포팅 시 TS 회귀 테스트 누락 — Fig 의 fixture 디렉터리 (`packages/autocomplete-parser/tests/`) 를 `crates/nerv-engine/tests/spec_parser/` 로 그대로 흡수.
- ❌ fuzzy matching 코드를 M0 에 작성 — M1 opt-in 까지 코드 자체 금지 (§4 불변식).
- ❌ deno_core / boa / Node embed — `rquickjs` 만 (§4 불변식).

## 9. 추천 협업 패턴 (Claude Code)

- **계획 모드(`/plan` 또는 ExitPlanMode 사용)** — M0-1, M0-2 같은 다단계 작업 시 먼저 계획 받고 검토 후 진입.
- **`/review`** — 큰 PR 직전 자기 검토.
- **하위 에이전트(Explore / Plan / general-purpose)** — withfig spec 패턴 탐색 시 Explore 에 위임 ("very thorough"), 아키텍처 설계 시 Plan.
- **`/init` 으로 본 파일 갱신** — 구조 변경 후.
- **CEO 리뷰 패턴** — 큰 결정 전에 의도적으로 비판적 두 번째 시각 요청. PLAN.md 가 v0.1→v0.5 까지 4번 REVISE 돈 이유.

## 10. 이 파일 갱신 트리거

- `PLAN.md` 가 vX → vX+1 로 올라가면 §2, §3, §4 갱신.
- 새 crate / 새 워크플로 추가 시 §6 갱신.
- 새 불변식 발견 시 §4 행 추가.
- 새 함정 만났을 때 §8 추가.
- spec / docs 수가 변할 때 §2 표 갱신.
- 흡수 crate strip 정책 변경 (예: `fig_remote_ipc` 부활) 시 §4 + §6 + PLAN §0.2 동시 갱신.

— 끝. 작업 즐겁게.
