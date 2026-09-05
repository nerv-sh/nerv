# `nerv uninstall` — 인수 기준 명세서

> **Status**: 인수 기준 (글이 코드보다 먼저). PLAN.md v0.6 §5.4 정합.
> **출시 차단 요건**: 본 문서의 *모든* 인수 기준이 e2e 자동 테스트에서 통과해야 v1.0 출시.
> **v0.6 정합**: marker 블록 식별/제거 로직은 `nerv-shell` (자작, 보존) + `nerv-integrations` (← upstream `fig_integrations`, 흡수) 에 분산. uninstall 절차 자체는 변경 없음. nerv-pty (M1 figterm opt-in) 도입 시 PTY shim 바이너리 (`~/.local/bin/nerv-pty`) 가 §2 인벤토리에 8번으로 추가됨 — §11 참조.

---

## 1. 원칙

> *"깔끔히 떠날 수 있다는 신뢰가 설치를 유도한다."*

Nerv는 사용자의 시스템에 다음 4가지 흔적을 남긴다. uninstall은 이를 **모두** 제거해야 한다 (단, `--keep-config` 명시 시 설정 디렉터리만 보존).

1. `~/.zshrc` 의 마커 라인
2. 백그라운드 데몬 (`nervd`)
3. 캐시 디렉터리
4. 설정 디렉터리 (옵션)

---

## 2. 흔적 인벤토리

uninstall 이 식별·제거해야 할 모든 경로/리소스의 권위 있는 목록.

| # | 종류 | 경로 / 리소스 | 생성 주체 | `--keep-config` 시 |
|---|------|---------------|----------|-------------------|
| 1 | shell hook | `~/.zshrc` 의 `# >>> nerv >>>` ~ `# <<< nerv <<<` 블록 | `eval "$(nerv init zsh)"` (rc 에 마커 블록 자동 기록·멱등 갱신; `>> ~/.zshrc` 리다이렉트도 동일 블록) | **삭제** |
| 2 | 데몬 프로세스 | `nervd` (PID 추적: `~/Library/Caches/nerv/nervd.pid`) | `nerv start` 또는 자동 기동 | **종료** |
| 3 | 데몬 소켓 | `~/Library/Caches/nerv/nervd.sock` | nervd | 삭제 |
| 4 | 데몬 로그 | `~/Library/Logs/nerv/nervd.log` (+ 회전 파일) | nervd | 삭제 |
| 5 | 캐시 디렉터리 | `~/Library/Caches/nerv/` 전체 — `specs/`, `frecency.tsv`, **`misses.tsv`** (spec 없는 명령 로컬 집계, `nerv doctor` 의 `spec misses` 행 출처), **`derived/`** (명령의 `--help` 에서 자동 추출한 spec — spec-conversion-policy §6.2) 포함 | 다양 | 삭제 |
| 6 | 설정 디렉터리 | `~/.config/nerv/` (XDG_CONFIG_HOME 존중) — `nerv.toml` + **`specs/` 사용자 overlay spec** (spec-conversion-policy §6.1) 포함 | 사용자 또는 `nerv init` | **삭제** (옵션 시 보존 — overlay spec 도 `--keep-config` 로만 살아남는다) |
| 7 | Homebrew 흔적 | `/opt/homebrew/bin/nerv`, 동봉 spec (`…/share/nerv/specs/`), formula 메타 | `brew install` | brew 가 처리 |

> **v1.0 비대상**: LaunchAgent (`~/Library/LaunchAgents/sh.nerv.nervd.plist`) — v1.0 은 `nerv start` / `stop` 수동 라이프사이클만. 자동 기동 도입 (v1.x) 시점에 본 인벤토리에 8번으로 추가하고 §4 step 6 도 함께 부활한다.
>
> **v1.0 M0 비대상 (M1 도입 예정)**: `nerv-pty` (← upstream figterm) PTY shim 바이너리 — PRD v0.6 §5.8 의 opt-in path 도입 시 `~/.local/bin/nerv-pty` + `pre.sh` 의 `exec -a` 라인이 §2 인벤토리에 추가됨. uninstall 절차도 PTY shim 종료 + 바이너리 삭제 + pre.sh 라인 제거 단계 추가. §11 참조.

> **macOS 경로 일관성 원칙**: 캐시 = `~/Library/Caches/nerv/`, 로그 = `~/Library/Logs/nerv/`, 설정 = `~/.config/nerv/`.

---

## 3. 마커 블록 형식

`nerv init zsh` 가 `~/.zshrc` 에 추가하는 정확한 형식 (멱등성 보장의 핵심):

```sh
# >>> nerv >>>
# Managed by `nerv init zsh`. Do not edit between markers.
# Version: 1.0.0
# Installed: 2026-04-29T15:30:00Z
eval "$(/opt/homebrew/bin/nerv init zsh --shell-script)"
# <<< nerv <<<
```

규칙:

- 시작 마커 `# >>> nerv >>>` 와 종료 마커 `# <<< nerv <<<` 는 **고정 문자열**. 변경 금지.
- `Version` / `Installed` 메타 라인은 갱신 시 덮어쓴다.
- 마커 사이 라인은 `nerv init zsh` 출력으로 *완전 대체* 한다 (사용자 수정 무효).
- `nerv init zsh` 를 여러 번 실행해도 마커 블록은 **항상 1개만** 존재해야 한다.

**v0.6 구현 매핑**: 마커 블록의 쓰기/탐지/삭제는 `crates/nerv-shell` (자작, 마커 상수 `MARKER_BEGIN` / `MARKER_END` + 4 unit test) 이 권위. `crates/nerv-integrations` (← upstream fig_integrations 흡수) 의 `pre.sh` / `post.zsh` 스크립트가 마커 블록 *내부* 본문을 제공. CLAUDE.md §4 invariant 의 "marker 교체 필수" 행 참조 — 흡수 시 fig 의 마커는 nerv 마커로 전수 교체됐는지 grep 검증.

---

## 4. uninstall 절차 (정확한 순서)

```
1. lock 획득 (~/Library/Caches/nerv/uninstall.lock)
2. 데몬 graceful stop:
   a. nervd.pid 읽기
   b. SIGTERM 전송 → 5초 대기
   c. 살아있으면 SIGKILL → 1초 대기
   d. nervd.sock 파일 삭제
3. shell hook 제거:
   a. 모든 알려진 셸 init 파일 스캔 — zsh: ~/.zshrc, ~/.zshenv, ~/.zprofile, ~/.zlogin / bash: ~/.bashrc, ~/.bash_profile, ~/.profile / fish: ~/.config/fish/config.fish. `nerv init {zsh,bash,fish}` 가 동일 마커 블록을 emit 하므로 셋 다 제거 대상 (bash/fish hook 잔존 시 nerv 부재에도 셸 시작마다 `eval "$(nerv …)"` → command-not-found 에러 — §114 위반).
   b. 마커 블록 (시작~종료 마커 포함) 추출
   c. 블록 1개씩 모두 제거 (여러 개 발견 시 모두)
   d. 마커 블록만 제거된 결과를 atomic write (임시파일 → rename)
   e. 백업: 원본을 ~/.zshrc.nerv-backup-<timestamp> 로 1회 복사
4. 캐시 삭제: rm -rf ~/Library/Caches/nerv/
5. 로그 삭제: rm -rf ~/Library/Logs/nerv/ (단, 본 uninstall 의 로그 §10 은 보존)
6. 설정 삭제 (--keep-config 미지정 시): rm -rf ~/.config/nerv/
7. lock 해제
8. 사용자 알림: stdout 1줄 — "nerv removed. backup: ~/.zshrc.nerv-backup-<ts>"
```

**원자성 (atomicity)**:

- `~/.zshrc` 수정은 *반드시* 임시파일 + `rename(2)` 로 처리 — uninstall 중 SIGKILL을 받아도 `.zshrc` 가 깨지지 않아야 한다.
- 각 단계는 멱등 — 중간 실패 후 재실행 가능해야 한다.

**실패 시 동작**:

- 단계별 실패는 stderr에 1줄 + 다음 단계 계속 (best effort).
- 완료 후 `nerv: 7/8 steps OK, 1 warning — see above` 형식 요약.
- exit code: `0` (완전 성공) / `1` (부분 실패, 흔적 일부 잔존) / `2` (lock 획득 실패).

---

## 5. `--keep-config` 옵션

```bash
nerv uninstall --keep-config
```

- §2 인벤토리 표의 *"--keep-config 시"* 컬럼 따름.
- 보존: `~/.config/nerv/` 만.
- 삭제: 그 외 모두 + zshrc 마커 블록 제거.
- 재설치 시 기존 사용자 설정 복원.

**⚠️ shell hook 은 항상 제거** — config 보존이 hook 보존을 의미하지 않는다 (hook 이 남아있으면 nerv 가 없는데도 zsh 시작 시 에러가 난다).

---

## 6. brew 통합

`brew uninstall nerv` 도 본 문서의 *모든 인수 기준* 을 만족해야 한다.

Homebrew formula 의 `post_uninstall` 훅에서:

```ruby
def post_uninstall
  system "#{bin}/nerv", "uninstall", "--quiet" if File.exist?("#{bin}/nerv")
end
```

`caveats` 사용자 안내:

```
nerv has been removed.
A backup of your .zshrc was saved to ~/.zshrc.nerv-backup-<timestamp>.
Restart your shell to complete cleanup.
```

`brew uninstall` 후 `nerv` 바이너리는 brew 가 제거하므로, post_uninstall 은 brew 가 바이너리 삭제하기 *전에* 실행되어야 한다 (Homebrew 의 기본 순서).

---

## 7. 인수 기준 (e2e 테스트 시나리오)

각 시나리오는 `expectrl` 기반 e2e 로 자동화. M1 12주차 베타 체크포인트의 차단 요건.

### 7.1 정상 경로 (Happy path)

```
GIVEN: 깨끗한 macOS + zsh 5.8+, brew 설치됨
WHEN:
  brew install nerv-sh/tap/nerv
  eval "$(nerv init zsh)"
  echo "source ~/.zshrc" | zsh -i -c 'git c<TAB>'   # 추천 1회 사용
  nerv uninstall
THEN:
  - exit code = 0
  - ~/.zshrc 에 마커 블록 0개
  - ~/.zshrc.nerv-backup-* 1개 존재
  - nervd 프로세스 0개
  - ~/Library/Caches/nerv/ 디렉터리 부재
  - ~/.config/nerv/ 디렉터리 부재
  - ~/Library/Logs/nerv/ 디렉터리 부재 (uninstall 자체 로그 제외)
  - 새 zsh 세션 시작 시 에러 없음
```

### 7.2 멱등성

```
WHEN: nerv uninstall 을 2회 연속 실행
THEN: 두 번째 실행도 exit 0, 메시지는 "nerv: nothing to remove"
```

### 7.3 사용자 .zshrc 수정 보호

```
GIVEN: 사용자가 .zshrc 의 마커 *외부* 에 자신의 코드를 직접 추가한 상태
WHEN: nerv uninstall
THEN:
  - 마커 외부 라인은 100% 보존 (line-by-line diff = 마커 블록 제외 0)
  - 사용자 코드의 들여쓰기/공백/주석 모두 보존
```

### 7.4 마커 외부의 nerv 라인은 건드리지 않음

```
GIVEN: 사용자가 .zshrc 어딘가에 무관한 "nerv" 문자열 (예: 변수명, 별칭)을 가짐
WHEN: nerv uninstall
THEN: 그 라인은 보존. 마커 블록만 제거됨
```

### 7.5 데몬이 응답하지 않을 때

```
GIVEN: nervd 가 SIGSTOP 으로 멈춰 있는 상태
WHEN: nerv uninstall
THEN:
  - SIGTERM 후 5초 타임아웃 → SIGKILL 전송
  - 종료 코드 0
  - stderr 에 "warning: daemon required SIGKILL" 1줄
```

### 7.6 권한 부족 (write protected file)

```
GIVEN: ~/.zshrc 가 chmod 444 로 읽기 전용
WHEN: nerv uninstall
THEN:
  - exit code 1
  - stderr: "error: cannot write ~/.zshrc — fix permissions and re-run"
  - 다른 단계는 best effort 진행됨
```

### 7.7 `--keep-config`

```
GIVEN: ~/.config/nerv/config.toml 사용자 편집 + ~/.config/nerv/specs/claude.json (overlay spec) 존재
WHEN: nerv uninstall --keep-config
THEN:
  - ~/.config/nerv/config.toml 보존
  - ~/.config/nerv/specs/claude.json 보존 (overlay 는 설정 디렉터리의 일부)
  - 그 외 모든 흔적 §2 인벤토리대로 제거
```

### 7.8 `brew uninstall` 동등성

```
GIVEN: Happy path 7.1과 동일 상태
WHEN: brew uninstall nerv 실행 (nerv uninstall 직접 호출 X)
THEN: 7.1의 THEN 모두 만족
```

### 7.9 다중 마커 블록 정리

```
GIVEN: .zshrc 에 마커 블록이 2개 이상 (사용자가 nerv init 을 잘못 두 번 추가)
WHEN: nerv uninstall
THEN: 모든 마커 블록 제거 (개수 = 0)
```

### 7.10 zsh 외 셸이 기본일 때

```
GIVEN: chsh -s /bin/bash 후 .zshrc 에 마커 존재 (사용자가 셸 변경했지만 nerv 안 지움)
WHEN: nerv uninstall
THEN: 정상 동작 (zsh 가 현재 기본 셸인지와 무관하게 마커 제거)
```

---

## 8. 비목표 (uninstall 이 *하지 않는* 것)

- Homebrew tap 제거 (`brew untap nerv-sh/tap`) — 사용자가 명시적으로.
- `~/.zshrc.nerv-backup-*` 백업 파일 정리 — 사용자가 검토 후 직접.
- `oh-my-zsh` 의 `plugins=(... nerv)` 항목 자동 제거 — 매니저별 가이드(`docs/install-zsh.md`)에서 안내.
- 사용자가 직접 만든 alias / 함수 중 `nerv` 명령에 의존하는 것의 식별.
- 다른 사용자 계정의 흔적 (uninstall 은 현재 `$HOME` 만 처리).

---

## 9. 보안 고려

- `rm -rf` 는 항상 검증된 절대 경로로만. `$HOME` 이 빈 문자열이면 즉시 abort.
- 심볼릭 링크 따라가지 않음 (`--keep-config` 시에도 `~/.config/nerv/` 가 심링크면 따라가지 않고 심링크만 삭제).
- `~/.zshrc.nerv-backup-*` 백업 파일 권한 = 원본과 동일 (`stat` 후 `chmod`).

---

## 10. 로깅

uninstall 의 모든 단계는 `~/Library/Logs/nerv/uninstall-<timestamp>.log` 에 기록. `--quiet` 옵션 시 stdout 만 억제, 로그는 유지.

uninstall 자체의 로그는 §4 step 5 ("로그 삭제")에서 제거되지 않는다 — 디버깅을 위해 *남긴다*. 사용자가 직접 정리하거나, 다음 nerv 설치 시 자동 정리.

**v0.6 구현 매핑**: 로그 디렉터리 (`~/Library/Logs/nerv/`) 의 생성/회전은 `crates/nerv-log` (← upstream fig_log 흡수, 경로 재배선) 가 담당. PII 미기록 정책 (§3.5) 은 흡수 후에도 유지 (CLAUDE.md §4 invariant 의 "Q 경로 잔존 금지" 행 참조).

---

## 11. M1 figterm (`nerv-pty`) opt-in 시 인벤토리 추가분

PRD v0.6 §5.8 의 opt-in path 가 활성화되면 (`nerv init zsh --pty`) 본 §2 표에 다음 항목이 추가된다:

| # | 종류 | 경로 / 리소스 | 생성 주체 | `--keep-config` 시 |
|---|------|---------------|----------|-------------------|
| 8 | PTY shim 바이너리 | `~/.local/bin/nerv-pty` | `nerv init zsh --pty` | 삭제 |
| 9 | pre.sh 의 `exec -a "<shell> (nerv-pty)" nerv-pty` 라인 | `~/.zshrc` 의 마커 블록 내부 (또는 `nerv-integrations/pre.sh` 가 sourcing 된 위치) | `nerv init zsh --pty` | 삭제 |
| 10 | runtime 소켓 디렉터리 | `$XDG_RUNTIME_DIR/nervrun/` (Linux) / `$TMPDIR/nervrun/` (macOS) | `nerv-pty` 첫 기동 | 삭제 |
| 11 | data 디렉터리 | `~/Library/Application Support/nerv/` (macOS) / `$XDG_DATA_HOME/nerv/` (Linux) | `nerv-pty` settings/state | **보존** |

**브랜드 정합 참고**: figterm path 가 활성화되면 다음 env var 와 설정
키가 사용자 환경에 잔존할 수 있다 — uninstall 은 이들을 *직접*
지우지 않지만 figterm 종료 후 다음 셸 세션에는 영향이 없다 (process
env 한정):

- env vars: `NERV_PTY_SESSION_ID`, `NERV_PARENT`, `NERV_TERM`,
  `NERV_SHELL`, `PROCESS_LAUNCHED_BY_NERV` (figterm 이 child shell 에
  주입 — 셸 종료 시 사라짐)
- settings keys: `pty.enabled`, `pty.csi-u.enabled` (data 디렉터리의
  `settings.json` — `--keep-config` 가 아닐 때만 삭제 대상)

uninstall 절차 (§4) 에 PTY shim 종료 단계 추가:

```
2.5. PTY shim 종료:
     a. 모든 활성 zsh 세션에 SIGTERM (사용자 confirm 후, ─force 시 자동)
        — 또는 pre.sh 의 exec 교체만 제거하고 새 셸부터 ZLE 복귀
     b. ~/.local/bin/nerv-pty 삭제
```

ZLE-only 사용자는 본 §11 무관 (인벤토리 §2 그대로). figterm
opt-in 사용자만 §11 추가분도 검증 대상.

---

*문서 v1.1 — PLAN.md v0.5 §5.4 인수 기준의 정밀 명세. v1.0 → v1.1 변경: §2 인벤토리에서 LaunchAgent 제거 (v1.0 비대상), §4 절차에서 LaunchAgent 단계 삭제, 단계 번호 9→8 재정렬. 변경 트리거: M0-1 PoC 결과로 데몬 IPC 구조 변경 시, LaunchAgent 자동 기동 도입 (PLAN v?.x) 시.*
*v1.3 — PLAN.md v0.6 정합. v1.1 → v1.3 변경: §2 / §3 / §10 에 v0.6 구현 매핑 행 추가 (nerv-shell + nerv-integrations + nerv-log 흡수 정합), §11 신설 (M1 figterm opt-in 시 인벤토리 추가분 + uninstall 절차 단계 2.5). 본문 인수 기준 자체는 변경 없음 (PRD §5.4 그대로). 변경 트리거: figterm M1 도입 commit, LaunchAgent 자동 기동 도입.*
*v1.4 — 브랜드 strip 정합. §11 에 runtime 소켓 디렉터리 (`$XDG_RUNTIME_DIR/nervrun/`, 이전 `cwrun`) 와 data 디렉터리 (`~/Library/Application Support/nerv/`, 이전 `amazon-q`) 행 추가 + figterm path env vars / settings keys 잔존 정책 절 신설. 인수 기준 / 절차 자체는 무변경 — Q→NERV 리네이밍 결과를 인벤토리에 반영한 정합 갱신. 변경 트리거: figterm runtime 정합 dogfooding 결과로 추가 인벤토리 발견 시.*
*v1.5 — 캐시 인벤토리 정합 (2026-09-05). v1.4 → v1.5 변경: §2 행 5 에 `misses.tsv` (spec-miss 로컬 집계) 와 `derived/` (`--help` 파생 spec 캐시) 명시. 둘 다 `~/Library/Caches/nerv/` 아래라 §4 step 4 의 `rm -rf` 가 이미 덮는다 — 절차 무변경, 인벤토리 문서화만. 구현 측: rc 파일 재작성이 공용 `paths::write_atomic` 으로 통일돼 퍼미션 보존 + 실패 시 temp 파일 unlink (흔적 0 계약이 실패 경로까지 적용). 변경 트리거: 캐시 디렉터리 밖에 새 산출물이 생길 때.*
