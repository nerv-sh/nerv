# Nerv — 셸 자동완성 CLI 기획서 v0.5 (구현 진입 직전)

> **한 줄 요약**: 사라진 Fig의 인라인 자동완성을 **macOS + zsh + 정적 spec 한 점**에서 다시 살린다. 로그인·텔레메트리·AI 없음.

> **상태**: CEO v0.4 리뷰의 GO 조건 3건 + 제거 3건 + 추가 4건 반영 — **v0.5로 확정, 구현 진입 가능**.

---

## 0. v0.4 → v0.5 변경 요약 (CEO v0.4 리뷰 반영)

| 카테고리 | 항목 | 결과 |
|----------|------|------|
| **GO 조건 ①** | `error-states.md` 의 `nerv spec update` 잔존 참조 제거 | **완료** — `brew upgrade` 로 정정. PLAN.md GO 조건 ① 정합 회복 |
| **GO 조건 ②** | `first-5-min.md` 에 0.5단계 (설치 실패 path) 추가 | **완료** — 0.5-A (Xcode CLT 미설치), 0.5-B (oh-my-zsh 충돌), 0.5-C (재설치 멱등) |
| **GO 조건 ③** | withfig/autocomplete fork 트리거 6개월 → 3개월 | **완료** — 모니터링 주기도 30일 → 2주 (`spec-conversion-policy.md` §5.3) |
| 제거 ① | `nerv spec list --changes` (v1.x) | **삭제** — 빌드 파이프라인 단순화. v1.0 은 manifest SHA 기록만 |
| 제거 ② | LaunchAgent (uninstall §2 #7) | **삭제** — v1.0 은 수동 `nerv start/stop` 만. 자동 기동 도입 시 부활 |
| 제거 ③ | VS Code / JetBrains / Hyper / Warp (terminal-compat §1) | **삭제** — 매트릭스 외 *Triage 정책* 으로 대체 |
| 추가 ① | `nerv doctor` 자동 실행 트리거 (§3.6 신설) | **완료** — `nerv start` 직후 / 첫 IPC / `brew upgrade` 후 1회 |
| 추가 ② | spec age soft notice | **완료** — 30일 경과 시 doctor 의 `ℹ` 항목으로 표시 |
| 추가 ③ | Fuzzy matching 비목표 명시 | **완료** — §4 비목표 + §5.1 인용 |
| 추가 ④ | M0-2 의사결정 트리 (Tier C 비율별 spec 50/30) | **완료** — §10 M0 끝 |
| 리스크 | 문서 vs 구현 delta 체크포인트 | **완료** — §10 M0 끝 추가 |
| 리스크 | zsh-autosuggestions 공존 e2e | **완료** — §10 M0-5 에 시각적 충돌 검증 1건 추가 |
| 정합성 | terminal-compat §5.1 오타 (iTcerm2) | **완료** |
| 정합성 | PLAN §15 cargo new 중복 | **완료** |

---

## 1. 이름 — `nerv`

**의미**: 키 입력이 신경(nerve)을 따라 흐르고 다음 명령이 반사적으로 떠오른다는 비유. 1음절, 짧고 강함.

**충돌 점검 (실측)**

- **crates.io** `nerv`: 미관리 클러스터 워크로드 매니저(2021, 1버전, ~1.2k DL). 우회: 패키지명 `nerv-cli` 게시, 바이너리는 `nerv` 유지.
- **Nerves (Elixir/IoT)**: 카테고리 다름. 도큐먼트에 *"Nerv (no s) — shell autocomplete for macOS zsh"* 일관 표기.
- **Homebrew formula 이름**: M0 첫째 날 확인.
- **GitHub org**: ✅ **`nerv-sh` 확보 완료**. 모든 레포는 `nerv-sh/*` 네임스페이스 사용.
- **상표 (NERV / Evangelion)**: 픽션 IP라 직접 충돌 낮음. 시각 모티프 차용 금지.

---

## 2. 배경

Fig는 2023년 Amazon에 인수되어 Amazon Q Developer CLI(`q`)로 흡수됐다. (a) Builder ID 로그인 강제, (b) AI·MCP·에이전트의 동거, (c) 반응성 저하. 사용자가 처음부터 원했던 것은 **다음 토큰의 즉시 제안** 한 가지뿐이다. Nerv는 그것만 한다.

---

## 3. 포지셔닝

| 구분 | Original Fig | Amazon Q (`q`) | Inshellisense (MS) | **Nerv** |
|------|--------------|----------------|--------------------|----------|
| 핵심 기능 | 인라인 자동완성 | AI + 자동완성 + 에이전트 | 인라인 자동완성 | **인라인 자동완성** |
| 인증 | 선택 | **Builder ID 필수** | 없음 | 없음 |
| 설치 | macOS 앱 | 큰 번들 + 권한 | `npm i -g` (Node) | **`brew install`** |
| 런타임 의존 | macOS 앱 | 다수 | **Node.js** | **단일 정적 바이너리** |
| 라이선스 | 일부 OSS | 상용 | MIT | **Apache-2.0** |
| spec 호환 | 자체 | 일부 | Fig spec | **Fig spec 정적 부분** |
| 텔레메트리 | 옵트아웃 | 기본 수집 | 없음 | **코드에 없음** |
| 인라인 `?` | 있음 | 부분 | 부분 | **핵심 기능** |

**메시지**: *"Fig는 사라졌다. Nerv가 그 자리를 메운다 — 로그인 없이, Node 없이, 한 줄 설치로."*

---

## 4. v1.0 범위 — 단일 조합

> **원칙**: "하나를 완벽히, 나머지는 v1 이후."

### 포함

- **OS**: macOS (Apple Silicon + Intel)
- **셸**: zsh 5.8+
- **터미널 보장**: iTerm2, Apple Terminal.app — *베스트에포트*: WezTerm, Alacritty, Kitty (§11)
- **tmux**: 기본 환경 동작 보장
- **zsh 플러그인 매니저**: oh-my-zsh, zinit, antigen 설치 경로 공식 지원
- **spec 소스**: `withfig/autocomplete` (MIT)에서 **상위 50개** 정적 JSON 변환. 런타임 JS 엔진 없음
- **UI**: 인라인 ANSI 팝업
- **인터랙션**: ↑↓ 탐색 / `Tab` 또는 `→` 채택 / `Esc` 닫기 / **`?` 인라인 도움말**
- **온보딩**: `brew install` → `nerv init zsh` → 30초 안에 첫 자동완성
- **종료**: `nerv uninstall` 흔적 0

### 비목표 (v1)

AI / 자연어, bash·fish·PowerShell·nushell, Linux·Windows, 동적 generator, 클라우드/팀 동기화, 인증, 텔레메트리, 자체 업데이트(brew에 위임), GUI 설정 앱, frecency, `nerv config` 명령, `nerv spec update` / `nerv feedback` 명령(GO 조건 ①), **fuzzy matching** (v1.x 검토 — §5.1 prefix-only 정책), **`nerv spec list --changes`** (v1.0 빌드 파이프라인 단순화 위해 v1.1+ 로 이연).

### 상위 50 spec 후보 풀

`git, gh, docker, kubectl, npm, pnpm, yarn, brew, cargo, rustup, go, python, pip, uv, poetry, node, deno, bun, make, cmake, ninja, ssh, scp, rsync, curl, wget, jq, yq, fd, rg, fzf, bat, eza, ls, find, grep, sed, awk, tar, zip, ssh-keygen, git-lfs, terraform, ansible, helm, aws, gcloud, az, vercel, netlify` — M0-2의 외삽 결과로 v1.0 확정 목록 결정.

---

## 5. 핵심 기능 상세

### 5.1 인라인 자동완성

- 매 키 입력 후 디바운스 5–10 ms로 토큰화 → 위치 추론.
- 50개 CLI 정적 spec 즉시 사용. 정상 위치는 추천, 동적 위치는 §5.1 힌트.
- **매칭 알고리즘 — v1.0 은 prefix-only**: `git co` → `commit` / `config` (prefix), 단 `checkout` 은 `git ch` 에서만 매칭 (`checkout` 은 `c-h-...` 로 시작). `git chk` → 결과 0 (fuzzy 미지원). Fuzzy 는 §4 비목표 → v1.x 검토.
- **동적 generator 인자 힌트 UX** (URL 직접 임베드, GO 조건 ①):
  ```
  ⤷ 동적 완성은 v1.1에서 지원 예정 — 직접 입력하세요
     ▸ git branch --list 로 후보 확인
     ▸ 요청: https://github.com/nerv-sh/nerv/issues/new?template=dynamic.yml&cmd=git+checkout
  ```
- 같은 라인에서 한 번 표시 후 5초 디바운스.

### 5.2 `?` 인라인 도움말

- 추천 위에서 `?` → spec의 `description` 펼침. man page 대체 킬러 차별점.
- 인수 기준: 50개 spec의 1단계 플래그 100% 1줄 설명.

### 5.3 30초 온보딩 + 5분 리텐션

```bash
$ brew install nerv-sh/tap/nerv
$ eval "$(nerv init zsh)"
$ git c⎵                       # 즉시 추천 표시
```

- KPI: `brew install` → 첫 팝업 ≤ 30초.
- `nerv init zsh` 멱등(마커 주석 기반 갱신).
- 첫 실행 1회 한정 안내 메시지에 "**git, docker, kubectl 명령으로 5분만 사용해 보세요**" 1줄 가이드(§10 M0-7 시나리오 링크).

### 5.4 깔끔한 uninstall (출시 차단 요건)

- `~/.zshrc` 의 `# >>> nerv >>>` ~ `# <<< nerv <<<` 마커 라인 제거.
- 데몬 종료, 캐시·설정 삭제 (`--keep-config` 옵션).
- `brew uninstall nerv` 도 동일 결과 (formula `caveats` + `post_uninstall`).
- e2e: *설치 → 사용 → 제거 → 흔적 0*.

### 5.5 에러 상태 UX (5종, 자동 감지)

| 상황 | 감지 시점 | 화면 |
|------|----------|------|
| `nervd` 미기동 | UDS 연결 실패 | 회색 1줄: `[nerv] daemon not running — run: nerv start` (세션 1회) |
| spec 손상 | 데몬 시작 / lazy load | 해당 spec만 비활성, stderr 1줄, `doctor` 가 상세 |
| zsh < 5.8 | `nerv init zsh` | stderr 경고 + `exit 0` (자동완성 비활성) |
| ZLE 위젯 충돌 (zsh-autocomplete 등) | `nerv init zsh` | stderr 경고 + 충돌 가이드 URL |
| spec 버전 불일치 | 데몬 시작 | stderr 1줄 + `doctor` |

원칙: **고장은 조용히 알리고, 고치는 한 줄을 함께.**

### 5.6 zsh 플러그인 매니저 호환 ★ 신설

대다수 zsh 사용자가 oh-my-zsh / zinit / antigen 위에서 동작한다. `init zsh >> ~/.zshrc` 만 안내하면 어색하므로 매니저별 공식 경로를 v1.0에 포함.

| 매니저 | 설치 경로 | 검증 방식 |
|--------|----------|----------|
| **plain zsh** | `eval "$(nerv init zsh)"` 추가 | M0 e2e |
| **oh-my-zsh** | `nerv-sh/nerv-omz` 별도 레포 — `plugins=(... nerv)` | M1 e2e |
| **zinit** | `zinit load nerv-sh/nerv-zsh` | M1 e2e |
| **antigen** | `antigen bundle nerv-sh/nerv-zsh` | M1 e2e |
| **prezto** | 베스트에포트 (M1 1회 수동) | — |

핵심 hook 스크립트는 동일, 패키징만 다르다. `docs/install-zsh.md` 에 매니저별 1페이지 가이드.

### 5.7 spec 변환 실패 정책 ★ 신설

`withfig/autocomplete` TS spec 중 일부는 정적 변환이 불가능하거나 부분만 가능하다. 정책:

1. **변환 0% (구조 자체 추출 실패)**: 빌드에서 제외. CI 알림. *50개 풀에서 다른 후보로 교체*.
2. **변환 부분 성공 (서브커맨드만, 인자 generator는 동적)**: 포함하되 `specs-prebuilt/<name>.json` 의 메타에 `"limited": true` 마킹.
3. **`nerv spec list` 표시**: 부분 변환 spec은 `git (limited — 동적 인자 v1.1)` 처럼 *(limited)* 라벨.
4. **사용자 시점**: 정상 위치는 평소처럼 동작, 동적 위치에서 §5.1 힌트.
5. **회귀**: 다음 릴리즈에서 변환 성공률이 떨어진 spec은 CI에서 차단.

---

## 6. 아키텍처

```
┌─ Terminal (iTerm2 / Terminal.app / + tmux / + 베스트에포트 3종) ─┐
│   사용자 키 입력                                                  │
│        ▼                                                          │
│   ┌─────────────────────────────────────────┐                     │
│   │  zsh ZLE widget (zle-line-pre-redraw)    │ ── LBUFFER ──▶     │
│   │  (oh-my-zsh / zinit / antigen 호환 패키징)  │ ◀── JSON 추천 ──   │
│   └────────────┬────────────────────────────┘                     │
│                │ Unix domain socket                               │
│                ▼                                                  │
│   ┌─────────────────────────────────────────┐                     │
│   │  nervd  (Rust 데몬, 정적 spec only)        │                    │
│   │  Parser / Engine / Specs(50, 일부 limited)│                    │
│   │  Counter (in-memory) / IPC (JSON-RPC)     │                    │
│   └─────────────────────────────────────────┘                     │
└───────────────────────────────────────────────────────────────────┘
```

**런타임 JS 엔진 없음** — TS spec은 빌드타임에 `swc` 로 트랜스파일 + 정적 추출. 동적 generator는 v1.1에서 `deno_core` 검토.

**tmux 고려** — alternate screen 미사용, raw cursor save/restore + line clearing만. M0 검증.

---

## 7. 기술 스택

- **언어**: Rust 1.85+ (edition2024 안정화 버전 — clap 4.6+ 등 요구)
- **CLI**: `clap` v4
- **IPC**: `tokio` + `interprocess` (UDS)
- **TUI/ANSI**: `crossterm` (직접 ANSI; alternate screen 회피)
- **빌드타임 트랜스파일**: `swc_core`
- **저장소**: in-memory `HashMap<prefix, count>` (v1.0). SQLite는 v1.1
- **설정**: TOML 직접 편집 (`nerv config` 명령 v1.0 없음)
- **테스트**: `cargo nextest`, `expectrl` (zsh + tmux + 매니저별 시나리오)
- **배포**: Homebrew tap (`nerv-sh/homebrew-tap`) 단일 채널
- **서명/공증**: macOS Developer ID + notarization

---

## 8. 저장소 구조

```
nerv/
├─ Cargo.toml
├─ crates/
│  ├─ nerv-cli/         nerv-daemon/      nerv-engine/      nerv-shell/
├─ shell-integrations/zsh/_nerv.zsh
├─ build/spec-transpile/
├─ specs-prebuilt/                        # 빌드 산출 (릴리즈에 포함)
├─ vendor/withfig-autocomplete/           # subtree (MIT, 버전 핀)
├─ docs/
│  ├─ architecture.md   uninstall-spec.md   error-states.md
│  ├─ terminal-compat.md   install-zsh.md   benchmarks.md
│  └─ spec-conversion-policy.md
└─ .github/workflows/
```

별도 레포 (M1):

- `nerv-sh/homebrew-tap`
- `nerv-sh/nerv-omz` (oh-my-zsh 패키지)
- `nerv-sh/nerv-zsh` (zinit/antigen 공용)

---

## 9. CLI 명령 (최종 — GO 조건 ① 반영)

```bash
nerv init zsh           # 셸 통합 스크립트 출력
nerv doctor             # 환경 진단 (zsh, hook, 데몬, .zshrc, 충돌, spec 버전)
nerv start | stop       # nervd 라이프사이클
nerv spec list          # 내장 spec 목록 ((limited) 라벨 포함)
nerv uninstall          # 깔끔한 제거
```

**v1.0에서 의도적으로 제외**: `telemetry`, `update`, `spec install`, `spec update`, `spec dev`, `feedback`, `config`. 5개 명령으로 끝낸다 — 단순함이 신뢰의 일부.

---

## 10. 로드맵

### M0 — 스파이크 (4주, +1주는 Developer ID 선행 검증분)

산출물 10개. M0 끝에서 GO/No-Go + 의사결정 트리 + 문서-구현 delta 점검.

1. **ZLE → UDS → Rust → 인라인 ANSI** PoC. p95 < 25 ms (Apple Silicon, iTerm2).
2. **3개 spec 변환** (`git`, `docker`, `kubectl`). 정적 추출 비율 측정 → v1.0 50개 확정 외삽.
3. **`?` 인라인 도움말 PoC** (위 3개 spec).
4. **30초 온보딩 시뮬레이션** — `brew tap` 흉내 + 녹화 영상.
5. **터미널 호환성 + tmux 검증 + zsh-autosuggestions 공존 e2e** — iTerm2 + Terminal.app e2e + tmux 안 깨짐. **zsh-autosuggestions 활성 상태에서 Nerv 팝업 + ghost text 시각적 충돌 0** 검증 (실패 path 0.5-B 의 합격 기준과 정합). WezTerm/Alacritty/Kitty 1회 수동.
6. **Inshellisense 1대1 정량 벤치** — `git c` 5타 latency, RSS. carapace-bin / zsh-autocomplete 정성 1단락.
7. **★ 첫 5분 사용 시나리오 스크립트** (GO 조건 ②) — `git status / log / checkout` → `docker ps / build / run` → `kubectl get / describe / logs` 12단계 + **0.5단계 (설치 실패 path 3건)**. 각 단계에서 *어디서 추천이 뜨고 어디서 §5.1 힌트가 뜨는지* 명시. 스크립트 + 녹화 영상을 `docs/first-5-min.md` 에 커밋.
8. **★ Developer ID 서명/공증 선행 검증** (GO 조건 ③) — Apple 개발자 프로그램 가입 + 인증서 발급 + 빈 바이너리(`fn main(){}`)로 sign + notarize + Homebrew tap 설치 → Gatekeeper 통과 e2e. **이 항목 미통과 시 M0 = No-Go**.
9. **★ `withfig/autocomplete` 포크 전략 확정** — vendor subtree 기준선 commit hash 핀, 자체 PR 수용 정책 초안, **3개월 commit 부재 또는 archived 시 forward-only fork** 결정 (v0.5 GO 조건 ③). `docs/spec-conversion-policy.md` 의 §5.3.
10. **★ 문서 vs 구현 delta 점검** (CEO 리스크 #1 대응) — M0 끝에 5종 docs (`uninstall-spec`, `error-states`, `terminal-compat`, `first-5-min`, `spec-conversion-policy`) 의 인수 기준 vs M0 PoC 구현 사이의 갭을 1쪽 표로 정리. 갭이 큰 항목은 M1 0–6주차 우선순위로 끌어올리거나, 문서를 현실에 맞춰 v1.2 로 개정.

**M0 Go/No-Go**: 1+2+3+5+8 동시 충족, 6에서 latency 동급 이상, 7 시나리오에서 12단계 중 10단계 + 0.5단계 3건 중 2건 만족 시 GO. 미달 시 ZLE 통합 / spec 매칭 / 사용 시나리오 재설계.

**M0-2 결과 의사결정 트리** (CEO 리스크 #4 대응 — 사전 확정으로 6주차 심리적 저항 최소화):

| Tier C 비율 (3종 외삽) | 결정 |
|------------------------|------|
| 0% (모두 A 또는 B) | **그대로 50개** + M1 16주 |
| 1–10% (1–5개 교체 필요) | **그대로 50개** + 대체 큐 5개로 보강 + M1 16주 |
| 11–20% (6–10개 교체 필요) | **그대로 50개** 시도하되 M1 6주차 체크포인트에서 80% 변환률 미달 시 즉시 30개로 축소 |
| 21–40% (11–20개 교체 필요) | **사전 30개로 축소** 시작 — 메시지: *"상위 30개 지원, 이후 50→100 점진 확장"* |
| > 40% | **No-Go**. 정적 변환 전략 자체 재검토 (deno_core 도입 v1.0 재검토) |

이 트리를 M0-2 직전에 *글로 박아둠* — Tier 분포 결과를 본 표에 매칭만 하면 결정 자동화.

### M1 — v1.0 (16주 기본, 6주차 + 12주차 체크포인트)

> **기본 16주 채택** — CEO 권고대로 spec 30 축소가 아니라 일정 연장. *"상위 50개 지원"* 메시지를 보존해 마케팅 가치 유지.

- **0–6주차**: 50개 spec 변환 파이프라인, ZLE 안정화, 인라인 `?`, 에러 상태 UX 5종.
  - **6주차 중간 체크포인트** — 50개 중 80%+ 변환 / latency p95 < 25 ms / tmux+2터미널 회귀 / uninstall 흔적 0. **모두 통과 시만 진행**. 미달 시 spec 50→30 (최후 수단) 또는 추가 4주 연장.
- **7–12주차**: 50개 spec의 1단계 플래그 `?` 도움말 100%, in-memory 사용 카운터로 prefix-match 가중, `nerv doctor` 자동 감지 5종, **zsh 플러그인 매니저 패키지** (`nerv-omz`, `nerv-zsh`) e2e.
  - **12주차 베타 체크포인트** — 내부 dogfooding 2주.
- **13–16주차**: Homebrew tap 공개, 30초 KPI 자동 측정 CI, 모든 매니저 e2e CI, 문서 6종 완비, v1.0 출시.

### v1.0 출시 후 — North Star Metric ★ 신설

텔레메트리 없이 추적 가능한 공개 신호 4종:

1. **Homebrew formula install count** — `brew analytics` 공개 데이터에서 30일 install 수.
2. **GitHub stars** — 7일/30일 변화율.
3. **GitHub Issues 활성도** — 주간 신규 이슈/PR 수, 평균 응답 시간.
4. **Discord/Matrix 채널 멤버 수** — 가입 수 + 주간 활성 발화자.

**리텐션 추정**: v1.x에서 *옵트인* 단발 설문(e.g., `nerv survey` 명령으로 Google Form 링크 1회 표시) 1회 진행. 그 외 자동 추적 일체 없음.

**v1.0 출시 6개월 시점의 임의 성공 기준**:

- 30일 brew installs ≥ 5,000
- GitHub stars ≥ 2,000
- 7일 후에도 사용 중인 사용자 ≥ 50% (옵트인 설문 응답 기준)
- 외부 PR ≥ 20건

미달 시 시장 신호로 받아들이고 v2 방향(bash → Linux 등) 재검토.

### v1 이후 (커뮤니티 시그널 기반, AI 항목 제거 — GO 조건 ④)

- v1.1: 동적 generator (`deno_core`).
- v1.1: SQLite frecency 학습.
- v1.1: `nerv spec list --changes` (manifest diff 명령).
- v1.2: spec 50 → 200+ 점진 확장.
- v1.x: **fuzzy matching** (prefix-only → fuzzy 옵션 도입).
- v1.3: bash 지원.
- v1.4: Linux.
- v2.0: fish.
- v2.x: spec 레지스트리 (`nerv spec install <pkg>`), `nerv config`.
- v?.x: LaunchAgent 자동 기동 (도입 시 `uninstall-spec.md` §2 인벤토리에 항목 부활).
- 일정 미정: PowerShell, nushell, Windows.
- **AI / 자연어 모드는 본 로드맵에서 제거**. 필요 시 외부 플러그인 / 별도 프로젝트로 분리. *"AI 없음"* 포지셔닝과 정합.

---

## 11. 터미널 호환성 매트릭스

| 터미널 | v1.0 보장 | 검증 |
|--------|----------|------|
| iTerm2 (latest) | **보장** | M0 e2e + M1 CI |
| Apple Terminal.app | **보장** | M0 e2e + M1 CI |
| WezTerm | 베스트에포트 | M0 1회 수동 |
| Alacritty | 베스트에포트 | M0 1회 수동 |
| Kitty | 베스트에포트 | M0 1회 수동 |
| tmux (위 안에서) | **보장** | M0 e2e + M1 CI |

매트릭스 외 (Warp / VS Code 통합 / JetBrains / Hyper / Tabby 등) 는 **공식 지원 외** — `terminal-compat.md` §1 의 *Triage 정책* 따름.

ANSI는 raw cursor save/restore + line clearing만, alternate screen 진입 안 함.

---

## 12. 경쟁자 벤치마크 (M0 결과로 채울 표 — Inshellisense 정량 only)

| 항목 | Inshellisense | **Nerv (목표)** | 메모 |
|------|---------------|----------------|------|
| 런타임 의존 | Node.js | 없음 | 정성 |
| 키 응답 p95 | TBD | **< 25 ms** | M0 정량 |
| RSS 메모리 (idle) | TBD | TBD | M0 정량 |
| spec 호환 | Fig spec 다수 | Fig spec 정적 부분 | 정성 |
| 인라인 `?` | 부분 | **전체** | 정성 |
| 설치 한 줄 | npm | brew | 정성 |

carapace-bin / zsh-autocomplete: 아키텍처 상이로 정량 비교 의미 낮음 → `docs/benchmarks.md` 정성 1단락.

---

## 13. 리스크 (재정리)

| 리스크 | 심각도 | 대응 |
|-------|-------|------|
| 1인 개발 + 16주 M1 → 번아웃 | **높음** | 6/12주차 체크포인트, 일일 작업 상한 자율 설정, 외부 기여 적극 수용 |
| Developer ID 서명/공증 트러블 (경험 없을 시) | **높음 ↑** (v0.3 중간에서 상향) | **M0-8로 선행** — Apple 가입 + 빈 바이너리 e2e 가 M0 GO 조건 |
| 정적 spec 한계 (동적 generator 인자) | 중간 | §5.1 힌트 UX (URL 임베드), v1.1 우선 |
| TS → JSON 변환의 spec별 난이도 편차 | 중간 | M0-2 3종 검증 + §5.7 변환 실패 정책 |
| tmux/터미널 ANSI 차이 | 중간 | M0-5 Go/No-Go 직결, 6주차 회귀 |
| `withfig/autocomplete` archived 또는 포맷 변경 | 중간 | **M0-9에서 포크 전략 확정** (v0.3 11주차에서 앞당김) |
| zsh 플러그인 매니저별 충돌 | 중간 | M1 7–12주차 매니저별 e2e + `doctor` 충돌 감지 |
| `nerv` 명명 검색 혼선 (Nerves) | 낮음 | *"Nerv (no s)"* 일관 표기 |
| crates.io `nerv` 선점 | 낮음 | 패키지명 `nerv-cli` |
| Homebrew 단일 채널 채택 한계 | 낮음 | v1.4 Linux 시점에 추가 |

---

## 14. 라이선스 & 거버넌스

- 본 프로젝트: **Apache-2.0**.
- `vendor/withfig-autocomplete/`: MIT 보존, NOTICE 명시, 버전 핀 (M0-9).
- DCO(`Signed-off-by`) 시작.
- 거버넌스: 초기 BDFL → 기여자 누적 시 재검토.

---

## 15. 다음 액션 (착수 직후 1–2주)

1. ✅ GitHub org `nerv-sh` 확보.
2. ✅ Apple 개발자 프로그램 가입.
3. ✅ **5종 인수 기준 문서 완료 (v1.1)** — `docs/uninstall-spec.md` (§5.4) + `docs/error-states.md` (§5.5) + `docs/terminal-compat.md` (§11) + `docs/first-5-min.md` (§5.3 / M0-7) + `docs/spec-conversion-policy.md` (§5.7 / M0-9).
4. ✅ **cargo workspace + 5 crates 스캐폴딩** — `cargo check`/`clippy -D warnings`/`fmt --check`/`test` 모두 통과 (8 단위 테스트).
5. ✅ **M0-9 — `withfig/autocomplete` subtree pin** = `aef52acff84c45edde61ae610cc2c964802b9a38` (1,484 TS spec, ~102 MB, MIT). NOTICE 갱신 + `.github/workflows/upstream-monitor.yml` (cron 매 2주, 90일 부재/archived 시 `fork:trigger` 이슈 자동 생성).
6. **다음 — Homebrew tap 레포** `nerv-sh/homebrew-tap` 생성 (formula 는 v1.0 출시 임박 시 작성).
7. M0-1 (zsh ZLE → UDS → ANSI) 30줄 PoC + latency 측정.
8. M0-2 트랜스파일러 진입점 + git/docker/kubectl 3종 스파이크.

---

*문서 v0.5 — CEO v0.4 리뷰의 GO 조건 3건 + 제거 3건 + 추가 4건 + 정합성 3건 모두 반영. docs 5종도 v1.1 로 갱신 (error-states / first-5-min / spec-conversion-policy / terminal-compat / uninstall-spec). 본 문서로 구현 진입. 다음 갱신 트리거: M0 종료 (산출물 10번 — 문서 vs 구현 delta 점검), 6주차 / 12주차 체크포인트, 또는 핵심 비목표 변경.*
