# Error States — UX 명세서

> **Status**: 인수 기준 (글이 코드보다 먼저). PLAN.md v0.4 §5.5 정합.
> **원칙**: *"고장은 조용히 알리고, 고치는 한 줄을 함께 보여준다."*

---

## 1. 설계 원칙

1. **자동 감지 우선** — `nerv doctor` 를 사용자가 *직접 실행하지 않아도* 시스템이 먼저 알린다.
2. **시끄럽지 않게** — 프롬프트를 가리거나 입력을 막지 않는다. 회색 1줄 + 디바운스.
3. **조치 1줄 동봉** — *어디가 문제인지* 가 아니라 *무엇을 치면 고쳐지는지* 를 보여준다.
4. **차라리 비활성** — 의심스러운 상태에서는 자동완성을 *조용히 끈다*. 잘못된 추천보다 추천 없는 게 낫다.
5. **사용자 데이터 보호** — 어떤 에러도 `.zshrc` 등 사용자 파일을 손상시키지 않는다.

---

## 2. 5종 에러 상태 (확정 카탈로그)

| ID | 상황 | 감지 시점 | 수단 | 사용자 화면 |
|----|------|----------|------|------------|
| **E1** | `nervd` 데몬 미기동 | ZLE widget 첫 키 입력 시 UDS 연결 실패 | `connect(2)` `ECONNREFUSED` 또는 소켓 부재 | 회색 1줄 hint (§3.1) |
| **E2** | spec 파일 손상 / 파싱 실패 | 데몬 시작 시 + lazy load 시 | JSON parse error | 해당 spec만 비활성, stderr 1줄 (§3.2) |
| **E3** | zsh < 5.8 | `nerv init zsh` 실행 시 | `$ZSH_VERSION` 비교 | stderr 경고 + `exit 0`, 자동완성 비활성 (§3.3) |
| **E4** | ZLE 위젯 충돌 | `nerv init zsh` 실행 시 + 데몬 시작 시 | 환경 변수 / 알려진 시그니처 휴리스틱 | stderr 경고 + URL (§3.4) |
| **E5** | spec 버전 불일치 | 데몬 시작 시 | `specs-prebuilt/manifest.json` 의 schema 버전 vs 데몬 빌드 버전 | stderr 1줄 + `doctor` (§3.5) |

위 5개 외의 모든 에러는 **fail-quiet** — 자동완성을 끄고 stderr 한 줄 + 다음 키 입력에 영향 없음.

---

## 3. 케이스별 상세

### 3.1 E1 — `nervd` 데몬 미기동

**감지**:

- ZLE widget 이 UDS (`~/Library/Caches/nerv/nervd.sock`) 에 connect 시도.
- 실패 사유: 소켓 파일 부재 / `ECONNREFUSED` / `EACCES`.

**사용자 화면** (입력 라인 *아래* 회색 1줄):

```
[nerv] daemon not running — run: nerv start
```

**규칙**:

- 세션 1회만 표시. 같은 zsh 세션에서 재시도 시 메시지 억제.
- ZLE 가 5초 동안 추가 connect 시도하지 않음 (재연결 폭주 방지).
- 사용자가 키 입력 계속 가능 — 자동완성 단순 비활성.
- `nerv start` 성공 후 다음 키부터 자동 복구 (별도 안내 없음).

**테스트**:

```
GIVEN: nervd 미기동
WHEN: zsh 세션에서 git ⎵
THEN:
  - 입력 차단 없음
  - 회색 1줄 1회 표시
  - 같은 세션에서 다시 git ⎵ 시 메시지 추가 표시 안 됨
```

---

### 3.2 E2 — spec 파일 손상 / 파싱 실패

**감지**:

- 데몬이 `specs-prebuilt/<name>.json` 을 lazy load 할 때 JSON parse error 또는 schema mismatch.
- 데몬 시작 시 manifest 와 실제 파일 sha256 mismatch.

**사용자 화면**:

- 해당 spec **만** 비활성 — 다른 spec 의 자동완성은 정상.
- stderr (데몬 로그 `~/Library/Logs/nerv/nervd.log`):
  ```
  [nerv] spec disabled: docker — invalid JSON at line 142 (run `nerv doctor` for details)
  ```
- 사용자 인터랙티브 셸에는 *직접 표시하지 않음* — soft fail. `nerv doctor` 가 항목으로 보고.

**규칙**:

- 한 spec 의 손상이 데몬 전체를 죽이지 않는다.
- 손상 spec 은 `nerv spec list` 에 `docker (disabled — corrupt)` 로 표기.
- 다음 데몬 재시작 또는 `brew upgrade nerv` 시 재시도. (v1.0 에서는 spec 이 바이너리에 내장 — 별도 spec 갱신 명령 없음. PLAN.md GO 조건 ① 정합.)

**테스트**:

```
GIVEN: specs-prebuilt/docker.json 의 마지막 } 를 제거 (의도적 손상)
WHEN: nervd 시작 + git/docker 자동완성 시도
THEN:
  - git 자동완성 정상
  - docker 자동완성 비활성 (제안 0개)
  - nerv doctor 가 "docker: disabled — corrupt JSON" 표시
  - nervd 프로세스 살아있음
```

---

### 3.3 E3 — zsh < 5.8

**감지**: `nerv init zsh` 가 `$ZSH_VERSION` 환경 변수를 파싱.

```bash
# 의사코드
zver="${ZSH_VERSION:-unknown}"
if ! version_ge "$zver" "5.8"; then
  warn_and_exit_zero
fi
```

**사용자 화면** (stderr):

```
[nerv] zsh 5.8+ required (current: 5.6.2) — please upgrade.
       Suggested: brew install zsh && chsh -s $(brew --prefix)/bin/zsh
       Autocomplete will be disabled until zsh is upgraded.
```

**규칙**:

- `exit 0` — `eval "$(nerv init zsh)"` 가 비-0 종료 시 사용자 `.zshrc` 가 깨질 수 있음. 따라서 정상 종료하되 hook 출력은 빈 문자열.
- macOS 기본 zsh (보통 5.9+) 면 통과.
- bash/sh 환경에서 `nerv init zsh` 를 잘못 실행해도 same — `exit 0` + 셸 안내 메시지.

**테스트**:

```
GIVEN: ZSH_VERSION=5.6.2 (시뮬레이션)
WHEN: eval "$(nerv init zsh)"
THEN:
  - exit code 0
  - stderr 위 메시지 표시
  - .zshrc 에 마커 블록 추가되지 않음
  - 다음 zsh 세션이 정상 시작됨 (에러 0)
```

---

### 3.4 E4 — ZLE 위젯 충돌

**감지** (`nerv init zsh` 실행 시 + 데몬 시작 시 두 단계):

`init zsh` 단계 — 환경에서 알려진 라이벌 도구의 흔적 검사:

- `$ZSH_AUTOSUGGEST_USE_ASYNC` 정의 → zsh-autosuggestions 사용 중
- `$ZSH_AUTOCOMPLETE` 또는 `_zsh_autocomplete` 함수 정의 → zsh-autocomplete 사용 중
- `bindkey -M menuselect` 출력 비어있지 않음 → fzf-tab 또는 유사 도구
- `_fzf_complete` 함수 → fzf 자동완성 통합
- `q init zsh` 흔적 (`# Amazon Q` 문자열) — Amazon Q 와 동시 사용

데몬 시작 단계 — ZLE 진단 ping (`zle -lL` 출력 분석) 으로 widget 등록 충돌 검사.

**사용자 화면** (stderr, init zsh 시):

```
[nerv] detected zsh-autocomplete — Nerv runs alongside but key bindings may conflict.
       See: https://nerv.sh/docs/conflicts#zsh-autocomplete
```

여러 도구가 감지되면 모두 1줄씩.

**규칙**:

- 차단하지 않음 — 사용자가 *공존 OK* 라고 판단할 수 있다. hook 은 정상 설치.
- `nerv doctor` 가 항목으로 보고 (충돌 강도 1–3 등급).
- 위 URL 페이지에서 도구별 권장 충돌 회피 설정 (예: zsh-autosuggestions 의 `ZSH_AUTOSUGGEST_USE_ASYNC=1` 비활성, Nerv 의 `bindkey` 우선순위 등) 안내.

**위젯 등록 정책**:

- Nerv 의 ZLE widget 이름은 `__nerv_complete` (이중 언더스코어 prefix). 충돌 가능성 최소화.
- 키 바인딩은 v1.0 에서 `Tab` 만 사용, 다른 키는 권장 — 그러나 사용자가 disable 가능 (`config.toml`).

**테스트**:

```
GIVEN: zsh-autosuggestions 가 oh-my-zsh 플러그인으로 활성
WHEN: eval "$(nerv init zsh)"
THEN:
  - stderr 충돌 안내 1줄
  - 마커 블록은 정상 추가
  - 다음 키 입력 시 양쪽 모두 동작 (Nerv 추천 + 자동제안 ghost text)
  - 충돌 경고 후 5초 디바운스 → 같은 세션에서 재출력 X
```

---

### 3.5 E5 — spec 버전 불일치

**감지**:

- 데몬 시작 시 `specs-prebuilt/manifest.json` 의 `schema_version` (예: `2`) 과 데몬 빌드의 supported schema 비교.
- 사용자가 수동으로 `specs-prebuilt/` 를 다른 버전 nerv 의 것으로 바꿔치기한 경우 발생.

**사용자 화면** (stderr, 데몬 로그):

```
[nerv] spec schema mismatch — daemon expects v2, found v1.
       Run: brew reinstall nerv  (또는: nerv doctor)
       Autocomplete disabled until resolved.
```

추가로 ZLE 가 첫 키 입력 시 회색 1줄:

```
[nerv] spec mismatch — run: brew reinstall nerv
```

**규칙**:

- 자동완성 전체 비활성 (단일 spec 비활성 vs 시스템 전체 비활성의 차이).
- `nerv doctor` 가 차단 사유로 표시 (붉은색).
- 데몬은 살아 있되 추천 응답 빈 배열.

**테스트**:

```
GIVEN: manifest.json 의 schema_version 을 손으로 1 로 변경
WHEN: nerv start && zsh 세션에서 git ⎵
THEN:
  - 자동완성 0개
  - stderr / log 위 메시지
  - ZLE 회색 1줄 1회
  - nerv doctor 가 "spec schema mismatch" 항목 표시 (red)
```

---

## 3.6 `nerv doctor` 자동 실행 트리거 ★ 신설

§1 원칙 1번 ("자동 감지 우선") 을 보장하는 마지막 장치 — 사용자가 `nerv doctor` 를 *직접 입력하지 않아도* 다음 시점에 자동으로 진단이 1회 실행되어 문제 발견 시 1줄 요약을 출력한다.

| 자동 실행 시점 | 표시 정책 |
|----------------|----------|
| `nerv start` 직후 | 데몬 시작이 성공해도 자동 진단 1회. 결과는 다음 §3.6.1 형식 |
| 데몬 첫 IPC 응답 직전 | ZLE 첫 키 입력 시점에 데몬이 lazy 진단 1회 (E2/E5 대비) |
| `brew upgrade nerv` 후 첫 zsh 세션 | 마커 블록의 `Version` 메타가 변경됐음을 감지하면 첫 prompt 직전 1회 |

### 3.6.1 출력 정책

진단 결과 *모든 항목 OK* → **아무 것도 출력하지 않음** (조용한 성공).

진단 결과 1개 이상 warning/error → 입력 라인 *위* 회색 1줄 (예시):

```
[nerv] doctor: 1 warning, 0 errors — run: nerv doctor
```

### 3.6.2 spec age 안내 (soft notice)

내장된 spec 의 빌드 일자가 **30일 이상 경과** 한 경우 `nerv doctor` 가 항목으로 표시:

```
  ℹ specs               built 47 days ago
                        → run: brew upgrade nerv  (선택)
```

- 강제 X — `info` 등급 (회색 ℹ 아이콘).
- exit code 영향 X (성공으로 카운트).
- 본 항목은 *자동 실행* 시 1줄 요약에 포함되지 *않는다* (소음 방지).
- 사용자가 `nerv doctor` 를 직접 실행했을 때만 표시.

### 3.6.3 디바운스

- `nerv start` 직후 자동 진단은 세션당 1회 만 — 이후 키 입력에 영향 X.
- 같은 zsh 세션에서 데몬 재시작이 있어도 추가 표시 억제 (24시간).
- `brew upgrade` 감지 후 진단은 *1회 한정*, 마커 블록 메타 갱신 후 silent.

### 3.6.4 비목표

- 백그라운드에서 주기적으로 자동 실행하는 daemon timer X.
- 네트워크 호출 (latest version 확인 등) X. spec age 는 빌드 메타 (`manifest.json` 의 build_date) 만 비교.
- 사용자 모르게 자동 수정 (auto-fix) X.

---

## 4. 메시지 톤 가이드

- **언어**: 영어 (CLI 표준, 국제 사용자 대상). 한글 번역은 v1.x 검토.
- **포맷**: `[nerv] <문제>` — `<조치>`. 한 줄. 80자 이하 우선.
- **추측 금지**: "may be"/"might" 표현 사용 안 함. 확정된 진단만 표시.
- **이모지/색**: 회색만 사용 (ANSI `\033[2;37m`). 빨강/노랑은 `nerv doctor` 출력에만.
- **사과/완곡어구 금지**: "Sorry"/"Oops" 사용 안 함. 사실만.

---

## 5. `nerv doctor` 출력 통합

`nerv doctor` 는 위 5종 모두를 자동 점검 + 추가 항목 (§5.1) 포함.

**출력 예시**:

```
nerv doctor

  ✓ zsh version          5.9 (>= 5.8)
  ✓ shell hook           ~/.zshrc 마커 블록 1개 (멱등 OK)
  ✓ daemon               nervd running (pid 12345, uptime 2h)
  ✓ spec schema          v2 (matches daemon)
  ℹ specs                built 47 days ago
                         → run: brew upgrade nerv  (선택)
  ⚠ widget conflicts     zsh-autosuggestions detected
                         → see https://nerv.sh/docs/conflicts
  ✗ spec health          1/50 disabled: docker (corrupt JSON, line 142)
                         → run: brew reinstall nerv

Result: 4 OK, 1 info, 1 warning, 1 error
```

**exit code**:

- `0` — 모두 OK 또는 경고만.
- `1` — 1개 이상의 error (`✗`).

### 5.1 doctor 가 추가로 점검하는 항목

- `~/.zshrc` 가 읽기 전용 / 심링크 / 마커 블록 누락
- nervd PID 파일은 있으나 프로세스 부재 (좀비 PID)
- 디스크 잔여 공간 < 100MB (캐시 쓰기 실패 위험)
- Homebrew 의 `nerv` formula 와 실제 바이너리 버전 불일치

각 항목은 본 §2 표에 추가 등재 없이 doctor 전용. 자동 표시 없음.

---

## 6. 로깅 정책

| 레벨 | 어디로 | 예시 |
|------|--------|------|
| ERROR | `~/Library/Logs/nerv/nervd.log` + stderr | E2 spec corrupt, E5 schema mismatch |
| WARN | `~/Library/Logs/nerv/nervd.log` | E4 widget conflict (한 번 기록 후 디바운스) |
| INFO | log 만 | 데몬 기동/종료, spec 로드 시간 |
| DEBUG | log 만 (`NERV_LOG=debug` 시) | IPC 메시지, parser 단계별 |

로그 회전: 10MB × 3 파일 (`.log`, `.log.1`, `.log.2`).

PII / 사용자 입력 내용은 *기록하지 않음*. 토큰화된 위치 (서브커맨드/플래그/인자) 만.

---

## 7. 구현 체크리스트 (M1 7주차 정합)

- [ ] E1: ZLE widget 의 connect 실패 처리 + 5초 디바운스 + 1회 표시
- [ ] E2: spec lazy load + per-spec disable + nerv doctor 보고
- [ ] E3: `nerv init zsh` 의 `$ZSH_VERSION` 분기 + `exit 0` + stderr
- [ ] E4: 알려진 5종 라이벌 도구 휴리스틱 + URL 안내 + 디바운스
- [ ] E5: manifest schema 버전 검사 + 전역 비활성 + 회색 hint
- [ ] doctor: 위 5종 + 추가 4종 + spec age (info) 통합 출력 + exit code 규약
- [ ] doctor 자동 실행 트리거 3종 (§3.6) — `nerv start` 직후 / 첫 IPC / `brew upgrade` 후
- [ ] 자동 실행 디바운스 (세션 1회, 24시간 억제)
- [ ] 로그 회전 + PII 미기록 + 디버그 모드
- [ ] e2e: 위 케이스별 테스트 5종 + 자동 실행 테스트 3종 + 통합 테스트 1종

---

*문서 v1.1 — PLAN.md §5.5 의 정밀 명세. v1.0 → v1.1 변경: §3.6 doctor 자동 실행 트리거 신설, spec age soft notice 추가, E2 의 잔존 `nerv spec update` 참조 제거 (PLAN GO 조건 ①). 변경 트리거: 새 라이벌 도구 출현, schema v3 도입, doctor 추가 항목 합의 시.*
