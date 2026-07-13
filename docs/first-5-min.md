# First 5 Minutes — 사용자 시나리오 명세서

> **Status**: 인수 기준 (글이 코드보다 먼저). PLAN.md v0.6 M0-7 / §5.3 정합.
> **목적**: 설치 후 5분간 사용자가 "아, 이게 되네" 를 느끼는 순간을 정의한다. 리텐션의 핵심.
> **v0.6 정합**: 본 시나리오의 12+0.5 단계는 v0.5 그대로. PRD v0.6 의 엔진 전환 (자작 → Fig Rust crates 흡수 + TS Rust 포팅) 은 *내부 구현 변경*, 사용자 체감 동일.
> **v1.3-v1.4 현행화**: M1 의 Tier B generator 실행 + well-known 패턴 회복 (Rust-native) 으로 동적 힌트 4단계 **전부 (4/7/11/12) 실완성으로 격상** (`git checkout` → 실제 branch 목록, `docker run` → 로컬 image 목록, `kubectl -n` → namespace 목록, `kubectl describe pod` → 리소스 목록). frecency 도 v1.1 예정 → 출하됨. 설명 표시는 `?` 키 구상 → **footer 자동 설명 (Fig style)** 로 변경.

---

## 1. 시나리오 개요

설치 직후의 신규 사용자가 가장 자주 만지는 3개 CLI — **git / docker / kubectl** — 에서 12단계 키 입력을 거치며 *정적 추천과 동적 완성 (Tier B generator) 이 각각 어디서 뜨는지* 를 미리 정의한다.

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
- 정렬: 알파벳 + frecency boost (재선택 항목이 상단으로 — 출하됨; 첫 사용 시엔 usage 데이터가 없어 순수 알파벳)
- 선택 행의 1줄 설명이 footer 에 자동 표시 (`add` 선택 시 `Add file contents to the index`) — `?` 키 불필요 (Fig style)

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
- `--oneline` 선택 시 footer 설명: `Show each commit on a single line`
- 기간/저자 등 *값* 이 필요한 플래그는 `--since=<date>` 처럼 placeholder 표시

#### 4단계 — `git checkout ⎵` (★ 동적 완성 케이스 — v1.3 격상)

| 입력 | 기대 화면 |
|------|----------|
| `git checkout ` | 실제 branch 목록: 현재 repo 의 로컬 + 원격 브랜치 (최근 커밋순) |

Tier B template generator (`git branch -a --sort=-committerdate`) 를
엔진이 직접 spawn (200ms timeout + TTL 5s LRU cache). cwd-aware —
클라이언트 셸의 CWD 기준 repo.

**합격 기준**:

- git repo 안에서: 실제 브랜치 이름들이 추천으로 표시 (`-`, `--` 정적 suggestion 포함)
- git repo 밖에서: generator 가 후보 0 → 빈 응답 (팝업 미표시, 입력 방해 X)
- generator 실행이 keystroke latency 를 깨지 않음 (cache hit 시 p95 ≤ 25 ms 유지)
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
- `run` 선택 시 footer 설명: `Create and run a new container from an image`

#### 6단계 — `docker ps -⎵`

| 입력 | 기대 화면 |
|------|----------|
| `docker ps -` | 짧은 플래그: `-a`, `-q`, `-l`, `-n`, `-s` |

**합격 기준**:

- 짧은 플래그 (`-a`) 와 긴 플래그 (`--all`) 가 별도 추천
- `-a` 선택 시 footer 설명: `Show all containers (default shows just running)`

#### 7단계 — `docker run --rm -it ⎵` (★ 동적 완성 케이스 — v1.3 격상)

| 입력 | 기대 화면 |
|------|----------|
| `docker run --rm -it ` | 로컬 image 목록 (Tier B template `docker images --format …`) |

**합격 기준**:

- 로컬에 pull 된 image 들이 추천으로 표시 (4단계와 동일 generator 머신)
- "Docker Hub 의 모든 태그를 가져와야 한다" 같은 자동 네트워크 호출 없음 (v1.0 비목표 — 로컬 `docker images` 출력만)
- **잔존 한계**: `ubuntu:⎵` 처럼 콜론 *뒤* tag 위치는 미지원 (token 중간 cursor-context — getQueryTerm string-form 범위 밖). 빈 응답이며 입력 방해 X

#### 8단계 — `docker compose ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `docker compose ` | `up`, `down`, `build`, `logs`, `ps`, `exec`, `restart` |

**합격 기준**:

- 2단계 서브커맨드 정확히 인식 (`docker` → `compose` → `up`)
- `up` 선택 시 footer 설명: `Create and start containers`

---

### kubectl 파트 (4단계)

#### 9단계 — `kubectl ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `kubectl ` | `get`, `describe`, `apply`, `delete`, `logs`, `exec`, `config`, `port-forward`, … |

**합격 기준**:

- ≥ 12개 추천
- `get` 선택 시 footer 설명: `Display one or many resources`

#### 10단계 — `kubectl get ⎵`

| 입력 | 기대 화면 |
|------|----------|
| `kubectl get ` | 리소스 종류: `pods`, `services`, `deployments`, `nodes`, `configmaps`, `secrets`, `namespaces`, `ingresses`, … |

**합격 기준**:

- 정적으로 알려진 리소스 종류 만 표시 (CRD 는 동적이라 제외)
- ≥ 15개

#### 11단계 — `kubectl get pods -n ⎵` (★ 동적 완성 케이스 — v1.4 격상)

| 입력 | 기대 화면 |
|------|----------|
| `kubectl get pods -n ` | 실제 namespace 목록 (`kubectl get namespaces --no-headers -o custom-columns=:metadata.name`) |

당초 유일한 잔존 gap 이었다. 원본 Fig spec 의 root `-n/--namespace` arg
가 generator 자체가 없는 빈 선언이라 (recognizer 가 잡을 closure 도
없음), 2단 회복으로 격상:

1. **ts-to-json enrichment** (`enrichK8sNamespaces`): k8s 계열 5종
   (kubectl/helm/helmfile/kubecolor/argo) 변환 시 `-n/--namespace`
   옵션의 빈 arg 에 namespaces Tier B template 주입 + root `-n` 에
   `isPersistent` 부여 (global flag 실의미 — upstream 이 안 박아둔 것).
2. **엔진 dispatch 수정**: option-arg dispatch 가 current level 옵션만
   탐색하던 latent bug → `find_option_inherited` 로 ancestor persistent
   옵션까지 탐색. 이 버그는 isPersistent 를 쓰는 104개 spec 전체에서
   "subcommand 뒤 persistent 옵션의 arg generator 무시" 로 잠복해 있었다.

**합격 기준**:

- 클러스터 연결 시: 실제 namespace 이름들이 추천으로 표시, prefix 필터 동작
- 클러스터 미연결 시: generator 후보 0 → 빈 응답, 입력 방해 X
- `kubectl get ⎵` (positional) 은 여전히 리소스 *타입* 완성 — 옵션-arg 와 positional 슬롯이 섞이지 않음 (회귀 테스트: `persistent_root_option_arg_generator_runs_mid_chain`)

#### 12단계 — `kubectl describe pod ⎵` (★ 동적 완성 케이스 — v1.3 격상)

| 입력 | 기대 화면 |
|------|----------|
| `kubectl describe pod ` | 현재 컨텍스트의 실제 리소스 목록 (`kubectl_resources` well-known 회복) |

**합격 기준**:

- 클러스터 연결 시: 실제 pod 이름들이 추천으로 표시
- 클러스터 미연결 시: generator 후보 0 → 빈 응답, 입력 방해 X (200ms timeout 이 latency 폭주 차단)
- 4/7/12 모두 같은 실완성 UX — 일관성 검증. 사용자가 12단계까지 오면서 *동적 인자도 되는구나* 를 학습.

---

## 4. 합격 기준 (전체)

M0 산출물 7번 (Go/No-Go) 의 차단 요건:

- 12단계 중 **10단계 이상 통과** 시 GO.
- 단, **0단계 (30초 KPI) + 0.5단계 (실패 path 3건 중 2건) + 4/7/11/12 동적 완성 4건은 모두 통과** 가 별도 차단 요건.
- 통과 = 위 "기대 화면" 과 "합격 기준" 모두 충족.

세부 합격 기준:

| 카테고리 | 기준 |
|---------|------|
| 설치 / 실패 path | 0단계 30초 KPI + 0.5단계 3건 중 2건 합격 |
| latency | 1/2/3/5/6/8/9/10 단계 모두 p95 ≤ 25 ms (generator 단계는 cache hit 기준) |
| 추천 개수 | 1/5/9 단계 각 ≥ 8 (git), ≥ 10 (docker), ≥ 12 (kubectl) |
| footer 설명 | 1/3/5/8/9 단계에서 선택 행의 정확한 설명이 footer 표시 |
| 동적 완성 | 4/7/11/12 단계 모두 실완성 (branch / image / namespace / 리소스 목록) |
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
- 동적 완성 단계는 노란색 강조: `[4/12] git checkout <Tab> — 실제 branch 완성`
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
- ❌ 동적 완성 자리에 *잘못된* 추천 (예: 다른 repo 의 branch — cwd-aware 위반)
- ❌ generator 실패/timeout 이 에러 텍스트를 화면에 누출
- ❌ footer 설명이 선택 행과 어긋남 (off-by-one)
- ❌ Tab 이 일반 zsh 자동완성으로 fallback (Nerv hook 우선순위 실패)
- ❌ Esc 가 팝업을 닫지 않고 다른 동작 트리거
- ❌ 30초 KPI 초과 (M1 CI 가 측정)
- ❌ 추천 텍스트에 ANSI 시퀀스 가 누출되어 보임

---

## 8. 동적 완성 회복 현황 (v1.3 갱신)

당초 "v1.1 rquickjs 로 동적 4단계 회복" 계획이었으나, 실제 회복은 두
갈래로 진행됐고 결과가 갈렸다:

- **Rust-native 회복 (출하됨)**: Tier B template generator 직접 spawn +
  well-known 패턴 recognizer (kubectl_resources / package_json_scripts /
  aws_list / filepaths 등). 4/7/12 단계가 이 경로로 실완성 격상 — JS
  엔진 없이.
- **rquickjs Tier C (출하됨 — 2026-07-08 결정 번복)**: 2026-06-06 첫
  e2e 는 실행률 0% 였으나 (async 未drain + `__awaiter` 미정의 + shell
  stub), 2026-07-07 머신러리 3종 수정으로 **실행률 82% settle / aws
  746/749 회복**. 릴리즈 빌드에 동봉 (`release.yml`
  `--features nerv-cli/quickjs,nerv-daemon/quickjs`, +0.78MB; JS 실측
  2-3ms < 25ms 예산). 잔존 tail (cargo closure / chezmoi / nx 등
  module-level 헬퍼 미포착) 만 defer — `docs/findings/tier-c-quickjs-e2e.md`
  §2026-07-08 참조.

잔존 closure-form generator 의 회복 경로는 여전히 **recognizer /
enrichment 패턴 추가가 1순위** (Tier C 는 마지막 fallback — CLAUDE.md
§4 JS 엔진 불변식) — 11단계가 그 증명 (generator 가 아예 없던
upstream gap 을 enrichment 로 메움).

M1 figterm opt-in (`NERV_PTY=1`) 도입 시 본 시나리오는 *figterm
환경에서도 동일하게 통과* 해야 한다 (M1 dogfooding 검증).

---

## 9. 변경 트리거

- 50개 spec 후보 풀에서 git/docker/kubectl 중 하나가 빠지는 경우 → 시나리오 재작성
- 30초 KPI 변경 (예: 20초로 강화)
- rquickjs 재개 조건 충족 (findings 문서 §Root causes) → §8 재평가

---

*문서 v1.1 — PLAN.md v0.5 §10 M0-7 의 정밀 명세. v1.0 → v1.1 변경: 0.5단계 (설치 실패 path 3건) 신설 — Xcode CLT 미설치 / oh-my-zsh 충돌 / 재설치 멱등 (CEO v0.4 GO 조건 ②). §4 합격 기준에 0.5단계 추가. 본 시나리오 통과가 v1.0 출시의 사용자 검증 차단 요건.*
*v1.2 — PLAN.md v0.6 정합. 시나리오 본문 무변경 (사용자 체감 동일). §8 만 갱신: v1.1 동적 완성 → rquickjs opt-in 명시, M1 figterm path 에서도 동일 통과 요구 추가. 변경 트리거: v1.1 rquickjs 도입 시 본 문서 deprecate, `first-5-min-v1.1.md` 로 이행.*
*v1.3 — 출하 현실 동기화. (1) 동적 힌트 4단계 중 4/7/12 를 실완성으로 격상 (Tier B template spawn + kubectl_resources 회복 — JS 엔진 없이). 11단계만 잔존 gap 으로 명시 + 회복 경로 2종 기록. (2) frecency 출하 반영 (1단계 정렬). (3) `?` 키 도움말 구상 → footer 자동 설명 (Fig style) 실구현 반영. (4) §8 재작성: rquickjs 0% e2e → 출시 미동봉 결정, Rust-native recognizer 가 회복 1순위. §7 안티패턴 동기 갱신.*
*v1.4 — 11단계 회복으로 동적 4단계 전부 격상. ts-to-json `enrichK8sNamespaces` (k8s 5종 — kubectl/helm/helmfile/kubecolor/argo — 의 빈 namespace arg 에 Tier B template 주입 + root `-n` isPersistent 부여) + 엔진 option-arg dispatch 의 inherited-lookup 수정 (persistent ancestor 옵션의 arg generator 가 subcommand 뒤에서 무시되던 latent bug — isPersistent 104 spec 전체 영향). 회귀 테스트 `persistent_root_option_arg_generator_runs_mid_chain`.*
