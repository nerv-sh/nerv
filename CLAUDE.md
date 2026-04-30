# CLAUDE.md — Nerv 프로젝트 컨텍스트

> 이 파일은 Claude Code 가 프로젝트 디렉터리 진입 시 자동으로 읽습니다. **수정·확장은 환영하되 삭제 금지** — 협업의 단일 출처(SSOT)입니다.

## 1. 한 줄 요약

**Nerv** = 사라진 [Fig](https://fig.io) 의 인라인 셸 자동완성을 macOS + zsh 에 정밀 복원한 Rust 단일 바이너리. 로그인 / AI / 텔레메트리 없음.

## 2. 권위 문서 (이 순서로 읽으세요)

| # | 파일 | 역할 |
|---|------|------|
| 1 | `PLAN.md` (v0.5) | 제품 정책 / 스코프 / 로드맵 — **모든 결정의 근거** |
| 2 | `docs/uninstall-spec.md` (v1.1) | `nerv uninstall` 인수 기준 (출시 차단 요건) |
| 3 | `docs/error-states.md` (v1.1) | 5종 에러 UX + `nerv doctor` 자동 실행 |
| 4 | `docs/terminal-compat.md` (v1.1) | 보장/베스트에포트 매트릭스 + ANSI whitelist/blacklist |
| 5 | `docs/first-5-min.md` (v1.1) | 12+0.5단계 사용자 시나리오 |
| 6 | `docs/spec-conversion-policy.md` (v1.1) | TS spec → JSON Tier A/B/C 정책 + fork 전략 |

> **원칙**: *"글이 코드보다 먼저"*. 어떤 동작을 바꾸기 전에 위 문서 중 해당 절을 먼저 갱신하고 PR 에 그 변경을 함께 커밋하세요. 코드와 문서가 어긋난 PR 은 리뷰 거부 사유.

## 3. 현재 단계

**M0 스파이크** — 산출물 10개 중 5개 완료:

- ✅ M0-9: `withfig/autocomplete` subtree pin (`aef52acf…`, 1,484 TS spec, MIT)
- ✅ cargo workspace 스캐폴딩 + 단위 테스트 8개 통과
- ✅ NOTICE / LICENSE / `.github/workflows/{ci,upstream-monitor}.yml`
- ⏳ M0-1: zsh ZLE → UDS → 인라인 ANSI PoC (p95 < 25 ms 검증)
- ⏳ M0-2: git/docker/kubectl 3종 spec 변환 → Tier 분포 외삽 → §10 의사결정 트리
- ⏳ M0-3, M0-4: `?` 도움말 PoC, 30초 온보딩 시뮬레이션
- ⏳ M0-5: 터미널 호환성 + tmux + zsh-autosuggestions 공존 e2e
- ⏳ M0-6: Inshellisense 1대1 정량 벤치
- ⏳ M0-7: 첫 5분 시나리오 12+0.5단계 녹화
- ⏳ M0-8: Apple Developer ID 서명/공증 빈 바이너리 e2e
- ⏳ M0-10: 문서 vs 구현 delta 1쪽 표 점검

다음 작업 우선순위는 **M0-1 → M0-2** (PLAN.md §15).

## 4. 절대 깨면 안 되는 불변식

코드 / 인프라 변경 시 다음을 어긋나면 즉시 차단:

| 영역 | 불변식 | 근거 |
|------|--------|------|
| 매칭 알고리즘 | **prefix-only** — `git co` ≠ `checkout` (`c-h-` 시작). fuzzy 는 v1.x 비목표 | PLAN §5.1 / `nerv-engine/src/ranker.rs` 회귀 테스트 |
| 매칭 알고리즘 | 빈 prefix 는 모두 매치 (`git ⎵` 케이스) | first-5-min §1단계 |
| 마커 블록 | `# >>> nerv >>>` ~ `# <<< nerv <<<` 는 **고정 문자열** | uninstall-spec §3 / `nerv-shell::MARKER_*` |
| 경로 | `~/Library/Caches/nerv/`, `~/Library/Logs/nerv/`, `~/.config/nerv/` — `directories` 크레이트 사용 X (docs 가 contract) | uninstall-spec §2 / `nerv-engine/src/paths.rs` |
| ANSI | alternate screen 진입 X, true color X, OSC 8/52 X — raw cursor save/restore + line clearing 만 | terminal-compat §3 / §6 |
| 에러 톤 | `[nerv] <문제> — <조치>` 영문 한 줄, 사과/완곡어구 금지, 회색만 사용 | error-states §4 |
| CLI 표면 | v1.0 명령은 5개 (`init / doctor / start / stop / spec list / uninstall`) — 추가 금지 | PLAN §9 / §0 GO 조건 ① |
| 비목표 | AI / 텔레메트리 / 자체업데이트 / `nerv config` / fuzzy / `spec list --changes` 코드 자체를 두지 않음 | PLAN §4 비목표 |
| Rust | toolchain 1.85, edition 2021. 변경 시 PLAN §7 + `rust-toolchain.toml` 동시 갱신 | `rust-toolchain.toml` |

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

# nerv-cli / nervd 로컬 실행 (M0-1 이후 의미)
cargo run -p nerv-cli -- init zsh
cargo run -p nerv-daemon

# spec 빌드 (M0-2 이후 의미)
cargo run -p nerv-spec-transpile -- \
    --input vendor/withfig-autocomplete/src/ \
    --output specs-prebuilt/ \
    --only git --only docker --only kubectl
```

## 6. 저장소 구조 (요약)

```
crates/
  nerv-cli/        # `nerv` 바이너리 (clap)
  nerv-daemon/     # `nervd` (tokio + UDS)
  nerv-engine/     # ipc / parser / paths / ranker / spec — 라이브러리
  nerv-shell/      # 마커 블록 init_block / strip_blocks (이미 테스트 4종)
build/spec-transpile/                # withfig TS → JSON 빌더 (swc 도입은 M0-2)
shell-integrations/zsh/_nerv.zsh     # ZLE widget 골격
specs-prebuilt/                      # 빌드 산출 (커밋 X)
vendor/withfig-autocomplete/         # subtree, MIT, pin = aef52acf…
docs/                                # 위 §2 5종
.github/workflows/{ci,upstream-monitor}.yml
```

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

### 7.3 6주차 / 12주차 / M0 종료 체크포인트

PLAN §10 에 명시된 차단 요건 4개를 *직접* 점검하기 전엔 다음 단계 진입 금지:

- 6주차: 50개 spec 중 80%+ 변환 / latency p95 < 25 ms / tmux+2터미널 회귀 / uninstall 흔적 0
- 12주차: 내부 dogfooding 2주
- M0 종료: 산출물 10개 중 1+2+3+5+8 충족 + 6 동급 latency + 7 시나리오 10/12 + 0.5 2/3

미달 시 PLAN §10 의 *의사결정 트리* (Tier C 비율별) 또는 spec 50→30 fallback.

## 8. 자주 빠지는 함정

- ❌ "checkout" 이 "co" 로 시작한다고 믿기 — `c-h-e-c-k…` 입니다. 회귀 테스트 박혀있음.
- ❌ `directories` 크레이트로 `~/Library/Caches/<bundle-id>/` 만들기 — docs 가 `~/Library/Caches/nerv/` 만 인정.
- ❌ `cargo fmt --all` 안 돌리고 PR — CI 가 `--check` 로 거부.
- ❌ 새 의존성을 워크스페이스 deps 에 안 넣고 직접 추가 — 일관성 깨짐.
- ❌ vendor/withfig-autocomplete/ 직접 편집 — 항상 upstream PR 먼저, 막히면 `vendor-patches/`.
- ❌ alternate screen / 24-bit color 사용 — terminal-compat §3 blacklist.
- ❌ `nerv` CLI 에 명령 추가 — 5개로 고정 (PLAN §9 / GO 조건 ①).

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

— 끝. 작업 즐겁게.
