# First 5 Minutes — 사용자 시나리오 명세서

> **Status**: 인수 기준 (글이 코드보다 먼저). PLAN.md v0.4 M0-7 / §5.3 정합.
> **목적**: 설치 후 5분간 사용자가 "아, 이게 되네" 를 느끼는 순간을 정의한다. 리텐션의 핵심.

---

## 1. 시나리오 개요

설치 직후의 신규 사용자가 가장 자주 만지는 3개 CLI — **git / docker / kubectl** — 에서 12단계 키 입력을 거치며 *어디서 추천이 뜨고 어디서 §5.1 동적 힌트가 뜨는지* 를 미리 정의한다.

이 시나리오는:

- M0 산출물 7번 — 녹화 영상 + 합격 기준 (`fail` 단계 ≤ 2 시 GO).
- M1 12주차 베타 검증의 회귀 시나리오.
- 마케팅용 GIF / 데모 영상의 *공식 스크립트*.

---

## 2. 환경 가정

- macOS 14 (Sonoma) 이상, Apple Silicon
- iTerm2 latest, 폰트 SF Mono 14pt, 화면 비율 16:9 (녹화 일관성)
- zsh 5.9 + 깨끗한 `.zshrc`
- `git`, `docker`, `kubectl` 가 PATH 에 존재 (Docker Desktop 설치, 빈 minikube 클러스터 OK)
- nerv 미설치 상태에서 시작

---

## 3. 12단계 시나리오

### 0단계 — 설치 (KPI: 30초)

```
$ brew install nerv-sh/tap/nerv
$ eval "$(nerv init zsh)"
```

**기대**: brew 출력 종료 후 새 프롬프트까지 ≤ 30초. 첫 실행 안내 1회 표시:

```
nerv installed. Try: git, docker, kubectl
Tip: press '?' on a suggestion to see what it does.
```

---

### 0.5단계 — 설치 실패 경로 (Fail Path) ★ 신설

happy path 만으로는 리텐션을 보장할 수 없다. 설치 실패 시 사용자가 보는 *첫 메시지* 가 이탈을 결정한다. 다음 3가지 실패 시나리오의 첫 화면을 정의한다.

#### 0.5-A — Xcode Command Line Tools 미설치

| 실패 지점 | 사용자가 보는 메시지 |
|----------|---------------------|
| `brew install nerv-sh/tap/nerv` | brew 가 자체적으로 `xcode-select --install` 안내 표시 |
| 그 후 `eval "$(nerv init zsh)"` | 사용자가 brew 안내를 따라 CLT 설치 후 재시도 → §0 정상 |

**Nerv 측 책임**:

- formula 의 `depends_on :macos => :ventura` (또는 그 이상) 명시.
- brew 가 처리하므로 추가 메시지 X. 단, `nerv doctor` 가 *Xcode CLT 미설치* 를 별도 진단 (사용자가 어떤 이유로 brew 우회 설치한 경우).

**합격 기준**: brew 안내가 표시되고, 사용자가 가이드를 따라 30분 안에 §0 으로 복귀 가능.

#### 0.5-B — oh-my-zsh 환경에서 `nerv init zsh` 첫 실행

| 입력 | 기대 화면 |
|------|----------|
| `eval "$(nerv init zsh)"` (oh-my-zsh + zsh-autosuggestions 활성 상태) | stderr 1줄 (E4 §3.4 형식): |

```
[nerv] detected zsh-autosuggestions — Nerv runs alongside but key bindings may conflict.
       See: https://nerv.sh/docs/conflicts#zsh-autosuggestions
[nerv] Tip: for cleaner integration, install via the oh-my-zsh plugin:
       https://github.com/nerv-sh/nerv-omz
```

**합격 기준**:

- `.zshrc` 마커 블록은 **정상 추가** (차단 X).
- 다음 키 입력에서 두 도구 모두 동작 (Nerv 팝업 + autosuggestions ghost text 동시).
- 시각적 충돌 없음 — 두 도구가 다른 화면 영역을 사용 (Nerv 는 입력 라인 *아래*, autosuggestions 는 입력 라인 *오른쪽*).
- 사용자가 *"옵션이 둘 다 있다"* 로 인지, *"고장났다"* 로 인지하지 않는다.

#### 0.5-C — 기존 마커 블록이 이미 존재 (재설치)

| 입력 | 기대 화면 |
|------|----------|
| `eval "$(nerv init zsh)"` (이전 nerv 의 마커 블록 잔존) | 마커 블록만 새 버전으로 갱신 (uninstall-spec §3 멱등성). stdout 안내: |

```
nerv: updated existing init block (v0.9.0 → v1.0.0)
```

**합격 기준**:

- 마커 블록 갯수 = 1 (중복 추가 X).
- 사용자의 다른 `.zshrc` 라인 손상 X.
- 갱신 사실이 사용자에게 보임 (silently 갱신은 X).

---

### 합격 기준 (0.5 전체)

- **0.5-A**: brew 안내 표시 + `nerv doctor` 가 별도 감지.
- **0.5-B**: 마커 블록 정상 추가 + 충돌 안내 1줄 + 두 도구 시각적 공존.
- **0.5-C**: 멱등 갱신 + 사용자 인지 가능한 안내.

위 3건 중 **2건 이상 통과** 시 0.5단계 합격. M0-7 의 차단 요건에 추가 (단계 0~12 합격 기준은 §4).

---

### git 파트 (4단계)

#### 1단계 — `git ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `git ` (스페이스 까지) | 인라인 팝업: `add`, `commit`, `push`, `pull`, `status`, `log`, `checkout`, `clone`, … |

**합격 기준**:

- 추천 ≥ 8개
- p95 응답 ≤ 25 ms
- 정렬: 알파벳 (v1.0) — frecency 는 v1.1
- `?` 키 → `add` 의 1줄 설명 (`Add file contents to the index`)

#### 2단계 — `git st⎵` (status 채택)

| 입력 | 기대 화면 |
|------|----------|
| `git st` | 추천: `status`, `stash` 만 (prefix 필터링) |
| `<Tab>` | `status` 채택 → 입력 라인이 `git status ` 로 갱신 |

**합격 기준**:

- prefix 필터링 정확
- 1개만 남았어도 자동 채택은 안 함 (사용자가 Tab 눌러야)
- Tab 후 팝업 자동 닫힘 (`Esc` 불필요)

#### 3단계 — `git log --⎵`

| 입력 | 기대 화면 |
|------|----------|
| `git log --` | 플래그 추천: `--oneline`, `--graph`, `--all`, `--author`, `--since`, … |

**합격 기준**:

- `--` prefix 인식 후 플래그만 추천 (서브커맨드 X)
- `?` 로 `--oneline` 의 설명: `Show each commit on a single line`
- 기간/저자 등 *값* 이 필요한 플래그는 `--since=<date>` 처럼 placeholder 표시

#### 4단계 — `git checkout ⎵` (★ 동적 힌트 케이스)

| 입력 | 기대 화면 |
|------|----------|
| `git checkout ` | 회색 1줄 힌트 (§5.1): |

```
⤷ 동적 완성은 v1.1에서 지원 예정 — 직접 입력하세요
   ▸ git branch --list 로 후보 확인
   ▸ 요청: https://github.com/nerv-sh/nerv/issues/new?template=dynamic.yml&cmd=git+checkout
```

**합격 기준**:

- 빈 추천이 아닌 *위 힌트* 가 표시될 것
- 5초 디바운스 — 같은 입력 라인에서 backspace + 재입력해도 즉시 재표시 X
- 사용자가 `main` 입력 후 → 정상 진행 (힌트는 사라짐)
- "고장났다" 라고 오해할 여지 없음

---

### docker 파트 (4단계)

#### 5단계 — `docker ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `docker ` | 추천: `ps`, `run`, `build`, `images`, `pull`, `push`, `exec`, `compose`, … |

**합격 기준**:

- ≥ 10개 추천
- `compose` 가 별도 서브커맨드로 표시 (Docker Compose v2)
- `?` 로 `run` 설명: `Create and run a new container from an image`

#### 6단계 — `docker ps -⎵`

| 입력 | 기대 화면 |
|------|----------|
| `docker ps -` | 짧은 플래그: `-a`, `-q`, `-l`, `-n`, `-s` |

**합격 기준**:

- 짧은 플래그 (`-a`) 와 긴 플래그 (`--all`) 가 별도 추천
- `?` 로 `-a` 설명: `Show all containers (default shows just running)`

#### 7단계 — `docker run --rm -it ubuntu:⎵` (★ 동적 힌트 케이스)

| 입력 | 기대 화면 |
|------|----------|
| `docker run --rm -it ubuntu:` | 회색 1줄 힌트 |

```
⤷ 동적 완성은 v1.1에서 지원 예정 — 직접 입력하세요
   ▸ docker images ubuntu 로 로컬 태그 확인
   ▸ 요청: https://github.com/nerv-sh/nerv/issues/new?template=dynamic.yml&cmd=docker+run
```

**합격 기준**:

- 4단계와 동일 행동 양식 (힌트 형식 일관)
- "Docker Hub 의 모든 태그를 가져와야 한다" 같은 자동 네트워크 호출 없음 (v1.0 비목표)

#### 8단계 — `docker compose ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `docker compose ` | `up`, `down`, `build`, `logs`, `ps`, `exec`, `restart` |

**합격 기준**:

- 2단계 서브커맨드 정확히 인식 (`docker` → `compose` → `up`)
- `?` 로 `up` 설명: `Create and start containers`

---

### kubectl 파트 (4단계)

#### 9단계 — `kubectl ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `kubectl ` | `get`, `describe`, `apply`, `delete`, `logs`, `exec`, `config`, `port-forward`, … |

**합격 기준**:

- ≥ 12개 추천
- `?` 로 `get` 설명: `Display one or many resources`

#### 10단계 — `kubectl get ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `kubectl get ` | 리소스 종류: `pods`, `services`, `deployments`, `nodes`, `configmaps`, `secrets`, `namespaces`, `ingresses`, … |

**합격 기준**:

- 정적으로 알려진 리소스 종류 만 표시 (CRD 는 동적이라 제외)
- ≥ 15개

#### 11단계 — `kubectl get pods -n ⎵` (★ 동적 힌트 케이스)

| 입력 | 기대 화면 |
|------|----------|
| `kubectl get pods -n ` | 회색 1줄 힌트 |

```
⤷ 동적 완성은 v1.1에서 지원 예정 — 직접 입력하세요
   ▸ kubectl get namespaces 로 후보 확인
   ▸ 요청: https://github.com/nerv-sh/nerv/issues/new?template=dynamic.yml&cmd=kubectl+-n
```

**합격 기준**:

- 4단계 / 7단계와 행동 일관

#### 12단계 — `kubectl describe pod ⎵` (또 다른 동적 케이스)

| 입력 | 기대 화면 |
|------|----------|
| `kubectl describe pod ` | 회색 1줄 힌트 (kubectl 컨텍스트 동적 인자) |

**합격 기준**:

- 4/7/11/12 모두 같은 hint UX — 일관성 검증.
- 사용자가 12단계까지 오면서 *어떤 인자가 동적인지* 직관적으로 학습.

---

## 4. 합격 기준 (전체)

M0 산출물 7번 (Go/No-Go) 의 차단 요건:

- 12단계 중 **10단계 이상 통과** 시 GO.
- 단, **0단계 (30초 KPI) + 0.5단계 (실패 path 3건 중 2건) + 4/7/11/12 동적 힌트 4건은 모두 통과** 가 별도 차단 요건.
- 통과 = 위 "기대 화면" 과 "합격 기준" 모두 충족.

세부 합격 기준:

| 카테고리 | 기준 |
|---------|------|
| 설치 / 실패 path | 0단계 30초 KPI + 0.5단계 3건 중 2건 합격 |
| latency | 1/2/3/5/6/8/9/10 단계 모두 p95 ≤ 25 ms |
| 추천 개수 | 1/5/9 단계 각 ≥ 8 (git), ≥ 10 (docker), ≥ 12 (kubectl) |
| `?` 도움말 | 1/3/5/8/9 단계에서 정확한 설명 표시 |
| 동적 힌트 | 4/7/11/12 단계 모두 §5.1 형식 일치, 디바운스 동작 |
| ANSI 무손상 | 모든 단계에서 입력 라인 텍스트 손상 X |

---

## 5. 녹화 사양

### 5.1 영상

- 도구: `asciinema` (텍스트 기반, GitHub README 임베드 가능)
- 보조: `ttygif` 또는 `vhs` 로 GIF 변환 (소셜 공유용)
- 길이 목표: ≤ 5분 (12단계 + 설치)
- 배속: 1.0x (실시간) — 마케팅용 GIF 만 1.5x
- 저장 위치: `docs/assets/first-5-min.cast`, `docs/assets/first-5-min.gif`

### 5.2 자막 / 주석

- 각 단계마다 1줄 캡션: `[1/12] git <Tab> — 서브커맨드 추천`
- 동적 힌트 단계는 노란색 강조: `[4/12] git checkout <Tab> — 동적 힌트`
- README 와 nerv.sh 랜딩 페이지에 임베드

---

## 6. 시나리오 데이터 준비

녹화 환경에서 미리 준비:

- `git status` 가 비어있지 않도록 임시 변경사항 1개 (`echo test > /tmp/x.txt && git -C /tmp init`)
- `docker ps` 가 1개 이상 컨테이너 표시 (`docker run -d --rm nginx:alpine`)
- `kubectl` 컨텍스트가 minikube 또는 kind 로 설정 — `kubectl get pods` 가 응답해야 함

깨끗한 macOS 가상머신 (UTM, multipass) 에서 시나리오 자동 셋업 스크립트:

```
docs/scripts/setup-first-5-min.sh
docs/scripts/teardown-first-5-min.sh
```

---

## 7. 비합격 사례 (안티패턴)

다음 중 하나라도 발생하면 즉시 fail:

- ❌ 어떤 단계든 입력 라인 텍스트가 깨짐
- ❌ 동적 힌트 자리에 빈 추천 (사용자가 "고장났다" 오해)
- ❌ 동적 힌트 자리에 잘못된 추천 (정적 변환이 부정확)
- ❌ `?` 키가 `?` 문자를 입력 라인에 삽입함
- ❌ Tab 이 일반 zsh 자동완성으로 fallback (Nerv hook 우선순위 실패)
- ❌ Esc 가 팝업을 닫지 않고 다른 동작 트리거
- ❌ 30초 KPI 초과 (M1 CI 가 측정)
- ❌ 추천 텍스트에 ANSI 시퀀스 가 누출되어 보임

---

## 8. v1.1 시나리오 예고 (확장 계획)

v1.1 동적 generator 도입 후 본 12단계의 4/7/11/12를 *실제 동적 추천* 으로 교체. 그 시점의 시나리오는 별도 `docs/first-5-min-v1.1.md` 로 작성.

마이그레이션 시:

- v1.0 사용자가 v1.1 로 업그레이드 시 동적 완성이 "갑자기 됨" — 사용자에게 *명시적 안내* 1회 (`nerv` 첫 실행 시 release notes 1줄).
- 본 시나리오의 4/7/11/12 단계는 v1.0 회귀 보장을 위해 v1.x 동안 보존 (legacy 모드).

---

## 9. 변경 트리거

- 50개 spec 후보 풀에서 git/docker/kubectl 중 하나가 빠지는 경우 → 시나리오 재작성
- §5.1 동적 힌트 메시지 형식 변경
- 30초 KPI 변경 (예: 20초로 강화)
- v1.1 동적 generator 출시 → 본 문서 deprecate, v1.1 문서로 이행

---

*문서 v1.1 — PLAN.md v0.5 §10 M0-7 의 정밀 명세. v1.0 → v1.1 변경: 0.5단계 (설치 실패 path 3건) 신설 — Xcode CLT 미설치 / oh-my-zsh 충돌 / 재설치 멱등 (CEO v0.4 GO 조건 ②). §4 합격 기준에 0.5단계 추가. 본 시나리오 통과가 v1.0 출시의 사용자 검증 차단 요건.*
