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

> **M1 4주차 체크포인트 ✅ PASS (2026-06-07)** — 4항목 전부 green: 50-spec 시나리오 54/54 (`crates/nerv-engine/tests/scenario_specs.rs`) / latency p95 **0.055 ms** (<25 ms) / tmux+2터미널 회귀 (`scripts/e2e-tmux-2term.sh`, cwd 격리 + 20-way 동시성) / uninstall 흔적 0. 상세 = PLAN §10 4주차 체크포인트.

**M0 흡수 스파이크 (v0.6 재정의)** — 산출물 8개 중 7개 완료:

- ✅ M0-9 (v0.5 산출물): `withfig/autocomplete` subtree pin (`aef52acf…`, 1,484 TS spec, ISC)
- ✅ cargo workspace 스캐폴딩 (16 active crates, 503 workspace test 통과)
- ✅ NOTICE / LICENSE / `.github/workflows/{ci,upstream-monitor}.yml`
- ✅ M0-1: `vendor/aws-autocomplete/` subtree add + NOTICE Apache+MIT
- ✅ M0-2: `git filter-repo` 로 10개 crate 추출 → `crates/nerv-{pty,term,ipc,proto,integrations,util,settings,os,log,diag}/`. chunk 3d 완료로 nerv-pty 도 workspace 합류 (런타임 opt-in 은 NERV_PTY=1, M1)
- ✅ M0-3: Rust edition 2024 bump (workspace + 모든 crate + rust-toolchain.toml)
- ✅ M0-4: `shell-parser/parser.ts` (20 KB) → `nerv-engine::shell_parser` Rust 포팅 (124 test)
- ✅ M0-5: `parseArguments.ts` → `nerv-engine::spec_parser` Rust 포팅 (chunks 1-5, 174 test) — types + static helpers + state machine + token classifier + matcher
- ✅ M0-6: `loadSpec.ts` → `nerv-engine::spec_loader` + JSON 직렬화 + `build-specs` 바이너리 + `nerv-engine::complete` 파이프라인 + daemon wire-up. TS→JSON 변환 자체는 M1 (또는 외부 node 스크립트). hand-rolled fixture (git, echo, docker, kubectl) + 24 integration test 통과
- ✅ M0-7: ZLE → CLI → UDS → 실엔진 wire-up + latency bench. IPC p95 0.052 ms, CLI cold-start p95 4.07 ms (25 ms 예산 대비 16%). `_nerv.zsh` widget 포맷 호환 확인
- ⏳ M0-8: Apple Developer ID 서명/공증 빈 바이너리 e2e (**No-Go 차단 요건** — 인프라 의존)

**보너스 진척 (M0 산출물 외 — M1 0-10주차 작업 대부분 선행 완료)**:
- ✅ CLI 5/5 표면 완성: `nerv init` / `start` / `stop` / `spec list` / `doctor` / `uninstall` (uninstall-spec.md §4 8-step atomic 포함)
- ✅ nerv-engine::complete cursor-context override 2종: flag prefix → options 우선, word prefix + node has subs → subcommands 우선 (`git ` 같은 root with positional fallback 처리)
- ✅ fixture pack 9종 hand-rolled (git/echo/docker/kubectl/npm/cargo/gh/brew/make) + 43 integration test
- ✅ TS→JSON 변환 파이프라인 `tools/ts-to-json/` (bun 기반, 715 spec 변환, 0 failure)
  - Tier A 440 / B 6 / C 246 자동 분류
  - **loadSpec depth=2 활성**: `aws ec2 <verb>`, `aws s3 <verb>`, `gcloud compute instances <verb>` 등 nested 자동완성 동작. depth=1 → 2 bump 측정: plain 180MB 무변동 (vendor source 거의 flat — aws/gcloud 서브디렉터리 0 loadSpec, 깊이 체인은 dotnet/pnpx 일부만), gzip cache 10MB → 11MB (+10%). 이전 "depth=4 → 100MB" 경고는 미회복 + 비압축 빌드 기준.
  - cycle-safe (visited Set + MAX_DEPTH gate)
- ✅ **SpecRegistry lazy load + 이벤트/mtime hybrid hot-reload**: at_dir → 디스크 접근은 lookup() 시점. 715 spec 캐시 환경에서도 daemon 즉시 기동. macOS FSEvents (`notify` crate) 가 spec dir 변경 push → pending invalidations 세트에 stem 등록 → lookup() 가 drain + cache evict. mtime check 는 belt-and-suspenders (watcher 실패 / 이벤트 누락 시 fallback). spec 재설치 시 daemon 재시작 불필요 + 이벤트 latency 거의 0.
- ✅ **gzip 압축 cache** (`flate2`): `*.json.gz` 자동 감지 + decompress. 45MB→4.5MB plain, 176MB→10MB at depth=1 (10×). `build-specs --compress` 플래그.
- ✅ **Tier B generator 실행**: 정적 shell command (예: `git branch --list`) → Rust 가 직접 spawn (200ms timeout) + TTL 5s LRU 64 cache (keystroke 마다 spawn 방지) + ANSI/git-marker line sanitization. Tier C (closure) 는 deno_core 비목표 정책 + closure JSON 직렬화 불가로 영구 defer.
- ✅ **well-known Tier C → B 회복** (signature-based recognizer 5종):
  - `Generator::PackageJsonScripts` — npm/yarn/pnpm/bun/rushx/nr 6 spec. walk-up + JSON 파싱 + scripts 키.
  - `Generator::Filepaths { folders_only }` — cd/cat/ls/59 spec. `ls -1ApL` closure 시그니처 감지. cwd-aware + dotfile 제외.
  - `Generator::ZoxideQuery` — z/zoxide 2 spec. zoxide 우선, `~/.z` 폴백 (zsh-z 포맷). fuzzy substring (path+name).
  - **aggressive script-fn 회복** — function-form script 를 stub context 로 호출 → returns string[] 면 Template 으로 강제. **kubectl 27 / docker 117 / docker-compose 23 / gh 23 / aws 1006 generator** 회복.
  - **JSON output 자동 추출** — `gh --json=…` / `kubectl get -o json` 등 JSON 결과를 array-of-objects 로 읽고 `name/number/id/title/key/metadata.name` 우선 필드 추출.
  - **`Generator::ScriptWithJsonPath`** — aws `postPrecessGenerator(out, parentKey, idField)` 패턴 인식. ts-to-json 이 postProcess source 에서 정규식으로 (parent_key, id_field) 캡처 → Rust 가 script spawn + JSON.parse + path 추출. 591 aws generator 회복 (iam list-users / ec2 describe-instances 등). closure body 자체 실행 없이 데이터로 우회.
  - **`Generator::AwsList`** — aws `listCustomGenerator(tokens, exec, verb, options, parentKey, childKey)` 패턴 인식. token-aware. ts-to-json 이 파일경로에서 service 추출 + custom: closure source 에서 (verb, lookup_flags, parent_key, id_field) 캡처. Rust 가 런타임에 tokens 에서 flag 값 찾아 `aws <service> <verb> [<flag> <val>]*` 실행 + JSON 추출. 89 generator 회복 (cloudwatch list-metrics / lambda list-layer-versions 등). string-form / array-form 양쪽 지원.
- ✅ **yarn-shorthand**: root args generator 를 subcommand emit 에 머지. `yarn web<Tab>` → `web:start/web:build:dev/...` (package.json scripts) + `yarn add` 같은 실제 subcommand 도 유지. additive merge + dedupe.
- ✅ **cwd-aware IPC**: `Request::Complete.cwd: Option<String>` 추가. CLI bridge 가 `std::env::current_dir()` 채움. 데몬은 자기 cwd 대신 클라이언트 cwd 사용. `cd` 마다 daemon 재시작 불필요.
- ✅ **UTF-8 char boundary 클램프**: `cursor` 가 multibyte (한글/CJK/emoji) 중간에 떨어질 때 `clamp_cursor_to_char_boundary` 로 직전 boundary 까지 감소. `'ㅊㅇ .'` 입력 시 패닉 → empty 응답.
- ✅ **inline ghost text**: top 제안 trailing 부분을 `POSTDISPLAY` 에 dim grey 로 표시. Right-arrow (line 끝일 때만) 로 accept. LBUFFER 끝이 공백이거나 prefix 비면 ghost off — "토큰 타이핑 중" 시그널 일치.
- ✅ **frecency ranking**: per-spec usage TSV (`~/Library/Caches/nerv/frecency.tsv`). 데몬이 `Request::RecordAccept` 받아 in-memory + opportunistic flush. score = `(count-1) / (1+age_days)` — single pick = no boost, 2+ picks 부터. daemon post-sort 가 alpha 결과를 boost-first 로 재정렬. `NERV_FRECENCY_FILE=-` 로 테스트 격리.
- ✅ 에러 UX shell-side: E1 widget hint, E2 doctor table, E3 zsh<5.8 check, E4 widget conflict 감지, **E5 spec schema mismatch**
- ✅ **E5 spec schema 버전 게이트** (error-states §3.5): `nerv-engine::manifest` (`SUPPORTED_SCHEMA_VERSION=2` + `check_schema(dir) -> {Ok/Missing/Mismatch}`). build-specs 가 `manifest.json` (schema_version) 작성 → daemon 부팅 시 비교, mismatch면 `error!` 로그 + Complete 전체 `Empty{reason}` 차단 (missing=관대, 구버전 호환). CLI bridge 가 schema reason 감지 → exit 3 → `_nerv.zsh` E5 회색 1줄 (`__NERV_E5_SHOWN`). doctor red row. 5 manifest unit + 1 daemon e2e (`schema_mismatch_disables_completion`).
- ✅ **widget UX**: sliding window (`MAX_VIS = min(LINES-6, 10)`), footer 카운터 `[k/total]` 항상 표시, 우측 border 정렬 (off-by-2 fix), Tab/Shift-Tab/Arrow 모두 wrap-cycle, precmd 에서 self-insert/accept-line/backward-delete/space 재바인딩 (Q/oh-my-zsh/fzf-tab hijack 방지), description 매행 → footer 단일 라인 (Fig style).
- ✅ SIGPIPE → SIG_DFL: `nerv spec list | head` panic 제거
- ✅ **CI 확장**: rust 1.85 핀 + `brew install protobuf` (nerv-proto build.rs 회피) + `build-specs-smoke` (plain+gzip vs 9 fixture) + `ts-to-json` (bun convert:one + JSON sanity) job. **ARM64-only** 매트릭스 (macos-13 queue 너무 길어서 drop).
- ✅ **release.yml 워크플로**: `v*.*.*` tag push → macos-14 빌드 + tarball (sha256) + GitHub Release 생성. `gh release create … --clobber` 로 idempotent.
- ✅ **e2e-isolated.sh 스모크 하네스**: `scripts/e2e-isolated.sh` 가 격리 `/tmp/nerv-test/.zshrc` 로 새 zsh 진입. Q/oh-my-zsh 등 영향 zero. 매번 무조건 rebuild + spec install + daemon 재시작.
- ✅ **Fig parity batch (alpha.7 후보)**: 6개 스키마 필드 + 엔진/위젯 wire-up. 715 spec 변환 시 자동 추출 + 707 loaded. 필드별 침투율:
  - `isPersistent` (104 spec) — option 상속, find_option_inherited 가 ancestor chain 까지 탐색
  - `priority` (77 spec) — `sort_by_priority_then_alpha` 가 emit 전체에서 균일 적용 (default 50)
  - `requiresSeparator` (67 spec) — `--color=` 강제, 위젯 insertion 에 `=` 첨가, parser 가 space-form 의 arg 바인딩 거부
  - `icon` (59 spec) — sanitize_icon 으로 `fig://*` URL strip + ≤4 byte + **non-ASCII 는 unicode-width width==2 강제** (Latin-extended `à` / ambiguous-width `⚠` 거부 — 1-cell 밀림 방지). 4-field wire format 로 위젯에 전달
  - `flagsArePosixNoncompliant` (41 spec) — go/docker/kubectl 스타일 `-foo` 를 long option 으로 라우팅
  - `filterStrategy` (27 spec) — `"substring"` 지원, `"fuzzy"` 는 mode=Prefix 시 prefix downgrade / mode=Fuzzy 시 서브시퀀스
  - `getQueryTerm` (0 spec but infra ready) — `cargo search "tokio,serde"` 같은 delim split. 현재 Fig spec 은 closure form 만 쓰지만 M1 회복 시 사용 예정
- ✅ **smart description fallback**: cd/z 같은 folder-only emit 의 footer 가 모두 "dir" 이던 문제. `dir_summary` 가 read_dir 1회로 `n items` / `empty` / `1 item` 출력. dotfile 제외, 200 entries cap (latency bound). 50µs/dir 추정.
- ✅ **fuzzy matching opt-in** (M1): `~/.config/nerv/nerv.toml` 의 `[matching] mode = "fuzzy"` 로 활성화. case-insensitive 서브시퀀스 (`git chk` → `checkout`). 데몬 부팅 시 1회 로드 (재시작 필요). per-arg `filterStrategy: "substring"` 은 mode 무관 우선. 서브커맨드/옵션/제너레이터 출력 전부 동일하게 게이트. 매칭 알고리즘: `nerv-engine::complete::matches_filter` + `matches_name`.
- ✅ **Homebrew tap 인프라**: `packaging/homebrew/nerv.rb` Formula 템플릿 (ARM-only `aarch64-apple-darwin`, `brew services` 통합). `.github/workflows/homebrew-bump.yml` 가 GitHub release 발행 시 자동으로 `nerv-sh/homebrew-tap` 의 Formula 를 버전+sha256 갱신. 사용자 액션: tap repo 생성 + `HOMEBREW_TAP_TOKEN` PAT secret 추가.
- ✅ **icon glyph width contract**: `sanitize_icon` 이 `unicode-width` 로 non-ASCII glyph display width == 2 강제 (이전 ≤4 byte gate 만으로는 ambiguous-width `⚠` / Latin-extended `à` 통과 → 1-cell 밀림). ASCII = width 1, non-ASCII = width 2 외 거부. 위젯 측은 "non-ASCII = 2 cells" 가정 그대로 유지 — contract 가 엔진에서 보장.
- ✅ **흡수 crate 브랜드 strip (active surface)**:
  - **env_var module 리네이밍**: `nerv_util::consts::env_var` 의 13 const + ad-hoc figterm-contract 6 string (Q_IS_LOGIN_SHELL / Q_EXECUTION_STRING / Q_SHELL_EXTRA_ARGS / Q_START_TEXT / Q_TERM_TMUX / Q_DISABLE_AUTOCOMPLETE) + private static (nerv-log) Q_* → NERV_*. QTERM_SESSION_ID → NERV_PTY_SESSION_ID, PROCESS_LAUNCHED_BY_Q → PROCESS_LAUNCHED_BY_NERV.
  - **nerv-os::Env 정리**: dead Q_*/AMAZON_Q_* 접근자 14개 삭제 (telemetry / inline shell completion / desktop release URL / sigv4 / custom certs / sendmessage workaround / Q_CODESPACES·Q_CI 오버라이드). 생존 6개 메서드 (q_log_level → nerv_log_level 등) 리네이밍 + 5 callsite (nerv-log / nerv-pty / nerv-term / nerv-util) 동기 갱신. env.rs -54 LOC.
  - **CLI / PRODUCT 리네이밍**: CLI_BINARY_NAME `q` → `nerv`, PTY_BINARY_NAME `qterm` → `nerv-pty`, CLI_CRATE_NAME `q_cli` → `nerv-cli`, URL_SCHEMA `q` → `nerv`, PRODUCT_NAME `Amazon Q` → `Nerv`, GITHUB_REPO_NAME `aws/amazon-q-developer-cli` → `nerv-sh/nerv`. `# {PRODUCT_NAME} pre block` 마커가 자동으로 `# Nerv pre block` 으로 흐름 → CLAUDE.md §4 invariant (no `# Q pre block` 잔존) 충족.
  - **dead 산출물 삭제**: CHAT_BINARY_NAME 와 nerv-log 의 qchat mcp.log dead branch (35 LOC + LogGuard._mcp_file_guard 필드), CLI_BINARY_NAME_MINIMAL, OLD_PRODUCT_NAME / OLD_CLI_BINARY_NAMES / OLD_PTY_BINARY_NAMES (CodeWhisperer legacy), consts::url 모듈 (docs.aws.amazon.com 6 URL, 0 consumer).
  - **bundle 식별자 예약**: APP_BUNDLE_ID `com.amazon.codewhisperer` → `sh.nerv.nerv`, APP_BUNDLE_NAME `Amazon Q.app` → `Nerv.app`. launchd_plist 테스트 + insta snapshot 동기 갱신. Apple Developer 서명 단계 (M0-8) 가 단순 codesign 으로 떨어짐.
  - **settings key 리네이밍 + AI 제거**: `qterm.csi-u.enabled` → `pty.csi-u.enabled`, `qterm.enabled` → `pty.enabled`. CLAUDE.md §4 invariant (no AI) 에 따라 AI translate intercept (`#foo<Enter>` → `q translate 'foo'`) ~23 LOC + `ai.terminal-hash-sub` 설정 + `ai_enabled` 플래그 제거. on-disk migration 없음 — nerv-pty 는 M1 opt-in 으로 live user 없음.
- ✅ **CI workflow_dispatch gate (2026-06-01 reset 까지)**: 1차 무료 Actions budget 90% (1,806 / 2,000 min) 도달. `ci.yml` 만 `on: workflow_dispatch:` 로 축소 (push/PR 트리거 정지). 다른 workflow 는 그대로 (release/homebrew-bump=tag/release event, upstream-monitor=cron 1·15일). restore 는 inline comment 한 줄 reflow.
- ✅ **Tier C executor 배선** (`feature = "quickjs"` opt-in): `nerv-quickjs` 스캐폴드 (rquickjs ~1MB sandbox, `eval_isolated` + `eval_with_budget` 200ms 기본) → `nerv-engine::tier_c::execute_custom_source` 헬퍼 → `complete.rs` 의 `Generator::Custom { source: Some(_), .. }` arm wire-up. ts-to-json 의 `captureClosureSource` 가 closure `toString()` 을 IIFE 형태로 감싸 (`(<fn>)(globalThis.__nerv_tokens, () => Promise.resolve(""))`) 32KB cap 적용해 emit. 715 spec 변환 → **473 closure source 캡처** (74 파일 분산, 100% capture rate). ⚠️ **capture ≠ execution**: 2026-06-06 e2e 검증 결과 473개 전부 sandbox 에서 0 candidate (None) — async 未await 76% + `__awaiter` 미정의 21% + shell stub. **실행률 0%**, quickjs 경로 현재 비작동. 상세 = `docs/findings/tier-c-quickjs-e2e.md`. 기본 빌드는 `nerv-quickjs` dep 0 (`cargo tree -p nerv-cli` / `nerv-daemon` 검증) — opt-in 만 binary 변동. Custom arm 은 well-known 회복 (aws_list 89 / kubectl_resources 86 / package_json_scripts 28 / ssh_hosts 9 / 외) 통과 후 마지막 fallback 으로만 동작. Soft-fail: tier_c None → next generator → smart filepaths fallback. 5 dispatch test + 1 default-build 호환 test.
- ✅ **figterm PTY shim Phase 1+2 배선**: `NERV_PTY=1` 환경변수 opt-in. `cmd_init(--shell-script)` 가 env 감지 → ZLE 위젯 (`_nerv.zsh`) 대신 PTY 부트스트랩 (`_nerv-pty.zsh`) emit + `NERV_PTY_BIN={absolute path}` 자동 export (sibling lookup). `_nerv.zsh` 자체도 top-level self-skip (벨트+서스펜더). 부트스트랩 = re-entry guard (`NERV_PTY_SESSION_ID`) + TTY check (CI/pipe 무시) + PATH fallback → `exec nerv-pty -- "$SHELL"`. nerv-pty 바이너리 (6.69MB release, nerv-engine dep +0.09MB) release tarball + Homebrew Formula 동봉 (idle until opt-in). 3 dispatch tests (`init_snippet_for_zsh_default_is_zle_widget` / `init_snippet_for_zsh_pty_mode_is_pty_bootstrap` / `resolve_pty_bin_for_init_finds_sibling`).
- ✅ **figterm PTY shim Phase 3a+3b 완료** (인라인 자동완성 작동): 흡수된 figterm 머신(PTY spawn / shadow term / interceptor)은 이미 LIVE 였고, completion 경로를 ~~desktop app(Hostbound, nerv-sh엔 없음)~~ → **nervd UDS 재배선**.
  - **3a ghost**: `nerv-pty::engine_client` 가 `Request::Complete{line,cursor,cwd}` 재사용(M0 ZLE/CLI bridge 와 동일 계약, 50ms timeout) → top suggestion trailing remainder 를 `nerv-pty::ghost` 가 dim faint 로 prompt 줄에 렌더 (DECSC/DECRC + faint SGR + erase-EOL, §4 ANSI whitelist 준수). Right-arrow(EOL)로 accept → shell stdin 주입.
  - **3a.2 inner-shell 697 마커**: `_nerv-pty.zsh` 의 inner-shell 분기(NERV_PTY_SESSION_ID set)가 즉시 return 하던 것 → precmd/preexec hook + PS1 wrap 으로 OSC 697 (StartPrompt/EndPrompt/NewCmd=`$NERV_PTY_SESSION_ID`/Shell=zsh/Dir/PreExec) emit. 이게 없으면 shadow term 이 edit buffer 못 봄(=ghost 안 뜸). upstream figterm 정렬 + brand-strip.
  - **3b popup**: `nerv-pty::popup` — ≥2 suggestion 시 prompt 아래 N행 리스트(reverse 선택행 + `[k/total]` footer, M0 MAX_VIS=LINES-6 clamp). `Overlay` state 가 ghost+popup 통합, reserved-rows high-water 로 prompt 안 밀림(`reserve_seq` = newline scroll + 상대 cursor-up). Tab/Down next, Shift-Tab/Up prev (wrap), Right accept, Esc dismiss. ghost = 선택행 mirror.
  - **3b.2 frecency**: PTY accept(Right-arrow)가 `Request::RecordAccept` 발사 (M0 ZLE `nerv _record` 미러). spec=첫 단어, insertion=popup 선택/완성 토큰. `engine_client::record_accept` fire-and-forget. 데몬이 RecordAccept 시 즉시 flush.
  - **0-row 클램프**: nerv-pty 가 0-row/0-col winsize 를 grid 에 넘겨 panic (`nerv-term grid/storage.rs` visible-lines assertion) 하던 것 → open-pty + resize 경로 모두 rows/cols `.max(1)` 클램프. 0×0 pty no-panic 검증.
  - **ZLE 팝업 컬럼 정렬** (`_nerv.zsh`): 박스를 `ESC[G`(컬럼1) 고정으로 그리던 것 → 프롬프트 폭+입력 길이로 커서 컬럼 계산해 `ESC[<col>G` 정렬 + 화면 우측 클램프. 긴 프롬프트서 박스가 좌측에 동떨어지던 문제 해결. DSR 미사용 (ZLE 위젯서 raw read 가 line editor 와 stdin 경쟁).
  - **e2e**: `scripts/e2e-pty-ghost.py` (PTY: `git che`→ghost "ckout" → Right-arrow accept→frecency.tsv 기록 → `git c`→popup `[1/2]` reverse → Tab→`[2/2]`) + `scripts/e2e-zle-popup.py` (ZLE: 긴 프롬프트서 팝업이 컬럼 ~71 정렬 확인). 반복 PASS. +17 unit test (ghost 8 / popup 9).
  - **3b.3 박스 chrome**: PTY popup 도 M0 ZLE 와 동일한 rounded box (╭─╮ top / `│ … │` rows, 선택행 reverse / ├─┤ divider / `[k/total]` footer / ╰─╯ bottom). `unicode-width` 로 컬럼 폭 계산 (CJK/wide glyph 우측 border 정렬), 폭은 터미널 cols 로 cap. rows()=visible+4.
- ✅ **테스트 커버리지 sweep**: 583 → 637 (+54 across 11 crates). log 1→4 / diag 1→7 / cli 4→19 (parse_zsh_version edges + count_tree + upgrade_tier + uninstall path + init snippet pick) / ipc 10→16 (BufferedReader / error variants / is_disconnect arms) / proto 10→13 (NotificationType wire-format + FigResult arms) / daemon 3→8 (invalid JSON / DoctorAutorun / unknown bin / pipeline / RecordAccept frecency) / integrations 13→22 (backup_file / Error display) / util 37→44 (Error display + UnknownDesktopErrContext + partitioned_compare edges + gen_hex_string charset) / pty 25→32 (ReadBuffer pure).
- ✅ **tech-debt sweep**: orphan `crates/nerv-util/src/error.rs` 삭제 (Error enum 인라인 중복, 0 consumer); `parse_zsh_version_compat` 중복 fn 삭제 (byte-identical); 8 dead `directories.rs` 함수 삭제 (chat/midway/AppImage = CLAUDE.md §4 비목표 + Linux M2+ — git history 에서 복구 가능); RwLock poison silent swallow 정책 doc-comment (graceful degradation = 의도, tracing dep 미추가). 총 -189 LOC.
- ✅ **`spawn_with_timeout` 헬퍼 추출**: `execute_template_generator` + `cached_cargo_metadata` 의 18-line spawn-drain-timeout 블록 dedup. `CARGO_METADATA_CACHE` 에 GENERATOR_CACHE 와 동일 LRU policy (max 64) 추가 (이전 unbounded leak — cd 마다 entry 누적).

**폐기된 v0.5 산출물**: M0-2 자작 transpile, `build/spec-transpile/` (loadSpec 포팅이 대체).

**진행중 옵션**:
- M0-8: 서명/공증 (Apple Developer 계정 + 인프라 필요)
- aws 624 script-fn 회복 (closure 가 token 에 의존 → rquickjs M1 필요. closure body 자체는 직렬화 가능. 단 deno_core 금지)
- bash / fish 지원 (큼)
- Linux / Windows 지원 (큼)
- figterm PTY shim opt-in (`NERV_PTY=1`) — **Phase 1+2+3a+3b 완료** (위 §3 참조). 인라인 ghost + popup(박스 chrome) + 네비 + frecency 작동, nervd UDS 재배선, 0-row 클램프, ZLE 팝업 컬럼 정렬, e2e PASS. Phase 3 follow-up 전부 완료.
- ✅ E5 manifest 완료 (위 §3 — schema 버전 게이트 + doctor red + ZLE 회색 1줄)
- 브랜드 strip 잔여 (defer): RUNTIME_DIR_NAME / DATA_DIR_NAME / Linux package name / desktop entry 일부는 후속 PR 에서 정리
- aws 624 closure-form generators 중 89 = `aws_list` 회복, 나머지 = `Generator::Custom { source }` 로 캡처됨 → `--features quickjs` 빌드에서 실행. 기본 빌드는 여전히 skip. **출시 결정 확정 (2026-06-07)**: Tier C 실행률 e2e = **0%** (473 캡처 / 0 실행) → `--features quickjs` scaffold 는 유지하되 **production 기본 OFF, 출시 바이너리 미동봉** (opt-in 만). PLAN §0.2 JS generator 행 + `docs/findings/tier-c-quickjs-e2e.md` 갱신. 재개 조건 = async Promise drain + `__awaiter`/shell host-global 주입 (finding §Root causes).

## 4. 절대 깨면 안 되는 불변식

코드 / 인프라 변경 시 다음을 어긋나면 즉시 차단:

| 영역 | 불변식 | 근거 |
|------|--------|------|
| 매칭 알고리즘 | **기본 prefix** — `git co` ≠ `checkout` (`c-h-` 시작). **fuzzy 는 opt-in** (`~/.config/nerv/nerv.toml` 의 `[matching] mode = "fuzzy"`). fuzzy 활성 시 case-insensitive 서브시퀀스. 데몬 부팅 시 1회 로드 — config 변경엔 재시작 필요 | PLAN §5.1 / `nerv-engine/src/config.rs` / `nerv-engine/src/complete.rs::matches_filter` |
| 매칭 알고리즘 | 빈 prefix 는 모두 매치 (`git ⎵` 케이스) | first-5-min §1단계 |
| 마커 블록 | `# >>> nerv >>>` ~ `# <<< nerv <<<` 는 **고정 문자열**. `fig_integrations` 흡수 시 marker 교체 필수 (Q 의 `# Fig pre block` 잔존 금지) | uninstall-spec §3 / `nerv-shell::MARKER_*` |
| 경로 | `~/Library/Caches/nerv/`, `~/Library/Logs/nerv/`, `~/.config/nerv/` — `directories` 크레이트 사용 X (docs 가 contract). `fig_util` / `fig_log` / `fig_settings` 흡수 시 Q 기본 경로 (`~/.config/q/`, `~/Library/Caches/amzn/`) 전부 nerv 경로로 재배선 | uninstall-spec §2 / `nerv-engine/src/paths.rs` |
| ANSI | alternate screen 진입 X, true color X, OSC 8/52 X — raw cursor save/restore + line clearing 만. **`fig_desktop` webview UI 흡수 금지** | terminal-compat §3 / §6 |
| Popup 렌더 | `zle -R "" "${plain[@]}"` 로 공간 reserve + `printf '\e7…\e8'` 로 colored overlay. **`zle -R "" "${colored[@]}"` 금지** (zle 가 ANSI escape 해석 X — literal `^[[…m` 출력). MAX_VIS 은 `LINES-6` 으로 clamp (popup 화면 넘어가면 save/restore 깨짐) | `shell-integrations/zsh/_nerv.zsh` |
| 에러 톤 | `[nerv] <문제> — <조치>` 영문 한 줄, 사과/완곡어구 금지, 회색만 사용 | error-states §4 |
| CLI 표면 | v1.0 명령은 5개 (`init / doctor / start / stop / spec list / uninstall`) — 추가 금지. `q_cli` 흡수 금지 (chat/login/translate 잔존 금지) | PLAN §9 |
| 비목표 | AI / 텔레메트리 / 자체업데이트 / `nerv config` / `spec list --changes` 코드 자체를 두지 않음. **흡수 시 `fig_api_client`/`fig_auth`/`fig_telemetry*`/`amzn-*`/`semantic_search_client` 의존 0** | PLAN §4 비목표 |
| JS 엔진 | **deno_core 임베드 금지**. Tier C 회복은 M1 = `rquickjs` (~1 MB) opt-in 만. **well-known 패턴은 Rust-native 회복 OK** (예: `Generator::PackageJsonScripts` — npm scripts 추출은 closure 우회) | PLAN §0.2 / §7 |
| IPC cwd | `Request::Complete.cwd` 는 **클라이언트(쉘) 의 CWD**. 데몬 process cwd 사용 금지 (`std::env::current_dir()` 데몬 측 호출은 fallback 만). CLI bridge 가 채워야 함 | `nerv-engine/src/ipc.rs::Request::Complete` |
| UTF-8 | 멀티바이트 입력 (한글/CJK/emoji) 으로 `cursor` 가 char 중간에 떨어질 수 있음 → **`clamp_cursor_to_char_boundary` 필수**. `&line[..cursor]` 직접 슬라이스 금지 | `nerv-engine/src/complete.rs` |
| PTY shim | `figterm` (`nerv-pty`) 는 **M1 opt-in only**. M0 ZLE widget 과 상호 배타. `NERV_PTY=1` 환경변수로 분기 | PLAN §5.8 / §6.2 |
| Rust | toolchain 1.85, **edition 2024** (upstream 정합). v0.5.1 의 edition 2021 폐기. 변경 시 PLAN §7 + `rust-toolchain.toml` + 본 §4 동시 갱신 | `rust-toolchain.toml` |
| vendor 편집 | `vendor/withfig-autocomplete/` 와 `vendor/aws-autocomplete/` **양쪽 모두 직접 편집 금지**. 변경은 `vendor-patches/{upstream,self}/` 또는 upstream PR | spec-conversion-policy §5.2 |
| icon sanitize | `Suggestion.icon` 은 절대 `fig://*` URL 통과 금지 — `sanitize_icon` 으로 strip. ≤4 byte + **non-ASCII 는 `unicode-width` width==2 강제** (Latin-extended `à` / ambiguous-width `⚠` 거부 — 1-cell 밀림 방지). ASCII 는 단일 1글자만. 위젯이 non-ASCII = 2 cells 가정하고 row 정렬하므로 엔진이 contract 를 보장해야 함 | `nerv-engine/src/complete.rs::sanitize_icon` |
| 흡수 crate 브랜드 | 흡수 crate 의 Q_*/Amazon Q/qterm/qchat 식별자는 모두 NERV_*/Nerv/nerv-pty 로 재배선 후 활성화. `# Q pre block` / `# Fig pre block` 마커 잔존 금지 — `PRODUCT_NAME = "Nerv"` 흐름으로 자동 reflow. dead Q_* 접근자 / AI/translate hook / qchat mcp.log branch 는 흡수 시 직접 strip. `vendor/aws-autocomplete/` 미수정 mirror 는 그대로 — strip 은 `crates/nerv-*` 사본 측에서만 | `crates/nerv-util/src/consts.rs` / `crates/nerv-os/src/env.rs` |
| filterStrategy 우선 | per-arg `filterStrategy: "substring"` 은 user `MatchMode` (Prefix/Fuzzy) 무관 우선. spec author 가 명시한 의도를 user mode 가 덮어쓰지 않음. `"fuzzy"` 값 자체는 user mode 와 동일 결과 (Prefix→prefix / Fuzzy→subsequence) | `nerv-engine/src/complete.rs::matches_filter` 첫 분기 |
| 4-field wire format | `nerv _complete` 출력은 `insertion\tdisplay\tdescription\ticon` 4-tab. icon 비면 빈 문자열. **field 추가 시 widget parser 동시 갱신 필수** | `crates/nerv-cli/src/main.rs::print_suggestion` + `_nerv.zsh` |
| parserDirectives 적용 | `flagsArePosixNoncompliant` 는 root spec 의 directive 만 체크 (subcommand chain 상속 X). Go/docker/kubectl 처럼 root 부터 일관된 스타일이 권장 | `nerv-engine/src/spec_parser.rs::ShortOption` arm |
| getQueryTerm 범위 | string form 만 (single-byte delim chars). function form 은 Tier C → M1. delim chars 마지막 위치에서 split, insertion 에 context prefix 보존 | `nerv-engine/src/complete.rs::split_by_query_term` |
| spec schema 버전 (E5) | `SUPPORTED_SCHEMA_VERSION` bump 시 build-specs(manifest 작성) + daemon(게이트) + error-states §3.5 동시 갱신. **manifest 부재 = 관대 (구버전 호환), mismatch 만 차단**. corrupt manifest = missing 취급 | `nerv-engine/src/manifest.rs` |

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

# 격리 e2e 스모크 (Q/oh-my-zsh 등 영향 zero, /tmp/nerv-test ZDOTDIR)
./scripts/e2e-isolated.sh                       # 빌드 + spec install + 데몬 + 격리 zsh
NERV_SKIP_BUILD=1 ./scripts/e2e-isolated.sh    # 빌드 건너뛰기
NERV_SKIP_SPECS=1 ./scripts/e2e-isolated.sh    # spec install 건너뛰기

# Release tag (자동 빌드 + GitHub Release 생성, ARM-only)
git tag -a v0.1.0-alpha.N -m "v0.1.0-alpha.N"
git push origin v0.1.0-alpha.N                 # → release.yml 트리거

# Frecency 디버그 (NERV_FRECENCY_FILE=- 로 테스트 격리)
NERV_FRECENCY_FILE=- cargo test --workspace
cat ~/Library/Caches/nerv/frecency.tsv         # spec\tinsertion\tcount\tunix

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
  nerv-engine/     # 자작 + TS 포팅분 (shell_parser / spec_parser / spec_loader (gzip 자동감지) / complete (lazy registry) / ipc / ipc_client (nervd UDS 클라 단일소스) / manifest (E5 schema 게이트) / paths / ranker)
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
packaging/homebrew/nerv.rb           # Homebrew Formula 템플릿 (auto-bumped on release)
vendor/withfig-autocomplete/         # subtree, ISC, pin = aef52acf… (TS specs 1,484)
vendor/aws-autocomplete/             # M0-1 subtree, Apache+MIT, 미수정 mirror (drift 감지)
vendor-patches/{upstream,self}/      # cherry-pick 보관소 (M1)
docs/                                # 위 §2 6종 + reference/ (TS 포팅 참조본)
docs/archive/PLAN.v0.5.1.md          # 이전 PRD 보존
.github/workflows/{ci,release,upstream-monitor,upstream-prs,homebrew-bump}.yml
                                     # homebrew-bump = release 시 tap Formula 자동 갱신
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
- **M1 4주차** ✅ **PASS (2026-06-07)**: 50개 spec 시나리오 통과 (`scenario_specs.rs` 54/54) / latency p95 0.055 ms (<25 ms) / tmux+2터미널 회귀 (`e2e-tmux-2term.sh`) / uninstall 흔적 0
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
- ❌ fuzzy matching 을 기본 활성 — `MatchMode::Prefix` 가 default. 활성 코드 경로는 `~/.config/nerv/nerv.toml` 의 `[matching] mode = "fuzzy"` opt-in 뿐.
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
