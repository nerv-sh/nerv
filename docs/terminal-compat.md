# Terminal Compatibility — 명세서

> **Status**: 인수 기준 (글이 코드보다 먼저). PLAN.md v0.6 §11 정합.
> **원칙**: ANSI 시퀀스는 *최소한*만. alternate screen 진입 안 함. tmux 안에서도 안 깨진다.
> **v0.6 boundary**: 본 문서는 **M0 ZLE path 기준**. M1 의 figterm (`nerv-pty`) opt-in path 는 §6.4 신설 참조.

---

## 1. 보장 매트릭스 (v1.0 기준)

| 터미널 | 등급 | 검증 | M0 | M1 |
|--------|------|------|----|----|
| iTerm2 (latest) | **보장** | 자동 e2e | ✓ | CI |
| Apple Terminal.app | **보장** | 자동 e2e | ✓ | CI |
| WezTerm | 베스트에포트 | 수동 1회 | ✓ | — |
| Alacritty | 베스트에포트 | 수동 1회 | ✓ | — |
| Kitty | 베스트에포트 | 수동 1회 | ✓ | — |
| **tmux (보장 터미널 안)** | **보장** | 자동 e2e | ✓ | CI |
| **tmux (베스트에포트 안)** | 베스트에포트 | — | — | — |

### 등급 정의

- **보장**: v1.0 출시 차단 요건. 회귀 발생 시 즉시 hotfix. CI 가 매 PR 검증.
- **베스트에포트**: 동작하지만 보증 X. 회귀 보고 시 best-effort 수정. CI 미포함.

### 매트릭스 외 터미널 — Triage 정책

위 매트릭스에 없는 모든 터미널 (Warp, VS Code/JetBrains 통합 터미널, Hyper, Tabby 등) 은 **공식 지원 외**. 이슈 트래커 정책:

- **재현 가능한 버그 보고**: GitHub Issues 의 `triage:unsupported` 라벨로 분류, 응답 시한 없음.
- **격상 트리거**: (a) 30일 내 동일 보고 ≥ 5건 + (b) 재현 절차 명확 → 베스트에포트 등급 격상 검토.
- **PR 환영**: 외부 기여로 베스트에포트 검증이 추가되면 매트릭스 편입 가능 (수동 1회 검증 + 알려진 이슈 기록).

본 매트릭스를 좁게 유지하는 이유는 *지원 약속의 진실성*. 매트릭스에 올라온 항목은 책임지고 관리한다.

---

## 2. 허용 ANSI 시퀀스 (whitelist)

인라인 팝업 렌더에 사용할 수 있는 시퀀스 *전체*:

| 목적 | 시퀀스 | 비고 |
|------|--------|------|
| 커서 위치 저장 | `ESC 7` (DECSC) | `ESC [s` (SCO) 보다 호환성 ↑ |
| 커서 위치 복원 | `ESC 8` (DECRC) | |
| 커서 N행 아래 | `ESC [ <n> B` | |
| 커서 N행 위 | `ESC [ <n> A` | |
| 커서를 컬럼 1 로 | `ESC [ G` | |
| 라인 끝까지 지움 | `ESC [ K` (EL 0) | |
| 화면 끝까지 지움 | `ESC [ J` (ED 0) | 팝업 영역 정리에만 |
| 색상 (전경) | `ESC [ 38 ; 5 ; <n> m` | 256색까지만, true color 미사용 |
| 색상 (배경) | `ESC [ 48 ; 5 ; <n> m` | |
| Bold | `ESC [ 1 m` | |
| Dim (회색) | `ESC [ 2 m` | E1 hint 등에 사용 |
| 리셋 | `ESC [ 0 m` | |

---

## 3. 금지 시퀀스 (blacklist)

`crossterm` 의 다음 기능을 *사용하지 않는다*:

| 시퀀스 | 이유 |
|--------|------|
| `ESC [ ? 1049 h/l` (alternate screen) | tmux scrollback 깨짐, Terminal.app 깜빡임 |
| `ESC [ ? 25 h/l` (커서 표시/숨김) | 일부 터미널에서 사용자 설정 덮어씀 |
| `ESC [ ? 1004 h/l` (focus events) | 미지원 터미널 다수 |
| `ESC [ <n> ; <n> H` (절대 좌표 이동) | 입력 라인이 어디 있는지 모름. 상대 이동만 |
| Bracketed paste mode 변경 | 사용자 설정 보호 |
| OSC 52 (clipboard) | tmux passthrough 복잡, v1 비목표 |
| 24-bit true color | tmux 패스스루 + Terminal.app 미지원 |
| 이미지 프로토콜 (Sixel, Kitty graphics, iTerm2 imgcat) | v1 비목표 |
| Hyperlink (OSC 8) | tmux 패스스루 미흡, 대신 텍스트 URL |

### 3.1 글리프 폭 (icon)

추천 popup 의 각 행은 `" <icon> <display> <pad> "` 로 leading slot
하나를 icon 에 할당. 위젯은 **모든 non-ASCII glyph 를 2 cells 폭** 으로
가정하고 ASCII row 에는 trailing space 1칸을 padding 으로 더한다 (행
정렬 보존). 따라서 엔진의 `sanitize_icon` 이 다음을 *반드시* 보장한다:

- ASCII 1글자 만 통과 (`$`, `>` 등) — width 1, 슬롯 1 cell + pad
- non-ASCII 는 `unicode-width` width == 2 만 통과 (📦 📝 中 등 4-byte
  supplementary emoji + CJK ideograph). Ambiguous-width (UAX #11 의 A)
  나 Latin-extended (`à`, `é`) 는 width 1 로 렌더돼 행이 1 cell 밀리므로
  거부. zero-width combiner / VS-16 같은 다중 codepoint sequence 도 거부
  (≤4 byte 게이트가 차단).

위젯 정렬 contract 가 깨지면 popup 우측 border (`│`) 가 한 cell 클리핑돼
즉시 시각적 회귀로 잡힌다.

---

## 4. 렌더 전략 (요약)

### 입력 라인을 가리지 않는다

```
                         ┌─ 사용자가 보는 화면 ─┐
                         │                      │
prompt> git c█           │ ← 사용자 입력 라인     │
                         │                      │
  checkout  Switch...    │ ← 추천 (회색)         │
  clone     Clone...     │                      │
  commit    Record...    │                      │
                         └──────────────────────┘
```

- 사용자 입력 라인은 그대로. 팝업은 *아래* 에 N행 추가.
- 사용자가 키 입력하면: 저장된 cursor 로 복귀 → 입력 라인 갱신 → cursor 다시 저장 → 팝업 재렌더.

### 팝업 갱신 알고리즘

```
1. ESC 7                          # cursor 저장
2. ESC [ B                         # 한 줄 아래로
3. ESC [ G                         # 컬럼 1로
4. for each row in popup:
     ESC [ K                       # 행 지움
     <row content>
     \r\n
5. ESC [ J                         # 화면 끝까지 지움 (이전 팝업이 더 길었다면)
6. ESC 8                          # cursor 복귀
```

### tmux 고려

- tmux 는 `DECSC/DECRC` 를 자기 윈도우 단위로 처리 — pane 분할에서도 안전.
- `ESC [ J` (화면 끝까지 지움) 가 다른 pane 영향 X (tmux 가 격리).
- 단, **alternate screen 진입은 tmux 의 scrollback 을 깨뜨림** → 절대 사용 안 함.
- `set -g default-terminal "xterm-256color"` 또는 `tmux-256color` 둘 다 지원.

### 라인 수가 부족할 때

터미널 마지막 줄에 입력 라인이 있으면 팝업 N행을 그릴 공간이 없음.

**처리**:

- 사용자 입력 라인 위치 추적 (zsh `${PROMPT_EOL_MARK}` + `LBUFFER` 길이로 추정).
- 화면 하단에서 N행 부족하면 자동 스크롤 (터미널이 처리) — 입력 라인이 위로 올라가고 팝업이 그 아래.
- 단, 스크롤 발생은 사용자가 즉시 인지 가능 (시각적 점프) — *마지막 수단*.
- 1차: 팝업 행수를 동적 축소 (요청 5행 → 가용 2행 표시).

---

## 5. 터미널별 알려진 이슈 + 회피

### 5.1 iTerm2

- *알려진 이슈*: 글자 폭 wide character (한글/이모지) 와 narrow character 혼재 시 cursor 이동 1픽셀 어긋남.
- 회피: 팝업에 wide character 사용 금지 (영문 + ASCII 박스만).
- *알려진 이슈*: `iTerm2 Status Bar` 활성 시 화면 하단 1행을 가져감.
- 회피: 자동 감지 어려움 — 가용 행수 계산 시 -1 conservative.

### 5.2 Apple Terminal.app

- *알려진 이슈*: 256색 인덱스 일부가 다른 터미널과 RGB 매핑 다름.
- 회피: 회색 (8 / 240 / 244) 만 사용. 색상 의존 정보 표시 X.
- *알려진 이슈*: `ESC [ K` 가 lazy 하게 적용되는 경우 있음.
- 회피: 팝업 갱신 후 `flush()` 명시적 호출.

### 5.3 WezTerm / Alacritty / Kitty (베스트에포트)

- 일반적으로 ANSI 표준 준수도가 더 높음 → 큰 이슈 없음.
- *공통*: 사용자가 사용자 정의 keybinding 으로 Tab/Esc 를 가로챘다면 Nerv 가 받지 못함. 회피 가이드 문서.
- *Kitty*: 사용자 정의 graphics protocol 활성 시에도 영향 없도록 OSC 미사용 원칙 유지.

### 5.4 위치 계산 일반 원칙

일부 터미널/IDE 통합은 자체 OSC 시퀀스 (예: `133`, `633`) 를 입력 라인 주변에 삽입하여 cursor 위치 계산을 어긋나게 만들 수 있다. 회피:

- ZLE widget 이 `LBUFFER` 길이를 *zsh 에 다시 물어* 권위 있는 위치 확보. 터미널 보고는 신뢰하지 않는다.
- 이 원칙은 매트릭스 외 터미널(§1 Triage 정책 참조)에서 발생할 이슈에도 자연스럽게 대응한다.

---

## 6. tmux 명세

### 6.1 보장 조건

- tmux 3.2 이상.
- 보장 터미널 (iTerm2 / Terminal.app) *안에서* 실행.
- `set -g default-terminal "tmux-256color"` 또는 `xterm-256color`.
- copy-mode 가 아닌 일반 입력 모드.

### 6.2 보장 동작

- 인라인 팝업 정상 렌더.
- pane 분할 (가로/세로) 환경에서 *현재 pane* 안에만 팝업.
- 다른 pane / window 영향 없음.
- session detach/attach 후에도 정상 동작.
- nested tmux (`tmux in tmux`) 는 *베스트에포트*.

### 6.3 tmux 검증 e2e (M0-7)

> v0.6 정합: 본 e2e 는 ZLE path (M0). M1 figterm opt-in 의 tmux 검증은 §6.4 별도.

```
GIVEN: tmux 3.2+ 안 iTerm2
WHEN:
  tmux new-session
  zsh -i
  eval "$(nerv init zsh)"
  git c<Tab>
THEN:
  - 추천 팝업 표시
  - 입력 라인 텍스트 손상 없음
  - tmux 상태바 / pane border 손상 없음
  - Ctrl-B + arrow 로 pane 전환 시 이전 pane 잔재 없음
  - tmux detach + attach 후 다시 git c<Tab> 정상
```

---

### 6.4 figterm opt-in path (M1) ★ 신설 (v1.2 / PRD §5.8)

M1 에서 `nerv-pty` (← upstream figterm) 가 PTY shim 으로 도입되면
edit-buffer 인터셉트 방식이 ZLE widget → PTY 가로채기로 바뀐다.
이때 본 문서 §2–§5 의 ANSI 시퀀스 정책은 *그대로 유지* 되지만,
다음이 추가된다:

| 항목 | M0 ZLE | M1 figterm |
|------|--------|-----------|
| 입력 라인 위치 추론 | zsh `LBUFFER` | `nerv-term` (← alacritty_terminal) screen state — 정확 |
| prompt 경계 감지 | 없음 | precmd/preexec OSC 697 markers (`_nerv-pty.{zsh,bash,fish}` 부트스트랩이 emit — `Shell=` 마커가 edit-buffer 게이트) |
| 셸 지원 | zsh 만 | zsh + bash + fish (셋 다 출하됨 — bash/fish 는 ZLE 부재로 PTY 가 *유일* 경로) |
| 바이너리 배치 | `~/.zshrc` source | `nerv-pty` 가 release tarball/Formula 동봉 — `nerv init` 이 sibling lookup 으로 `NERV_PTY_BIN` 자동 export, 부트스트랩이 `exec nerv-pty -- "$SHELL"` |
| 활성화 분기 | 기본 | `NERV_PTY=1` 환경변수 |
| 코드서명 | 불필요 | **필수** (Apple Developer ID + notarization, M0-8 인프라 재활용) |

PTY path 의 자동 검증: `scripts/e2e-pty-ghost.py` (zsh) +
`scripts/e2e-pty-bash.py` + `scripts/e2e-pty-fish.py` — 셋 다
ghost / accept+frecency / popup 박스 / Tab 네비 / PreExec 5종 체크.
fish 4.x 는 startup 시 터미널 capability 쿼리 (XTGETTCAP / DA / OSC 11)
응답을 기다리므로 bare-PTY harness 가 응답을 에뮬레이트한다 (실 터미널
에서는 비문제).

ZLE 와 figterm 은 **상호 배타** — `NERV_PTY=1` 감지 시 ZLE widget
자동 비활성. CLAUDE.md §4 invariant 행 참조.

베스트에포트 3종 (WezTerm/Alacritty/Kitty) 은 alacritty_terminal
의 screen state 정확도 덕분에 M1 에서 *보장* 등급 격상 검토 (M1
10주차 dogfooding 결과 기준).

## 7. 검증 방법론

### 7.1 자동 e2e (보장 등급 + tmux)

`expectrl` (Rust) 로 PTY + ANSI 캡처. 시나리오:

1. **render-only**: 팝업이 정확한 ANSI 시퀀스로 그려지는지 (캡처 후 정규식 매칭).
2. **interaction**: Tab/Esc/↑↓ 키 동작.
3. **redraw**: 입력 키 → 팝업 갱신 → 입력 라인 무손상.
4. **scroll**: 화면 하단에서 자동 스크롤 동작.
5. **tmux**: pane 분할, detach/attach.

CI 매트릭스 (M1):

```
matrix:
  os: [macos-13, macos-14]   # Intel + Apple Silicon
  terminal: [iterm2, terminal-app, tmux-iterm2, tmux-terminal-app]
```

GitHub Actions의 macOS runner 에서 iTerm2/Terminal.app 을 headless 로 띄우는 것은 까다롭다. 대안:

- **headless PTY 시뮬레이션** — `vt100` (Rust crate) 으로 가상 VT 구현, 시퀀스 비교.
- 실제 터미널 검증은 **로컬 macOS + nightly 자기 호스트 runner** 로 분리.

### 7.2 수동 1회 (베스트에포트 등급)

M0 / 매 마이너 릴리즈마다 다음 체크리스트:

- 설치 → `git c<Tab>` 추천 표시 → Tab 채택 → 정상 종료
- 화면 하단 스크롤 시나리오
- 한글 입력이 섞인 라인 (예: `git commit -m "한글 ⎵"`) 에서 cursor 어긋남 없는지
- Esc 로 팝업 닫기

체크리스트는 `docs/manual-test-checklist.md` 로 분리. M0 끝에 작성.

---

## 8. 사용자가 보고할 때

`docs/terminal-compat.md` 에 미등재 터미널에서의 이슈는 GitHub Issues 의 `terminal-compat` 라벨.

이슈 템플릿 필수 항목:

- 터미널 + 버전
- macOS 버전 + 아키텍처 (Intel/Apple Silicon)
- tmux 사용 여부 + 버전
- `nerv doctor` 출력 전체
- 재현 키 시퀀스
- 캡처 영상 또는 스크린샷

베스트에포트 등급 격상은 (a) 검증 가능한 재현 절차 + (b) 사용자 수요 시그널 (이슈 ★ 수) 기반.

---

## 9. 비목표

- Windows Terminal, Linux 터미널 (alacritty-linux, gnome-terminal 등) — v1.4 (Linux) 까지 검토 안 함.
- 24-bit true color — v2.x.
- 이미지 / 그래픽 프로토콜 — 영구 비목표 (자동완성 도구의 필요 영역 아님).
- 마우스 인터랙션 (스크롤, 클릭으로 추천 선택) — v2.x.

---

## 10. 변경 트리거

- 새 macOS 메이저 릴리즈에서 Terminal.app 동작 변경
- iTerm2 메이저 릴리즈에서 ANSI 처리 변경
- tmux 4.x 출시
- 베스트에포트 → 보장 격상 결정 (수요 시그널 충족 시)

---

*문서 v1.1 — PLAN.md v0.5 §11 의 정밀 명세. v1.0 → v1.1 변경: §1 매트릭스에서 Warp / VS Code / JetBrains / Hyper 제거 (Triage 정책으로 대체), §5.1 iTcerm2 오타 수정, §5.4 위치 계산 일반 원칙으로 통합. M0-5 에 zsh-autosuggestions 공존 e2e 시나리오 추가 (PLAN v0.5 정합).*
*v1.2 — PLAN.md v0.6 정합. PRD §5.8 figterm opt-in 도입으로 §6.4 신설 (M1 nerv-pty path 와 ZLE path 의 차이 + 상호 배타 + Apple 서명 요건 + 베스트에포트 격상 검토). §1 매트릭스 자체는 변경 없음 (M0 기준 유지). 변경 트리거: figterm 의 M1 dogfooding 결과로 베스트에포트 → 보장 격상.*
*v1.3 — §3.1 신설 (icon glyph width contract — `sanitize_icon` 이 unicode-width width==2 강제, ambiguous-width 거부). 위젯의 "non-ASCII = 2 cells" 가정을 엔진이 책임지는 contract 를 명시. 변경 트리거: 5th wire 필드 도입 (per-row width 명시 전송) 시 본 절 deprecate.*
*v1.4 — §6.4 현행화: bash/fish PTY path 출하 반영 (fish "M1+1" 예정 → M1 출하). 부트스트랩 파일명 (`post.*`/`pre.sh` 구상 → `_nerv-pty.{zsh,bash,fish}` 실명), `NERV_PTY_BIN` sibling lookup, OSC 697 `Shell=` 마커 게이트, PTY e2e 3종 (`e2e-pty-{ghost,bash,fish}.py`) 명시. fish 4.x capability-query 주의 추가.*
