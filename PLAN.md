# Nerv — 셸 자동완성 CLI 기획서 v0.6 (Rust + Fig 엔진 흡수)

> **한 줄 요약**: 사라진 Fig의 인라인 자동완성을 **AWS가 보존한 Fig Rust 코드 (amazon-q-developer-cli-autocomplete)** 위에 macOS + zsh 단일 정적 바이너리로 다시 살린다. 자작 엔진 폐기, Fig 엔진 흡수 + TS → Rust 포팅. AI / 로그인 / 텔레메트리 / Electron 없음.

> **상태**: v0.5.1 의 "Rust 자작" thesis 폐기 → "Fig Rust 엔진 흡수 + TS 부분 Rust 포팅" thesis로 전환. PRD 재작성 draft (구현 진입 전 사용자 승인 대기).

---

## 0. v0.5.1 → v0.6 변경 요약

### 0.1 폐기된 가정 (v0.5.1)

| 항목 | v0.5.1 가정 | v0.6 폐기 사유 |
|------|------------|--------------|
| 엔진 자작 | `nerv-engine/parser.rs` + `nerv-engine/ranker.rs` 직접 작성 | 35% real / 65% stub 진단. Tokenizer + position 추론 + arg state machine 직접 작성 = 3+개월. Fig가 이미 검증한 8년치 로직 (TS) 무시 |
| Spec 변환 자작 | `build/spec-transpile/` 가 swc로 TS → JSON 직접 변환 | Tier 정책은 유지하되 변환기 자작 폐기. Fig `autocomplete-parser` 의 `loadSpec.ts` 가 동일 일을 검증된 형태로 수행 |
| Spec 50개 큐레이션 | `withfig/autocomplete` 에서 상위 50개만 변환 | Fig 엔진 흡수 시 1,484개 전부 자동 지원 (Tier A/B 정적 변환). Tier C (custom generator) 만 v1.1+ |
| Matching = prefix-only 절대 불변 | CLAUDE.md §4 불변식 | Fig 기본 = fuzzy. **prefix-only → 기본 prefix, fuzzy 옵션 (`nerv.toml`)** 로 완화. 불변식 §4 갱신 필요 |
| Rust edition 2021 | `rust-toolchain.toml` | upstream = edition 2024. **2024로 bump 필수**. CLAUDE.md §4 + `rust-toolchain.toml` + PLAN §7 동시 갱신 |

### 0.2 신규 채택

| 항목 | 결정 |
|------|------|
| 상위 의존 | `aws/amazon-q-developer-cli-autocomplete` (Apache-2.0 + MIT, 2026-02-03 활성) 의 Rust crates 흡수 |
| 흡수 방식 | `vendor/aws-autocomplete/` 미수정 mirror (drift 감지) + `crates/nerv-*` 에 `git filter-repo` 로 9개 crate strip+rename 임포트 |
| TS 엔진 처리 | `packages/autocomplete-parser/` + `packages/shell-parser/` 는 subtree 하지 않음. `docs/reference/` 에 복사 → Rust 1:1 포팅 |
| JS generator | M0 = Tier A/B JSON only (현 정책 유지). M1 = **rquickjs** opt-in (Tier C 회복). deno_core 비채택 (~30MB 과잉) |
| Edit-buffer 인터셉트 | M0 = 기존 ZLE widget 유지 (latency 검증 우선). M1 = **figterm opt-in 추가** (bash/fish 도달 + ANSI 엣지케이스 해소). 두 path 사용자 선택 |
| 새 비목표 | deno_core 임베드, fig_desktop webview UI, Q chat/AI 기능 (모두 strip 대상) |

### 0.3 v0.5.1 산출물 처분

| 산출물 | 처분 | 이유 |
|--------|------|------|
| docs 5종 (`uninstall-spec`, `error-states`, `terminal-compat`, `first-5-min`, `spec-conversion-policy`) | **유지 + v1.2 → v1.3 갱신** | 인수 기준은 도메인 로직, 엔진 구현체 무관 |
| `crates/nerv-cli` | **유지** | 5-cmd skeleton 유효. `fig_diagnostic` 흡수 후 `cmd_doctor` 만 재배선 |
| `crates/nerv-daemon` | **부분 폐기** | UDS listener 보존. `stub_complete()` 폐기 → `nerv-engine::complete()` 로 위임 |
| `crates/nerv-engine` | **재작성** | `parser.rs` + `ranker.rs` stub 폐기. `parseArguments.ts` + `shell-parser/parser.ts` Rust 포팅으로 대체 |
| `crates/nerv-shell` | **유지** ✅ | 마커 로직 완성 + 4 unit test. `nerv-integrations` (fig_integrations 흡수) 와 통합 |
| `build/spec-transpile/` | **폐기** | `loadSpec.ts` 포팅이 대체 |
| `shell-integrations/zsh/_nerv.zsh` | **M0 유지 / M1 figterm 도입 시 deprecate** | hybrid stage 전략 |
| `vendor/withfig-autocomplete/` | **유지** (M0-9 산출물) | spec 데이터 소스. subtree pin 갱신만 |

---

## 1. 이름 — `nerv` (변경 없음)

PLAN v0.5 §1 그대로. 충돌 점검 결과 유효.

---

## 2. 배경 (보강)

Fig는 2023년 Amazon에 인수되어 Amazon Q Developer CLI(`q`)로 흡수됐다. 흡수 과정에서 (a) Builder ID 로그인 강제, (b) AI/MCP/에이전트 동거, (c) 반응성 저하가 발생. 2025년 3월 `withfig/fig` 레포는 archived 처리됐고, 메인 `aws/amazon-q-developer-cli` 마저 2026년 chat-only로 축소되며 autocomplete crate가 제거됐다.

그러나 AWS는 **`aws/amazon-q-developer-cli-autocomplete`** 라는 별도 레포에 Fig의 원형 코드 (figterm + autocomplete-parser + 34 crates) 를 Apache-2.0 + MIT 로 보존했다. 2026-02-03 마지막 커밋, 52 star — **잘 알려지지 않은 골드마인**.

Nerv v0.6 의 thesis: *"Fig 엔진은 이미 존재한다. 자작이 아니라 흡수다. 흡수한 뒤 macOS+zsh 단일 정적 바이너리로 응축한다."*

---

## 3. 포지셔닝 (재정렬)

| 구분 | Original Fig | Amazon Q (`q`) | Inshellisense (MS) | **Nerv v0.6** |
|------|--------------|----------------|--------------------|---------------|
| 핵심 기능 | 인라인 자동완성 | AI + 자동완성 + 에이전트 | 인라인 자동완성 | **인라인 자동완성** |
| 인증 | 선택 | **Builder ID 필수** | 없음 | 없음 |
| 설치 | macOS 앱 | 큰 번들 + 권한 | `npm i -g` (Node) | **`brew install`** |
| 런타임 의존 | macOS 앱 (Electron+) | 다수 | **Node.js** | **단일 정적 바이너리** |
| 라이선스 | 일부 OSS | 상용 | MIT | **Apache-2.0** (+ MIT 흡수분) |
| 엔진 출처 | 자체 | 자체 (Q로 변형) | TS 자체 + Fig spec | **Fig Rust 엔진 직계** |
| Spec 수 | 1,484 (자체) | 일부 | 600+ | **1,484 (Tier A/B 변환분, M0 = 50)** |
| 텔레메트리 | 옵트아웃 | 기본 수집 | 없음 | **코드에 없음** |
| 인라인 `?` | 있음 | 부분 | 부분 | **핵심 기능** |
| Tier C generator | JS exec (Wry/V8) | JS exec | JS exec (Node) | **M1 rquickjs opt-in** |

**메시지**: *"Fig는 archived 됐다. Fig 코드는 살아있다. Nerv가 그 코드를 macOS+zsh 단일 바이너리로 다시 묶었다 — 로그인 없이, Node 없이, Electron 없이."*

---

## 4. v1.0 범위

### 포함

- **OS**: macOS (Apple Silicon + Intel, universal binary)
- **셸**: zsh 5.8+ (M0), bash 5+ (M1, figterm opt-in 시)
- **터미널 보장**: iTerm2, Apple Terminal.app — *베스트에포트*: WezTerm, Alacritty, Kitty
- **tmux**: 기본 환경 동작 보장
- **zsh 플러그인 매니저**: oh-my-zsh, zinit, antigen
- **Edit-buffer 인터셉트**: M0 = ZLE widget (현 PoC), M1 = figterm opt-in 추가 (사용자 선택)
- **Spec 소스**: `withfig/autocomplete` 1,484개 → Tier 분류 → A/B 정적 JSON (M0 = 상위 50, M1 = 전체)
- **Matching**: 기본 prefix, `nerv.toml` 로 fuzzy 옵션 (M1)
- **UI**: 인라인 ANSI 팝업 (Fig desktop webview 폐기)
- **인터랙션**: ↑↓ 탐색 / `Tab` 또는 `→` 채택 / `Esc` 닫기 / `?` 인라인 도움말
- **온보딩**: `brew install` → `nerv init zsh` → 30초 안에 첫 자동완성
- **종료**: `nerv uninstall` 흔적 0

### 비목표 (v1)

AI / 자연어, bash·fish·PowerShell·nushell (M0 zsh only — M1 bash figterm 경유 도입 검토), Linux·Windows, 클라우드/팀 동기화, 인증, 텔레메트리, 자체 업데이트 (brew 위임), GUI 설정 앱, `nerv config` 명령, `nerv spec list --changes`, **deno_core 임베드** (rquickjs 만), **fig_desktop webview UI**, Q chat/AI 기능.

### M0 상위 50 spec 후보 풀 (변경 없음)

`git, gh, docker, kubectl, npm, pnpm, yarn, brew, cargo, rustup, go, python, pip, uv, poetry, node, deno, bun, make, cmake, ninja, ssh, scp, rsync, curl, wget, jq, yq, fd, rg, fzf, bat, eza, ls, find, grep, sed, awk, tar, zip, ssh-keygen, git-lfs, terraform, ansible, helm, aws, gcloud, az, vercel, netlify`

M1 = 1,484 전체 자동 Tier 분류.

---

## 5. 핵심 기능 상세

### 5.1 인라인 자동완성

- 매 키 입력 후 디바운스 5–10 ms.
- **Tokenizer**: `shell-parser/parser.ts` (20 KB, 손작성 bash grammar) → Rust 1:1 포팅 (`nerv-engine/src/shell_parser.rs`, `nom` 또는 hand-rolled).
- **Argument parser**: `autocomplete-parser/parseArguments.ts` (32 KB, 800+ LOC 상태머신) → Rust 포팅 (`nerv-engine/src/spec_parser.rs`).
- **Matching**:
  - 기본 = prefix (`git co` → `commit` / `config`, 단 `checkout` 은 `git ch` 에서만)
  - 옵션 = fuzzy (`~/.config/nerv/nerv.toml` 의 `[matching] mode = "fuzzy"`, M1)
- **Tier C 동적 generator UX** (M0 — rquickjs 도입 전):
  ```
  ⤷ 동적 완성은 v1.1에서 지원 예정 — 직접 입력하세요
     ▸ git branch --list 로 후보 확인
  ```
- 같은 라인에서 한 번 표시 후 5초 디바운스.

### 5.2 `?` 인라인 도움말

추천 위에서 `?` → spec의 `description` 펼침. Fig 의 `loadSpec.ts` 가 description 필드 보존하므로 추가 작업 없음.

### 5.3 30초 온보딩 + 5분 리텐션 (변경 없음)

### 5.4 깔끔한 uninstall (변경 없음)

`fig_integrations` 의 `pre.sh` / `post.zsh` 흡수 시 marker 를 `# >>> nerv >>>` ~ `# <<< nerv <<<` 로 교체. fig_integrations 의 snapshot tests 도 흡수 + 마커 갱신.

### 5.5 에러 상태 UX (변경 없음, 5종)

`fig_diagnostic` crate 흡수가 `nerv doctor` 자동 감지 5종을 80% 완성. `error-states.md` 와 정합 확인 후 부족분만 추가.

### 5.6 zsh 플러그인 매니저 호환 (변경 없음)

### 5.7 Spec 변환 정책 (재정의)

`spec-conversion-policy.md` 의 Tier A/B/C 정책 유지. 단 변환 주체 변경:

- v0.5.1: 자작 `build/spec-transpile/` 가 swc 로 TS → JSON 변환
- v0.6: **Fig `loadSpec.ts` (TS) → Rust 포팅한 `nerv-engine::load_spec()` 가 빌드타임 + 런타임 양쪽 처리**
  - 빌드타임: 1,484개 TS spec → JSON serialize (Tier A/B)
  - 런타임: 사용자 추가 spec (`~/.config/nerv/specs/*.ts`) lazy load (M1+)
- Tier C (`generator.custom`, `postProcess`) 는 M0 drop, M1 rquickjs 로 회복

### 5.8 figterm opt-in (신규, M1)

M0 ZLE widget 으로 latency 검증 완료 후, M1 에서 `figterm` (rebrand → `nerv-pty`) crate 흡수.

- 설치: `nerv init zsh --pty` 옵션 시 `~/.local/bin/nerv-pty` 배포 + `pre.sh` 에 `exec -a "<shell> (nerv-pty)" nerv-pty` 추가
- 효과: bash/fish 도달 + prompt boundary 정확 + alacritty terminal state 로 cursor 정확
- 비용: PTY shim Apple Developer ID 서명 + notarization 필수 (M0-8 인프라 재활용), ~20 MB binary, universal (arm64 + x86_64)
- ZLE 와 상호 배타: `NERV_PTY=1` 환경변수 감지 시 ZLE widget 자동 비활성

---

## 6. 아키텍처

### 6.1 M0 (ZLE path)

```
┌─ Terminal (iTerm2 / Terminal.app + tmux) ─────────────────────────┐
│   사용자 키 입력                                                    │
│        ▼                                                          │
│   ┌─────────────────────────────────────────────┐                 │
│   │  zsh ZLE widget (shell-integrations/zsh/)    │ ── LBUFFER ──▶  │
│   │  (oh-my-zsh / zinit / antigen 호환 패키징)     │ ◀── JSON ──     │
│   └────────────┬────────────────────────────────┘                 │
│                │ Unix domain socket (~/Library/Caches/nerv/)      │
│                ▼                                                  │
│   ┌─────────────────────────────────────────────┐                 │
│   │  nervd  (Rust 데몬, 흡수 + 자작 통합)            │                │
│   │  ┌─────────────────────────────────────────┐ │                │
│   │  │ nerv-ipc (← fig_ipc)                     │ │                │
│   │  │ nerv-engine (자작 + ← parseArguments port)│ │                │
│   │  │ nerv-proto (← fig_proto, strip)          │ │                │
│   │  │ Spec store: ~/Library/Caches/nerv/specs/ │ │                │
│   │  └─────────────────────────────────────────┘ │                │
│   └─────────────────────────────────────────────┘                 │
└───────────────────────────────────────────────────────────────────┘
```

### 6.2 M1 (figterm path, opt-in)

```
┌─ Terminal ────────────────────────────────────────────────────────┐
│   사용자 키 입력                                                    │
│        ▼                                                          │
│   ┌─────────────────────────────────────────────┐                 │
│   │  nerv-pty (← figterm) — PTY shim             │ ── 인라인 ANSI   │
│   │  alacritty_terminal 으로 screen state 추적     │                 │
│   └────────────┬────────────────────────────────┘                 │
│                │ UDS (nerv-ipc)                                   │
│                ▼                                                  │
│   nervd (M0 와 동일)                                                │
└───────────────────────────────────────────────────────────────────┘
```

**런타임 JS 엔진**: M0 없음 (Tier A/B JSON only). M1 = rquickjs opt-in (Tier C 호출 시점만 spin-up, idle 시 메모리 영점).

**ANSI 정책 (변경 없음)**: alternate screen 미사용, raw cursor save/restore + line clearing.

---

## 7. 기술 스택

- **언어**: Rust **edition 2024** (upstream 정합), rustc 1.85+
- **CLI**: `clap` v4 (변경 없음)
- **IPC**: `nerv-ipc` (← `fig_ipc`) + `tokio` + `interprocess`
- **Proto**: `nerv-proto` (← `fig_proto`, strip — figterm + local 메시지만)
- **Terminal state (M1)**: `nerv-term` (← `alacritty_terminal`)
- **TUI/ANSI**: `crossterm` (변경 없음)
- **Spec 파서**: `nerv-engine` 자작 (← TS `autocomplete-parser` + `shell-parser` 포팅)
- **JS engine (M1 opt-in)**: `rquickjs` (~1 MB, sandboxed)
- **저장소**: in-memory `HashMap<prefix, count>` (M0). SQLite v1.1 (변경 없음)
- **설정**: TOML 직접 편집 (`~/.config/nerv/nerv.toml`)
- **테스트**: `cargo nextest`, `expectrl` (zsh + tmux), `fig_integrations` 의 snapshot tests 흡수
- **배포**: Homebrew tap (`nerv-sh/homebrew-tap`)
- **서명/공증**: macOS Developer ID + notarization (universal binary)

---

## 8. 저장소 구조

```
nerv/
├─ Cargo.toml                                # workspace, resolver=3, edition=2024
├─ rust-toolchain.toml                       # 1.85+, edition2024
├─ crates/
│  # 기존 (보존)
│  ├─ nerv-cli/                              # 5-cmd CLI (fig_diagnostic 흡수)
│  ├─ nerv-daemon/                           # UDS listener (stub_complete 폐기)
│  ├─ nerv-engine/                           # 자작 + TS 포팅분 통합
│  │    ├─ src/
│  │    │   ├─ shell_parser.rs               # ← shell-parser/parser.ts 포팅
│  │    │   ├─ spec_parser.rs                # ← autocomplete-parser/parseArguments.ts 포팅
│  │    │   ├─ spec_loader.rs                # ← autocomplete-parser/loadSpec.ts 포팅
│  │    │   ├─ ranker.rs                     # 기존 보존 (prefix), fuzzy 옵션 M1
│  │    │   └─ paths.rs                      # 기존 보존
│  │    └─ tests/                            # parseArguments 회귀 테스트 (Fig 시나리오 흡수)
│  └─ nerv-shell/                            # 마커 로직 (보존) + nerv-integrations 통합
│
│  # 신규 (filter-repo 흡수)
│  ├─ nerv-pty/                              # ← figterm (M1 opt-in)
│  ├─ nerv-term/                             # ← alacritty_terminal
│  ├─ nerv-ipc/                              # ← fig_ipc
│  ├─ nerv-proto/                            # ← fig_proto (strip)
│  ├─ nerv-integrations/                     # ← fig_integrations (marker 교체)
│  ├─ nerv-os/                               # ← fig_os_shim
│  ├─ nerv-util/                             # ← fig_util (Q_→NERV_)
│  ├─ nerv-settings/                         # ← fig_settings (경로 재정의)
│  ├─ nerv-log/                              # ← fig_log
│  └─ nerv-diag/                             # ← fig_diagnostic
│
├─ vendor/
│  ├─ withfig-autocomplete/                  # 기존 subtree (TS specs 1,484, ISC)
│  └─ aws-autocomplete/                      # 신규 subtree (Apache+MIT, 미수정 mirror, drift 감지)
│
├─ vendor-patches/
│  ├─ upstream/                              # withfig/aws 양쪽 cherry-pick (M1)
│  └─ self/                                  # 우리 fork patches
│
├─ docs/reference/                           # 신규: TS 포팅 1:1 참조용
│  ├─ parseArguments.ts                      # 복사본 (포팅 가이드)
│  ├─ shell-parser.ts                        # 복사본
│  └─ loadSpec.ts                            # 복사본
│
├─ docs/
│  ├─ uninstall-spec.md      v1.3           # marker 교체 반영
│  ├─ error-states.md        v1.3           # fig_diagnostic 흡수 반영
│  ├─ terminal-compat.md     v1.2           # figterm opt-in 명시
│  ├─ first-5-min.md         v1.2           # 변경 최소
│  └─ spec-conversion-policy.md v1.3        # Tier 정책 + rquickjs M1 옵트인
│
└─ .github/workflows/{ci,upstream-monitor,upstream-prs}.yml
```

**별도 레포 (M1)**: `nerv-sh/homebrew-tap`, `nerv-sh/nerv-omz`, `nerv-sh/nerv-zsh` (변경 없음)

---

## 9. CLI 명령 (변경 없음 — 5개 + 내부 `_complete`)

```bash
nerv init zsh           # 셸 통합 스크립트 출력 (--pty 옵션은 M1)
nerv doctor             # 환경 진단 (nerv-diag 흡수로 80% 완성)
nerv start | stop       # nervd 라이프사이클
nerv spec list          # 내장 spec 목록
nerv uninstall          # 깔끔한 제거
```

**v1.0 제외 (변경 없음)**: `telemetry`, `update`, `spec install`, `spec update`, `spec dev`, `feedback`, `config`.

---

## 10. 로드맵

### M0 — 흡수 스파이크 (6주, +2주 Developer ID 선행 검증)

v0.5.1 의 M0 (자작 4주) 폐기. 새 M0 산출물 8개:

1. ✅ **`vendor/aws-autocomplete/` subtree pin** + Apache+MIT NOTICE.
2. ✅ **`git filter-repo` 로 10개 crate 추출** — `figterm`(nerv-pty, M1 opt-in 으로 workspace 보류), `alacritty_terminal`(nerv-term), `fig_ipc`(nerv-ipc), `fig_proto`(nerv-proto strip), `fig_integrations`(nerv-integrations strip), `fig_util`(nerv-util strip), `fig_settings`(nerv-settings strip), `fig_os_shim`(nerv-os), `fig_log`(nerv-log strip), `fig_diagnostic`(nerv-diag). 15 active crates `cargo check --workspace` 통과 (nerv-pty 는 chunk 3d 의존성 strip 마무리 후 합류).
3. ✅ **Rust edition 2024 bump** — workspace + 모든 crate + rust-toolchain.toml 동시.
4. ✅ **`shell-parser/parser.ts` → `nerv-engine::shell_parser` Rust 포팅** — bash grammar 1:1, 124 회귀 테스트 (e2e 22 + 단위 102).
5. ✅ **`parseArguments.ts` → `nerv-engine::spec_parser` Rust 포팅** — chunks 1-5 완료 (types + static helpers + state machine + token shape classifier + matcher). 174 단위 + 11 fixture integration test.
6. ✅ **`loadSpec.ts` → `nerv-engine::spec_loader` 포팅** + `build-specs` 바이너리 + `nerv-engine::complete` 파이프라인 + daemon wire-up. TS→JSON 변환 자체 (1,484 spec) 는 M1 (외부 node 스크립트 또는 rquickjs Tier C 시간 후). 현재 hand-rolled fixture (git, echo) 로 end-to-end 검증.
7. ✅ **ZLE → UDS → `nerv-engine::complete()` → 인라인 ANSI** — latency 측정 결과: IPC p95 **0.052 ms**, CLI cold-start p95 **4.07 ms** (25 ms 예산 16%). ZLE widget `_nerv.zsh` 의 `insertion\tdisplay\tdesc` 포맷 호환 확인. (인라인 ANSI 자체는 widget 이 이미 구현 — engine + widget 통합 완료.)
8. ⏳ **Developer ID 서명/공증 선행 검증** (v0.5.1 의 M0-8 그대로) — `fn main(){}` 빌드 + sign + notarize + Homebrew tap 설치 e2e. **No-Go 차단 요건** — Apple Developer 계정 / 인프라 의존.

**M0 Go/No-Go**: 1+2+3+7+8 동시 충족. 4+5+6 에서 상위 50 spec 의 `git status / log / checkout` + `docker ps / build / run` + `kubectl get / describe / logs` 시나리오 통과 시 GO.

**현재 상태 (2026-05-23)**: 1-7 완료. 8 만 남음 (서명/공증 인프라). Go/No-Go 시나리오 4종 모두 통과, hand-rolled fixture 9종 + 43 integration test. CLI 표면 5/5 완성 (init / doctor / start / stop / spec list / uninstall). **TS→JSON 변환 파이프라인 동작** (`tools/ts-to-json/` bun-based): 715 spec 변환 성공 (440 Tier A / 6 B / 246 C), daemon 로드 707 spec. loadSpec 재귀 인라인 인프라 존재 (depth=0 default — cache 압축 + lazy load 도입 후 활성). 에러 UX E1-E4 shell-side 구현 (E5 manifest 도입 후). 결과적으로 M1 0-4주차 작업의 약 70% 가 M0 단계에서 선행 완료.

**M0-2 Fig 흡수 결과 의사결정 트리**:

| 결과 | 결정 |
|------|------|
| 9개 crate 모두 strip 후 `cargo check` 통과 | 그대로 M1 진입 |
| 1–3개 crate 의존성 정리 실패 | 해당 crate 별 wrapper crate 작성 (1주 추가) |
| 4+개 crate 실패 | strip 전략 재검토 — `fig_desktop` 의 transitive deps 가 의심 |
| `edition 2024` workspace 충돌 (clap 4.5 미지원 등) | 임시 `2021` 유지 + 흡수 crate 별 격리 |

### M1 — v1.0 (14주, 6주차 + 10주차 체크포인트)

v0.5.1 의 M1 (16주) 단축. Fig 엔진 흡수로 0–6주차 작업 80% 제거.

- **0–4주차**: `nerv-engine` 자작 부분과 흡수 crate 통합 마무리. 50 spec 변환 파이프라인 완성. ZLE 안정화. 인라인 `?`. 에러 상태 UX 5종. **upstream PR 흡수 인프라** (`upstream-prs.yml` + `vendor-patches/{upstream,self}/` + 첫 cherry-pick 1건 시연 — withfig 측 + aws 측 양쪽).
  - **4주차 체크포인트**: 50개 spec 시나리오 통과 / latency p95 < 25 ms / tmux+2터미널 회귀 / uninstall 흔적 0.
- **5–10주차**: 1,484 spec 전체 Tier A/B 자동 변환 + 변환률 측정. `nerv doctor` 자동 감지 5종 완성. zsh 플러그인 매니저 패키지 (`nerv-omz`, `nerv-zsh`) e2e. **figterm (`nerv-pty`) opt-in 통합** + 서명/공증 자동화. **rquickjs Tier C opt-in** PoC.
  - **10주차 베타 체크포인트**: 내부 dogfooding 2주.
- **11–14주차**: Homebrew tap 공개, 30초 KPI 자동 측정 CI, 매니저 e2e CI, 문서 6종 v1.4 완비, v1.0 출시.

### v1.0 출시 후 (변경 없음 — North Star 4종)

### v1 이후 (재정렬)

- v1.1: **rquickjs Tier C 일반 공급** (M1 opt-in → 기본). bash spec generator 시나리오 대거 회복
- v1.1: SQLite frecency 학습
- v1.1: `nerv spec list --changes`
- v1.2: **bash 지원** (figterm 경유, M1 인프라 재활용)
- v1.2: fuzzy matching 정식 기능
- v1.3: Linux (figterm Linux PTY 지원 — `fig_remote_ipc` strip 분 일부 회복 검토)
- v2.0: fish
- v2.x: spec 레지스트리, `nerv config`
- **AI / 자연어 모드 제거 유지** — *"AI 없음"* 포지셔닝과 정합

---

## 11. 터미널 호환성 매트릭스 (변경 없음)

PLAN v0.5 §11 그대로. figterm opt-in 도입 시 `alacritty_terminal` 의 screen state 추적으로 베스트에포트 3종 → 보장 승격 검토 (M1 10주차).

---

## 12. 경쟁자 벤치마크 (변경 없음 — Inshellisense 정량)

---

## 13. 리스크 (재정리)

| 리스크 | 심각도 | 대응 |
|-------|-------|------|
| **`parseArguments.ts` 32 KB Rust 포팅 난이도** | **높음 ↑↑ (신규)** | M0-5 부분 포팅 (50 spec 시나리오만) → M1 전체. Fig 의 TS 회귀 테스트 모두 Rust port → 동작 등가 검증. `insta` snapshot 테스트로 출력 diff 방지 |
| **edition 2024 호환 깨짐** (clap, tokio, prost 등 일부 미지원) | **높음 ↑ (신규)** | M0-3 직후 즉시 `cargo check --workspace` — 충돌 발견 시 격리 |
| **Apache-2.0 + MIT vs 우리 Apache-2.0 정합** | 낮음 | NOTICE 명시, 흡수 crate별 `LICENSE` 파일 보존 |
| 1인 개발 + 14주 M1 → 번아웃 | 높음 | 4/10주차 체크포인트, 외부 기여 적극 수용 |
| Developer ID 서명/공증 (M0-8) | 높음 | 선행 검증 (변경 없음) |
| Tier C 동적 generator 한계 (M0 drop) | 중간 | M1 rquickjs opt-in 회복, M0 = §5.1 힌트 UX |
| `withfig/autocomplete` archived (1차 spec 소스) | 중간 | spec-conversion-policy §5.3 (2주 모니터, 3개월 부재 시 fork) |
| **`aws/amazon-q-developer-cli-autocomplete` archived 또는 EOL** (신규) | 중간 | 마지막 커밋 2026-02-03 활성 — `upstream-monitor.yml` 에 추가 (2주 cron, 90일 부재 시 fork) |
| **figterm 흡수 시 attack surface 증가** (신규) | 중간 | M1 opt-in 으로 격리, ZLE path 와 상호 배타 |
| tmux/터미널 ANSI 차이 | 중간 | M0-7 회귀 (M1 figterm 시 alacritty_terminal 로 해소) |
| zsh 플러그인 매니저 충돌 | 중간 | M1 5–10주차 e2e |
| `nerv` 명명 (Nerves) | 낮음 | 일관 표기 |

---

## 14. 라이선스 & 거버넌스

- 본 프로젝트: **Apache-2.0**
- `vendor/withfig-autocomplete/`: **ISC** 보존 (M0-9 확인 결과 — v0.5의 "MIT" 표기는 오류, ISC가 정확)
- `vendor/aws-autocomplete/`: **Apache-2.0 + MIT dual** 보존, NOTICE 명시, subtree pin
- 흡수된 `crates/nerv-{pty,term,ipc,proto,integrations,os,util,settings,log,diag}/`: 원본 라이선스 헤더 보존, `AUTHORS.md` 에 upstream 기여자 명시 (M1 0–4주차 작업)
- DCO (`Signed-off-by`) 시작
- 거버넌스: 초기 BDFL → 기여자 누적 시 재검토

---

## 15. 다음 액션 (착수 직후 1–2주)

1. ✅ GitHub org `nerv-sh` (v0.5.1)
2. ✅ Apple 개발자 프로그램 가입 (v0.5.1)
3. ✅ 5종 인수 기준 문서 (v0.5.1, v1.1 → v1.3 갱신 예정)
4. ✅ cargo workspace + 5 crates 스캐폴딩 (v0.5.1)
5. ✅ M0-9 — `withfig/autocomplete` subtree pin (v0.5.1)
6. **신규 — PRD v0.6 사용자 승인 → PLAN.md swap**
7. **신규 — `vendor/aws-autocomplete/` subtree add + NOTICE 갱신**
8. **신규 — `crates/nerv-engine` parser/ranker stub 폐기 PR (cleanup)**
9. **신규 — Rust edition 2024 bump (CLAUDE.md §4 + workspace + rust-toolchain.toml)**
10. **신규 — `git filter-repo` 로 9개 crate 추출 PoC**
11. **신규 — `shell-parser/parser.ts` → `nerv-engine::shell_parser` Rust 포팅 (M0-4)**

---

*v0.6 draft — v0.5.1 의 "Rust 자작 엔진" thesis 폐기, "Fig Rust 엔진 흡수 + TS Rust 포팅" thesis 채택. AWS 가 보존한 `amazon-q-developer-cli-autocomplete` (Apache+MIT, 활성) 의 9개 crate 흡수 + 2개 핵심 TS 모듈 (`parseArguments`, `shell-parser`) Rust 포팅. 일정 단축 (M0 4→6주, M1 16→14주), spec 50→1,484 자동 도달, 단일 정적 바이너리 thesis 유지. 사용자 승인 후 PLAN.md 로 swap, docs 5종 v1.3 갱신, CLAUDE.md §4 불변식 갱신 (prefix-only → 기본 prefix + fuzzy 옵션, edition 2021→2024).*
