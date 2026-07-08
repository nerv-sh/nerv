# Nerv — 셸 자동완성 CLI 기획서 v0.6 (Rust + Fig 엔진 흡수)

> **한 줄 요약**: 사라진 Fig의 인라인 자동완성을 **AWS가 보존한 Fig Rust 코드 (amazon-q-developer-cli-autocomplete)** 위에 macOS + zsh 단일 정적 바이너리로 다시 살린다. 자작 엔진 폐기, Fig 엔진 흡수 + TS → Rust 포팅. AI / 로그인 / 텔레메트리 / Electron 없음.

> **상태 (2026-05-29 / M0-8 정책 갱신 2026-07-04)**: M0 종료 (**8/8 완료** — M0-8 = Homebrew-only 배포 정책 확정, 공증 요건 폐기). M1 진입 — alpha.7~alpha.10 후보 누적: fuzzy matching opt-in, FSEvents push hot-reload, aws Phase 1+2 (680 generator 회복: ScriptWithJsonPath 591 + AwsList 89), Homebrew tap 자동화, icon glyph width 보정. 흡수 crate 브랜드 strip 단계: env_var 모듈 (Q_*→NERV_*) + dead 메서드 14개 삭제 + CLI binary "q"→"nerv" / PRODUCT_NAME "Amazon Q"→"Nerv" / bundle id sh.nerv.nerv / "# Q pre block" 마커 / qterm.* 설정 키 → pty.* / AI translate hook 제거. Q/Amazon 사용자 표면 잔존 0. 1,484 spec 자동 변환 (715 loaded, depth=1, gzip 10×). 558+ workspace test, CI 1차 budget 90% 도달 시 workflow_dispatch 게이트.

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
| JS generator | M0 = Tier A/B JSON only. M1 = **rquickjs** Tier C 회복. deno_core 비채택 (~30MB 과잉). **결정 뒤집힘 (2026-07-08): ~~출시 미동봉 (e2e 0%)~~ → 출시 동봉.** 2026-07-07 머신러리 수정으로 실행률 0% → 82% settle (aws 746/749), 재측정 결과 JS 머신러리 2-3ms (25ms 예산 내) + 출시 빌드에 quickjs 누락되어 있던 것 확인 → `release.yml --features nerv-cli/quickjs,nerv-daemon/quickjs` 동봉 (+0.78MB). 남은 tail(니치툴 module 헬퍼) = esbuild 번들러 deferred. 상세 = `docs/findings/tier-c-quickjs-e2e.md` |
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

### 0.4 v0.6 구현 진척 (2026-05 시점)

PRD 승인 이후의 핵심 마일스톤. PLAN.md 정책은 변경 없음 — *진척 추적용* 단일 출처는 `CLAUDE.md §3` 이고 PLAN.md 는 큰 묶음만 박는다.

| 묶음 | 상태 | 비고 |
|------|------|------|
| M0 흡수 (8개 산출물) | **8/8 완료** | M0-8 = Homebrew-only 배포 정책 확정 (공증 요건 폐기, 2026-07-04). ad-hoc 서명 (Rust/linker 자동) + brew quarantine 미부착으로 Gatekeeper 경고 없음. Developer ID 공증은 직접-tarball 배포 추가 시 opt-in |
| TS→JSON 변환 파이프라인 | ✅ `tools/ts-to-json/` | bun 기반, 715 spec, Tier A 440 / B 6 / C 246 분류, depth=1 (`aws ec2 <verb>` 등) |
| Spec store | ✅ lazy load + FSEvents hot-reload + gzip 10× 압축 | `~/Library/Caches/nerv/specs/*.json.gz` |
| aws closure 회복 (Phase 1) | ✅ `Generator::ScriptWithJsonPath` | 591 generator (iam list-users / ec2 describe-instances) — rquickjs 우회 |
| aws closure 회복 (Phase 2) | ✅ `Generator::AwsList` | 89 token-aware generator (cloudwatch list-metrics 등) |
| well-known Tier C→B 회복 | ✅ 5 recognizer | npm scripts / Filepaths / ZoxideQuery / function-form script-fn / JSON output |
| Matching opt-in | ✅ `[matching] mode = "fuzzy"` | M1 정책 — config 변경엔 데몬 재시작 필요 |
| Fig parity batch | ✅ 6 스키마 필드 | isPersistent / priority / requiresSeparator / icon (width==2 강제) / flagsArePosixNoncompliant / filterStrategy / getQueryTerm |
| Homebrew tap 자동화 | ✅ `nerv-sh/homebrew-tap` | release 발행 시 Formula 자동 bump |
| Release 인프라 | ✅ `release.yml` + ARM-only tarball | `v*.*.*` tag → GitHub Release |
| 흡수 crate 브랜드 strip | ✅ 진행 중 (active surface) | Q_*→NERV_* (env vars, methods), CLI binary "q"→"nerv", PRODUCT_NAME "Amazon Q"→"Nerv", bundle id `sh.nerv.nerv`, "# Q pre block" 마커 자동 reflow, qterm.* 설정 키 → pty.*, AI translate hook 제거 (CLAUDE.md §4 invariant) |
| CI 정책 | ⚠️ workflow_dispatch only (2026-06-01 reset 까지) | 1차 무료 budget 90% 도달, restore 한 줄 패치 |

**아직 인 진척 (M1 이후)**:
- ~~M0-8 서명/공증~~ → **폐기 (2026-07-04)**: Homebrew-only 배포로 공증 불필요. Developer ID 공증은 직접-tarball 배포 추가 시 opt-in (`scripts/sign-notarize-e2e.sh` 스캐폴드 보존)
- aws Phase 3 (closure 가 token 에 의존하는 624 케이스 — rquickjs opt-in 필요)
- ✅ **bash + fish 지원 (PTY 경로, MVP)**: `nerv init {bash,fish}` → `_nerv-pty.{bash,fish}` (OSC 697 markers, `Shell={bash,fish}` 필수). 둘 다 ZLE 없음 → PTY opt-in (`NERV_PTY=1`) 전용. ghost 작동 (`scripts/e2e-pty-{bash,fish}.py` PASS). bash=PROMPT_COMMAND, fish=`--on-event fish_prompt`+prompt wrap. **PreExec 구현**: fish=`--on-event fish_preexec` (clean), bash=gated DEBUG trap (2-guard: `PROMPT_SHOWN` 으로 startup 발화 차단 + `PREEXEC_DONE` 으로 커맨드당 1회). submit 시 발화 e2e 검증. **fish 주의**: (1) fish 4.x 는 터미널 capability 쿼리(XTGETTCAP/DA/OSC11) 응답 대기 — 실 터미널은 응답하나 e2e harness 는 emulate 필요. (2) fish 자체 grey autosuggestion 과 nerv ghost 공존 (사용자가 한쪽 비활성 선택 가능). `init_block` 이 fish 용 `| source` 문법 emit (POSIX `eval` 아님)
- Linux / Windows 지원 (큼)
- figterm PTY shim opt-in 실런타임 (`NERV_PTY=1`) — main.rs 980줄 + figterm-ipc + remote-ipc 정합 필요
- E5 manifest, spec depth=2+ (압축으로 무난, memory cost 평가)

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
  - 옵션 = fuzzy (`~/.config/nerv/nerv.toml` 의 `[matching] mode = "fuzzy"`) — 활성 시 case-insensitive 서브시퀀스 (`git chk` → `checkout`). 데몬 부팅 시 1회 로드, config 편집은 재시작 필요. spec per-arg `filterStrategy: "substring"` 은 user mode 무관 우선.
- **동적 generator**: Tier B (정적 shell command, 예 `git branch --list`) 는 Rust 가 직접 spawn (200ms timeout + TTL 5s LRU 64 cache). well-known Tier C (kubectl/docker/aws/npm scripts/filepaths 등) 는 signature recognizer 로 Rust-native 회복. 남은 closure-only tail 은 `rquickjs` Tier C (2026-07-08 출시 동봉, 실행률 82% settle — `docs/findings/tier-c-quickjs-e2e.md`). M0 의 "동적 완성은 v1.1 지원 예정 — 직접 입력하세요" 힌트 UX (`Response::DynamicHint` / `LimitedArg`) 는 작동하는 동적완성이 대체 → 삭제 (CLAUDE.md §3, first-5-min §8).

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
- 비용: ~20 MB binary, universal (arm64 + x86_64). 배포는 Homebrew (ad-hoc 서명 + quarantine 미부착으로 공증 불필요 — M0-8 정책). 직접-tarball 추가 시에만 Developer ID 공증 opt-in
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
- **서명/배포**: ad-hoc 서명 (Rust/linker 자동) + Homebrew tap (brew quarantine 미부착 → Gatekeeper 경고 없음). Developer ID 공증은 직접-tarball 배포 추가 시 opt-in (M0-8 정책, 2026-07-04)

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

### M0 — 흡수 스파이크 (6주) — ✅ 완료 (8/8)

v0.5.1 의 M0 (자작 4주) 폐기. 새 M0 산출물 8개:

1. ✅ **`vendor/aws-autocomplete/` subtree pin** + Apache+MIT NOTICE.
2. ✅ **`git filter-repo` 로 10개 crate 추출** — `figterm`(nerv-pty, M1 opt-in 으로 workspace 보류), `alacritty_terminal`(nerv-term), `fig_ipc`(nerv-ipc), `fig_proto`(nerv-proto strip), `fig_integrations`(nerv-integrations strip), `fig_util`(nerv-util strip), `fig_settings`(nerv-settings strip), `fig_os_shim`(nerv-os), `fig_log`(nerv-log strip), `fig_diagnostic`(nerv-diag). 15 active crates `cargo check --workspace` 통과 (nerv-pty 는 chunk 3d 의존성 strip 마무리 후 합류).
3. ✅ **Rust edition 2024 bump** — workspace + 모든 crate + rust-toolchain.toml 동시.
4. ✅ **`shell-parser/parser.ts` → `nerv-engine::shell_parser` Rust 포팅** — bash grammar 1:1, 124 회귀 테스트 (e2e 22 + 단위 102).
5. ✅ **`parseArguments.ts` → `nerv-engine::spec_parser` Rust 포팅** — chunks 1-5 완료 (types + static helpers + state machine + token shape classifier + matcher). 174 단위 + 11 fixture integration test.
6. ✅ **`loadSpec.ts` → `nerv-engine::spec_loader` 포팅** + `build-specs` 바이너리 + `nerv-engine::complete` 파이프라인 + daemon wire-up. TS→JSON 변환 자체 (1,484 spec) 는 M1 (외부 node 스크립트 또는 rquickjs Tier C 시간 후). 현재 hand-rolled fixture (git, echo) 로 end-to-end 검증.
7. ✅ **ZLE → UDS → `nerv-engine::complete()` → 인라인 ANSI** — latency 측정 결과: IPC p95 **0.052 ms**, CLI cold-start p95 **4.07 ms** (25 ms 예산 16%). ZLE widget `_nerv.zsh` 의 `insertion\tdisplay\tdesc` 포맷 호환 확인. (인라인 ANSI 자체는 widget 이 이미 구현 — engine + widget 통합 완료.)
8. ✅ **배포 서명 정책 = Homebrew-only (공증 요건 폐기, 2026-07-04)** — **결정**: v1.0 배포는 Homebrew tap 단독. `brew install` 은 다운로드에 `com.apple.quarantine` xattr 를 붙이지 않으므로 **Gatekeeper "미확인 개발자" 경고 자체가 발생 X** → Developer ID 공증 불필요. arm64 실행에 필요한 서명은 Rust/linker 가 자동으로 붙이는 **ad-hoc 서명**(`adhoc,linker-signed`, 검증: `codesign -dv target/release/nerv`)으로 충족. 유료 Apple Developer 계정 ($99/년) 및 개인/회사 인증서 불필요. `scripts/sign-notarize-e2e.sh` 는 미래에 직접-tarball 배포 + 공증을 추가할 때를 위한 opt-in 스캐폴드로 보존 (Apple Developer 계정 확보 시 재활성). **직접 tarball 다운로드 사용자** (Homebrew 미경유) 만 첫 실행 시 경고 → README/Formula 에 `xattr -dr com.apple.quarantine <path>` 안내. **공증은 v1.0 후 필요 시 추가** (No-Go 아님).

**M0 Go/No-Go**: 1+2+3+7+8 동시 충족 (8 = Homebrew-only 배포 정책 확정). 4+5+6 에서 상위 50 spec 의 `git status / log / checkout` + `docker ps / build / run` + `kubectl get / describe / logs` 시나리오 통과 시 GO. → **전 항목 충족, M0 GO**.

**현재 상태 (2026-05-26 → M0-8 정책 확정 2026-07-04)**: **M0 1-8 전부 완료** (8 = Homebrew-only 배포 정책, 공증 요건 폐기). Go/No-Go 시나리오 4종 통과, hand-rolled fixture 9종 + 43 integration test + 453 workspace test. CLI 5/5 완성.

- **TS→JSON 변환 + loadSpec depth=1** (715 spec, `aws ec2 <verb>` 등 nested 커버)
- **SpecRegistry lazy + mtime hot-reload + gzip 압축 (10×)** — daemon 즉시 기동, 10MB 풀세트
- **Tier B 실행** — script spawn + TTL 5s LRU 64 cache + ANSI/git-marker sanitization
- **well-known Tier C 회복 5종** — PackageJsonScripts (npm/yarn/pnpm/bun/rushx/nr) + Filepaths (cd/cat/ls/59 spec, folders_only 감지) + ZoxideQuery (z/zoxide, `~/.z` 폴백 + fuzzy substring) + aggressive script-fn 회복 (function-form script stub call → **aws 1006 / docker 117 / docker-compose 23 / kubectl 27 / gh 23 generator** 회복) + JSON output 자동 추출 (`gh --json`, `kubectl -o json` 등)
- **yarn-shorthand** — root args generator subcommand emit 머지 (additive + dedupe)
- **inline ghost text** — `POSTDISPLAY` dim grey, Right-arrow accept (line 끝일 때). **히스토리 우선** (zsh-autosuggestions 방식, `${history[(r)…]}` 최신 매치) → 없으면 spec 토큰 ghost. bare command (`pwd`) 도 표시
- **frecency ranking** — per-spec TSV, count>=2 부터 boost, time decay, daemon post-sort
- **cwd-aware IPC** — `cd` 시 daemon 재시작 불필요
- **UTF-8 char boundary 클램프** — 한글/CJK/emoji 입력 panic 방지
- **widget UX** — sliding window (`MAX_VIS=min(LINES-6,10)`), footer `[k/total]` 항상, 우측 border 정렬 (off-by-2 fix), Tab/Shift-Tab/Arrow wrap-cycle, PageUp/PageDown 창 단위 점프 (edge clamp, ZLE+PTY 동일), precmd 재바인딩 (Q hijack 방지), description 매행 → footer 단일 라인 (Fig style)
- **dogfood UX 배치 (2026-07, PR #13)** — `↩ Immediately execute` 센티넬 (기본 선택; 빈 토큰=Enter 실행, 부분 입력=첫 매치 하이라이트), 히스토리 인라인 ghost, Fig arg hints (`push [remote] [branch]`), no-op 완성 제거 (`git status` 재추천 X), zoxide 이름-매치 우선 랭킹, p10k 앵커 가드, ghost grey (region_highlight), 빈 버퍼 화살표→history, 팝업 flicker 제거. 엔진: cargo `-p` + aws 591 ScriptWithJsonPath fix. e2e 5종 + 유닛. 상세 = CLAUDE.md §3
- **에러 UX E1-E4 구현**
- **CI** — rust 1.85 핀 + protoc preinstall + build-specs-smoke + ts-to-json bun job + **ARM64-only** (Intel queue 너무 길어 drop)
- **release.yml** — `v*.*.*` tag push → macos-14 빌드 + tarball + sha256 + GitHub Release 자동 생성
- **e2e-isolated.sh 스모크 하네스** — 격리 `/tmp/nerv-test/.zshrc` zsh 진입

**완전한** Tier C (closure body 임의 JS 실행) 는 deno_core 금지 + closure 가 token/scope 컨텍스트 잡아서 JSON serialize 불가 → rquickjs 도입 전까지 deferred. **단 패턴 기반 데이터 캡처 (`ScriptWithJsonPath`, `KubectlResources`, `PackageJsonScripts` 등) 는 별개 우회로** — ts-to-json 이 closure 시그니처 인식 → 의미를 추출하면 Rust 가 JS 없이 동등 결과. M1 0-10주차 작업 대부분 선행 완료 — figterm opt-in (M1 5-10주차) + 서명/공증 인프라 (M0-8) 만 남음 (Homebrew tap 인프라는 alpha 단계 완료).

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
  - **4주차 체크포인트** ✅ **PASS (2026-06-07)**: 50개 spec 시나리오 통과 (`scenario_specs.rs` 54/54, git/docker/kubectl/npm/cargo/gh/brew/go/terraform/helm/pip) / latency p95 **0.055 ms** (<25 ms, 455× 여유) / tmux+2터미널 회귀 (`scripts/e2e-tmux-2term.sh` — cwd 격리 + 20-way 동시성) / uninstall 흔적 0 (nerv-shell strip 11종 + desktop_entry + uninstall-spec §4 atomic).
- **5–10주차**: 1,484 spec 전체 Tier A/B 자동 변환 + 변환률 측정. `nerv doctor` 자동 감지 5종 완성. zsh 플러그인 매니저 패키지 (`nerv-omz`, `nerv-zsh`) e2e. **figterm (`nerv-pty`) opt-in 통합** + 서명/공증 자동화. **rquickjs Tier C opt-in** PoC.
  - **10주차 베타 체크포인트**: 내부 dogfooding 2주.
- **11–14주차**: Homebrew tap 공개, 30초 KPI 자동 측정 CI, 매니저 e2e CI, 문서 6종 v1.4 완비, v1.0 출시.

### v1.0 출시 후 (변경 없음 — North Star 4종)

### v1 이후 (재정렬)

- v1.1: **rquickjs Tier C 일반 공급** (M1 opt-in → 기본). bash spec generator 시나리오 대거 회복
- v1.1: SQLite frecency 학습
- v1.1: `nerv spec list --changes`
- v1.2: **bash 지원** (figterm 경유, M1 인프라 재활용)
- v1.2: fuzzy matching 정식 기능 (M1 opt-in 완료, v1.2 = 기본값 검토 / ranker tuning)
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
| ~~Developer ID 서명/공증 (M0-8)~~ | ~~높음~~ → 해소 | Homebrew-only 배포로 공증 불필요 (2026-07-04). ad-hoc 서명 자동 + brew quarantine 미부착 |
| Tier C 동적 generator 한계 (M0 drop) | 중간 | Tier B 직접 spawn + well-known recognizer 회복 (kubectl/docker/aws/npm); closure-only tail 은 rquickjs Tier C (2026-07-08 출시 동봉, 82% settle) |
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
