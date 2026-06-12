#!/usr/bin/env bun
/**
 * convert.ts — withfig/autocomplete TypeScript specs → Nerv JSON.
 *
 * Loads each `*.ts` file under --input via Bun's dynamic import,
 * pulls the default export (a Fig.Spec), normalizes it to the Nerv
 * shape (snake_case fields, names as array, args as array, tier
 * classification), and writes one JSON per spec to --output.
 *
 * Tier detection:
 *   A = pure static (no generators, no Custom)
 *   B = generator.script with literal command (preserved as Template)
 *   C = generator.custom or generator.script with postProcess
 *       → recorded as { type: "custom" } / { type: "script", has_post_process: true }
 *       but not actually invoked. M1 rquickjs runs them.
 *
 * Usage:
 *   bun convert.ts --input <file.ts | dir> --output <file.json | dir>
 *
 * Refs: docs/spec-conversion-policy.md, PLAN.md M1 0-4주차
 */

import { readdir, mkdir, stat } from "node:fs/promises";
import { resolve, basename, extname, dirname, join } from "node:path";

/**
 * Detect whether a Fig generator object came from `filepaths()` or
 * `folders()` in `@fig/autocomplete-generators`. Both build a custom
 * async closure that ends up running `ls -1ApL` to list directory
 * entries — that exact flag set is a stable signature across the
 * package's history.
 *
 * Returns:
 *   - { kind: "filepaths", foldersOnly: true } for folders() OR
 *     filepaths({ showFolders: "only" }) (detected by inspecting
 *     the closure source for `"only"`).
 *   - { kind: "filepaths", foldersOnly: false } for the rest.
 *   - null if the generator isn't a filepaths/folders variant.
 *
 * False-positive risk is low: `"-1ApL"` is rare outside this lib.
 */
export const detectFilepathsGenerator = (
  g: any,
): { kind: "filepaths"; foldersOnly: boolean } | null => {
  if (g == null || typeof g !== "object") return null;
  if (typeof g.custom !== "function") return null;
  let src: string;
  try {
    src = g.custom.toString();
  } catch {
    return null;
  }
  if (!src.includes('"-1ApL"') && !src.includes("'-1ApL'")) return null;
  // showFolders gets baked into the closure as a literal string compare
  // in the filter step. The "only" variant is what cd uses.
  const foldersOnly =
    src.includes('"only"') || src.includes("'only'") || src.includes("=== \"only\"");
  return { kind: "filepaths", foldersOnly };
};

/**
 * Detect aws-style `postPrecessGenerator(out, "ParentKey", "IdField")`
 * inside a `postProcess` closure. The helper is locally defined per
 * aws spec file (iam.ts / ec2.ts / iam-roles-anywhere.ts / …) — what
 * stays stable across all of them is the call shape.
 *
 * Returns the captured `parent_key` and `id_field` (the latter is
 * optional in upstream; some calls omit it and emit the array element
 * itself).
 */
export const detectAwsJsonPath = (
  postProcess: any,
): { parent_key: string; id_field: string | null } | null => {
  if (typeof postProcess !== "function") return null;
  let src: string;
  try {
    src = postProcess.toString();
  } catch {
    return null;
  }
  // Match the canonical `postPrecessGenerator(out, "Parent", "Child")`
  // call. Allow single OR double quotes; tolerate stray whitespace.
  const m = src.match(
    /postPrecessGenerator\s*\(\s*\w+\s*,\s*["']([^"']+)["']\s*(?:,\s*["']([^"']+)["'])?\s*\)/,
  );
  if (!m) return null;
  return { parent_key: m[1], id_field: m[2] ?? null };
};

/**
 * Detect aws-style `listCustomGenerator(tokens, exec, "verb", flags,
 * "ParentKey", "IdField")` inside a `custom` closure. Each aws spec
 * file defines its own local helper; what stays stable is the call
 * shape and the surrounding service hint (set by `loadOneAt`).
 *
 * Two `flags` forms are supported:
 *  - array literal `["--name", "--other"]` → captured as-is.
 *  - single string `"--name"` → wrapped into a single-element list.
 */
export const detectAwsListCustom = (
  custom: any,
  serviceHint: string | null = AWS_SERVICE_HINT,
): {
  verb: string;
  lookup_flags: string[];
  parent_key: string;
  id_field: string | null;
} | null => {
  if (typeof custom !== "function" || !serviceHint) return null;
  let src: string;
  try {
    src = custom.toString();
  } catch {
    return null;
  }
  // Match either form. Capture: verb, flags blob (array or single
  // string), parent_key, optional id_field. Multi-line tolerant.
  const m = src.match(
    /listCustomGenerator\s*\(\s*\w+\s*,\s*\w+\s*,\s*["']([^"']+)["']\s*,\s*(\[[\s\S]*?\]|["'][^"']+["'])\s*,\s*["']([^"']+)["']\s*(?:,\s*["']([^"']+)["'])?\s*\)/,
  );
  if (!m) return null;
  const [, verb, flagsBlob, parentKey, idField] = m;
  const flagsTrim = flagsBlob.trim();
  let lookup_flags: string[];
  if (flagsTrim.startsWith("[")) {
    // Array literal — collect every quoted string inside.
    lookup_flags = Array.from(
      flagsTrim.matchAll(/["']([^"']+)["']/g),
      (mm) => mm[1],
    );
  } else {
    // Single-string form (iam.ts variant).
    lookup_flags = [flagsTrim.slice(1, -1)];
  }
  return {
    verb,
    lookup_flags,
    parent_key: parentKey,
    id_field: idField ?? null,
  };
};

type FigArg = {
  name?: string | string[];
  description?: string;
  isOptional?: boolean;
  isVariadic?: boolean;
  suggestions?: Array<string | { name?: string }>;
  template?: string | string[];
  generators?: any | any[];
  getQueryTerm?: string | string[] | ((token: string) => string);
  filterStrategy?: "prefix" | "fuzzy" | "default";
};

type FigOpt = {
  name: string | string[];
  description?: string;
  args?: FigArg | FigArg[];
  exclusiveOn?: string[];
  dependsOn?: string[];
  isRequired?: boolean;
  isRepeatable?: boolean | number;
  hidden?: boolean;
  isPersistent?: boolean;
  priority?: number;
  requiresSeparator?: boolean | string;
  icon?: string;
};

type FigSpec = {
  name?: string | string[];
  description?: string;
  subcommands?: FigSpec[];
  options?: FigOpt[];
  args?: FigArg | FigArg[];
  hidden?: boolean;
  requiresSubcommand?: boolean;
  parserDirectives?: { flagsArePosixNoncompliant?: boolean };
  loadSpec?: any;
  generators?: any;
  icon?: string;
  priority?: number;
};

type NervSuggestion =
  | string
  | {
      name: string;
      description?: string;
      displayName?: string;
      insertValue?: string;
      icon?: string;
      priority?: number;
    };

type NervArg = {
  name: string | null;
  description: string | null;
  is_optional: boolean;
  is_variadic: boolean;
  suggestions: NervSuggestion[];
  template: string | null;
  generators: NervGenerator[];
  getQueryTerm?: string;
  filterStrategy?: string;
};

type NervOpt = {
  names: string[];
  description: string | null;
  args: NervArg[];
  exclusive_on: string[];
  depends_on: string[];
  is_required: boolean;
  is_repeatable: boolean;
  hidden: boolean;
  isPersistent?: boolean;
  priority?: number;
  requiresSeparator?: boolean;
  icon?: string;
};

type NervSpec = {
  name: string;
  aliases: string[];
  description: string | null;
  subcommands: NervSpec[];
  options: NervOpt[];
  args: NervArg[];
  requires_double_dash: boolean;
  hidden: boolean;
  priority?: number;
  icon?: string;
  flagsArePosixNoncompliant?: boolean;
};

type NervGenerator =
  | { type: "template"; script: string[] }
  | { type: "script"; script: string[]; has_post_process: boolean }
  | { type: "custom"; description_hint: string | null; source?: string }
  | { type: "package_json_scripts" }
  | { type: "filepaths"; folders_only: boolean }
  | { type: "zoxide_query" }
  | { type: "ssh_hosts" }
  | { type: "makefile_targets" }
  | { type: "man_pages" }
  | { type: "package_json_deps" }
  | { type: "kubectl_resources" }
  | { type: "cargo_targets"; kind: string | null }
  | {
      type: "script_with_json_path";
      script: string[];
      parent_key: string;
      id_field: string | null;
    }
  | {
      type: "aws_list";
      service: string;
      verb: string;
      lookup_flags: string[];
      parent_key: string;
      id_field: string | null;
    };

/** Normalize a Fig `name` field (string | string[]) into our names array.
 *  Fig sometimes embeds `null` or sparse holes — filter to non-empty strings. */
const namesOf = (n: string | string[] | undefined | null): string[] => {
  if (n == null) return [];
  const list = Array.isArray(n) ? n : [n];
  return list.filter((x): x is string => typeof x === "string" && x.length > 0);
};

/** Normalize `args` (object | array | undefined) into our array form. */
const argsOf = async (a: FigArg | FigArg[] | undefined): Promise<NervArg[]> => {
  if (a == null) return [];
  const list = Array.isArray(a) ? a : [a];
  return await Promise.all(list.map(convertArg));
};

const TEMPLATE_MAP: Record<string, string> = {
  filepaths: "filepaths",
  folders: "folders",
  history: "history",
  help: "help",
};

const convertTemplate = (t: string | string[] | undefined): string | null => {
  if (t == null) return null;
  const candidates = Array.isArray(t) ? t : [t];
  for (const c of candidates) {
    const key = String(c).toLowerCase();
    if (TEMPLATE_MAP[key]) return TEMPLATE_MAP[key];
  }
  return null;
};

const convertGenerators = async (
  g: any | any[] | undefined,
): Promise<NervGenerator[]> => {
  if (g == null) return [];
  const list = Array.isArray(g) ? g : [g];
  const resolved = await Promise.all(list.map(convertOneGenerator));
  return resolved.filter((x): x is NervGenerator => x !== null);
};

// Mock packages payload for cargo's targetGenerator probe — one
// target per kind, prefixed with a sentinel so we can read back
// which kind(s) survived the closure's `target.kind.includes(kind)`
// filter. Used by `convertOneGenerator` to recover the closure-bound
// `kind` parameter that we can't see via `.toString()` source
// inspection.
const CARGO_KIND_PROBE = [
  "lib",
  "bin",
  "example",
  "test",
  "bench",
  "custom-build",
];
const CARGO_PROBE_MARK = "__nervkp_";
const CARGO_MOCK_METADATA = JSON.stringify({
  workspace_root: "",
  packages: [
    {
      name: "__probe",
      source: null,
      targets: CARGO_KIND_PROBE.map((k) => ({
        name: `${CARGO_PROBE_MARK}${k}`,
        src_path: "",
        kind: [k],
      })),
    },
  ],
});

const probeCargoKind = async (g: any): Promise<string | null | "any"> => {
  // Probe the targetGenerator closure with a synthetic cargo metadata
  // payload. The closure filters its `targets` by `kind` when that
  // captured param is set; we run it and check which of our marked
  // targets survives.
  //
  // Returns:
  //   - "lib" | "bin" | "example" | ... when exactly one mark survives
  //   - "any" when all marks survive (no kind filter — captures the
  //     fallthrough branch where the closure passes every target)
  //   - null on failure / unrecognised shape
  try {
    const mockExec = async () => ({
      stdout: CARGO_MOCK_METADATA,
      stderr: "",
      status: 0,
    });
    const out = await g.custom([], mockExec, {
      currentWorkingDirectory: "",
      sshPrefix: "",
      environmentVariables: {},
    });
    if (!Array.isArray(out)) return null;
    const survivors = out
      .map((s: any) => (typeof s?.name === "string" ? s.name : ""))
      .filter((n: string) => n.startsWith(CARGO_PROBE_MARK))
      .map((n: string) => n.slice(CARGO_PROBE_MARK.length));
    if (survivors.length === 1) return survivors[0];
    if (survivors.length === CARGO_KIND_PROBE.length) return "any";
    return null;
  } catch {
    return null;
  }
};

/**
 * Capture a Fig closure as a Tier C source string the Rust engine can
 * feed to `nerv-engine::tier_c::execute_custom_source` under the
 * `quickjs` feature. Wraps the closure in IIFE form so the eval
 * resolves to the closure's return value with `tokens` bound from the
 * sandbox global. `exec` is stubbed because the sandbox refuses host
 * bindings — closures that call it will throw at runtime and the
 * engine drops to "no candidates" silently.
 *
 * Returns null when:
 *  - the value isn't a function (defensive — caller already checks)
 *  - the closure can't be stringified (rare native / bound functions)
 *  - the source is suspiciously large (> 32 KB — guard against the
 *    rare spec that inlines a giant table; we'd rather fall through
 *    to the Tier B / well-known recovery paths than ship huge JSON).
 */
const captureClosureSource = (fn: any): string | undefined => {
  if (typeof fn !== "function") return undefined;
  let body: string;
  try {
    body = fn.toString();
  } catch {
    return undefined;
  }
  if (body.length > 32 * 1024) return undefined;
  return `(${body})(globalThis.__nerv_tokens, () => Promise.resolve(""))`;
};

const convertOneGenerator = async (g: any): Promise<NervGenerator | null> => {
  // Well-known: filepaths / folders from @fig/autocomplete-generators.
  // Detected by the unique `ls -1ApL` signature their closures emit.
  const fp = detectFilepathsGenerator(g);
  if (fp) return { type: "filepaths", folders_only: fp.foldersOnly };

  // Well-known: zoxide directory history. The vendor z / zoxide
  // specs both build the same `zoxide query --list --score` call
  // inside a custom closure — recognise by the literal command
  // string in the closure source.
  if (
    g != null &&
    typeof g === "object" &&
    typeof g.custom === "function"
  ) {
    let src = "";
    try {
      src = g.custom.toString();
    } catch {
      // closure unstringifiable — fall through
    }
    if (src.includes('"zoxide"') && src.includes('"--list"')) {
      return { type: "zoxide_query" };
    }
    // Well-known: SSH host enumeration. Fig's ssh.ts exports
    // `knownHosts` (reads `~/.ssh/known_hosts` via `cat`) and
    // `configHosts` (reads `~/.ssh/config` and follows `Include`).
    // Both closures are reused by scp.ts / sftp.ts / mosh.ts /
    // rsync.ts. We sniff for either of the canonical path strings
    // — anything else with that literal is unlikely to exist.
    if (
      src.includes(".ssh/known_hosts") ||
      src.includes("known_hosts") ||
      (src.includes(".ssh") && src.includes("Host "))
    ) {
      return { type: "ssh_hosts" };
    }
    // Well-known: make's `listTargets` closure (vendor src/make.ts).
    // Reads `Makefile` / `makefile` / `GNUmakefile` via executeCommand
    // + cat and emits target names. Detect by literal filename
    // references — false-positive surface is essentially zero.
    if (
      src.includes("Makefile") ||
      src.includes("makefile") ||
      src.includes("GNUmakefile")
    ) {
      return { type: "makefile_targets" };
    }
    // Well-known: man's `generateManualPages` closure. The vendor
    // implementation runs `man -k .` (apropos all). Sniff that
    // literal plus the alternate `manpath` / `man1` forms in case
    // upstream rewrites the closure later.
    if (
      src.includes('command: "man"') ||
      src.includes("'man'") ||
      src.includes("manpath") ||
      src.includes('"man1"') ||
      src.includes("'man1'")
    ) {
      return { type: "man_pages" };
    }
    // Well-known: npm's `dependenciesGenerator` (and pnpm / yarn
    // equivalents that mirror the same shape). Reads `package.json`
    // and joins dependencies / devDependencies / optionalDependencies.
    // Detect by the `devDependencies` + `package.json` literal
    // co-occurrence — both nearly unique to this pattern.
    if (
      src.includes("devDependencies") &&
      (src.includes("package.json") || src.includes('"package.json"'))
    ) {
      return { type: "package_json_deps" };
    }
    // Well-known: kubectl's resource-type closure. The vendor closure
    // in src/kubectl.ts builds `["kubectl","get",<type>,"-o","custom-
    // columns=:.metadata.name"]` for the second arg via
    // `typeWithoutName`, and uses a static `["kubectl","api-resources",
    // "-o","name"]` (scripts.types) for the first. Both end up as
    // closures in the conversion pass — fall back to running the
    // resource-types call directly. False-positive surface is tiny
    // since `api-resources` / `typeWithoutName` / `custom-columns=`
    // are kubectl-specific strings.
    if (
      src.includes("api-resources") ||
      src.includes("typeWithoutName") ||
      src.includes("custom-columns=:.metadata.name")
    ) {
      return { type: "kubectl_resources" };
    }
    // Well-known: systemctl's `unitGenerator` / `unitFileGenerator`
    // (vendor/withfig-autocomplete/src/systemctl.ts). Both shell out
    // to `systemctl list-units -o json --all --full` (or list-unit-
    // files). The complete.rs JSON extractor handles the `.unit` /
    // `.unit_file` field lookup. Closure source check is exact.
    if (src.includes('"systemctl"')) {
      if (src.includes('"list-units"')) {
        return {
          type: "template",
          script: [
            "systemctl",
            "list-units",
            "-o",
            "json",
            "--all",
            "--full",
          ],
        };
      }
      if (src.includes('"list-unit-files"')) {
        return {
          type: "template",
          script: [
            "systemctl",
            "list-unit-files",
            "-o",
            "json",
            "--all",
            "--full",
          ],
        };
      }
    }
    // Well-known: git's branch enumeration closures
    // (`gitGenerators.localBranches` / `localOrRemoteBranches`
    // in vendor/withfig-autocomplete/src/git.ts). Both shell out to
    // `git ... branch ... --no-color --sort=-committerdate` and
    // post-process to strip the `* ` / `+ ` markers. Rewrite to a
    // Template form — sanitize_generator_line already handles the
    // marker stripping. False positives essentially zero (the
    // git-marker + --sort=-committerdate co-occurrence is unique).
    if (
      (src.includes("--sort=-committerdate") ||
        src.includes("'-sort=-committerdate'")) &&
      (src.includes('"branch"') || src.includes("'branch'"))
    ) {
      const wantsRemote = src.includes('"-r"') || src.includes("'-r'");
      return {
        type: "template",
        script: wantsRemote
          ? [
              "git",
              "--no-optional-locks",
              "branch",
              "-a",
              "--no-color",
              "--sort=-committerdate",
            ]
          : [
              "git",
              "--no-optional-locks",
              "branch",
              "--no-color",
              "--sort=-committerdate",
            ],
      };
    }
    // Well-known: cargo's `targetGenerator({ kind })` — runs
    // `cargo metadata --format-version 1 --no-deps` and walks
    // `packages[*].targets[*]`, optionally filtering by
    // `target.kind.includes(kind)`. The closure captures `kind`
    // from outer scope so we can't read it from source — probe
    // the closure with synthetic packages to recover it.
    if (
      src.includes('"cargo"') &&
      src.includes('"metadata"') &&
      (src.includes("target.kind") || src.includes("targets.filter"))
    ) {
      const k = await probeCargoKind(g);
      if (k !== null) {
        return {
          type: "cargo_targets",
          kind: k === "any" ? null : k,
        };
      }
    }
  }

  if (typeof g === "function") {
    // Well-known: aws `listCustomGenerator(tokens, exec, "verb",
    // flags, "Parent", "Child")` — token-aware service enumeration.
    // Detected here because the function-form variant is used by
    // some aws files directly (no `custom:` wrapper).
    const aws = detectAwsListCustom(g);
    if (aws) {
      return {
        type: "aws_list",
        service: AWS_SERVICE_HINT!,
        verb: aws.verb,
        lookup_flags: aws.lookup_flags,
        parent_key: aws.parent_key,
        id_field: aws.id_field,
      };
    }
    // Custom generator function — Tier C. Capture source so the
    // `quickjs` opt-in build can execute it in the sandbox. Builds
    // without the feature parse and ignore the field.
    const source = captureClosureSource(g);
    return { type: "custom", description_hint: null, ...(source ? { source } : {}) };
  }
  if (typeof g !== "object" || g == null) return null;

  // `custom: async (...)=>{...}` form
  if (typeof g.custom === "function") {
    // Same well-known aws closure recovery, in object form.
    const aws = detectAwsListCustom(g.custom);
    if (aws) {
      return {
        type: "aws_list",
        service: AWS_SERVICE_HINT!,
        verb: aws.verb,
        lookup_flags: aws.lookup_flags,
        parent_key: aws.parent_key,
        id_field: aws.id_field,
      };
    }
    const source = captureClosureSource(g.custom);
    return { type: "custom", description_hint: null, ...(source ? { source } : {}) };
  }

  // `script:` form — can be string, string[], or function returning
  // string[]. We resolve function-form by calling with a stub context
  // (most kubectl-style closures just rewrite tokens into a fixed
  // command).
  if (g.script !== undefined) {
    // Well-known sniff BEFORE attempting resolution — function-form
    // closures that capture `tokens` (e.g. kubectl's typeWithoutName)
    // can't be statically executed but we can still recognise the
    // signature and pivot to a known-good static command.
    if (typeof g.script === "function") {
      let src = "";
      try {
        src = g.script.toString();
      } catch {
        // closure unstringifiable — fall through
      }
      if (
        src.includes("api-resources") ||
        src.includes("typeWithoutName") ||
        src.includes("custom-columns=:.metadata.name")
      ) {
        return { type: "kubectl_resources" };
      }
    }
    let scriptArr: string[] = [];
    if (typeof g.script === "function") {
      scriptArr = tryResolveScriptFn(g.script);
    } else if (Array.isArray(g.script)) {
      scriptArr = g.script.map((s: any) => String(s));
    } else if (typeof g.script === "string") {
      scriptArr = splitShellCommand(g.script);
    }
    if (scriptArr.length === 0) {
      // Couldn't recover a runnable command — surface as Tier C
      // marker so `nerv spec list` can show it without dropping.
      return {
        type: "script",
        script: [],
        has_post_process: typeof g.postProcess === "function",
      };
    }
    if (typeof g.postProcess === "function") {
      // Well-known: `npmScriptsGenerator` (`cat package.json` +
      // JSON.parse closure). Reused by npm/yarn/pnpm/bun/rushx/nr.
      if (isPackageJsonScriptsSignature(scriptArr)) {
        return { type: "package_json_scripts" };
      }
      // Well-known: aws `postPrecessGenerator(out, parentKey, idField)`
      // — recurring across iam/ec2/cloudfront/route53/etc. The closure
      // boils down to `JSON.parse(stdout)[parentKey]` then mapping
      // each element to `elm[idField]`. Capture as data so the Rust
      // engine can recover the same shape with no JS runtime.
      const jsonPath = detectAwsJsonPath(g.postProcess);
      if (jsonPath) {
        return {
          type: "script_with_json_path",
          script: scriptArr,
          parent_key: jsonPath.parent_key,
          id_field: jsonPath.id_field,
        };
      }
      // For everything else with a postProcess: still execute the
      // script as a Template. The engine streams raw stdout lines as
      // candidates — close enough for `-o name` / `--format` outputs
      // (kubectl/docker/gh). Worst case the user sees raw text
      // instead of a transformed label; better than empty.
      return { type: "template", script: scriptArr };
    }
    return { type: "template", script: scriptArr };
  }

  // `template:` form — handled at the arg level, not here.
  return null;
};

/** Quick-and-dirty shell command splitter — splits on whitespace, no quoting. */
const splitShellCommand = (cmd: string): string[] =>
  cmd
    .split(/\s+/)
    .filter((s) => s.length > 0);

/**
 * Try to invoke a Fig `generators.script: (tokens, context) => string[]`
 * closure with a stub argument set so we can capture the resulting
 * command. Many kubectl/docker/gh closures just rewrite tokens into
 * a fixed `[bin, sub, ...]` argv and don't depend on user state — for
 * those, this recovers a runnable Tier B script. Closures that touch
 * tokens / cwd in ways we don't simulate will return [] and the
 * generator stays Tier C.
 */
const tryResolveScriptFn = (fn: any): string[] => {
  const stubContext = {
    environmentVariables: process.env,
    currentProcess: "zsh",
    currentWorkingDirectory: process.cwd(),
    isDangerous: false,
    searchTerm: "",
  };
  try {
    const out = fn([], stubContext);
    if (Array.isArray(out) && out.every((x) => typeof x === "string")) {
      return out.map(String);
    }
    if (typeof out === "string" && out.length > 0) {
      return splitShellCommand(out);
    }
  } catch {
    // closure crashed on stub args — leave as Tier C
  }
  return [];
};

/**
 * Recognise the npmScriptsGenerator signature emitted by
 * `@withfig/autocomplete`'s npm/yarn/pnpm/bun/rushx/nr specs. The shape
 * is exactly `["bash","-c", "... cat package.json"]` where the inner
 * shell command walks the cwd upward until it finds a package.json.
 */
const isPackageJsonScriptsSignature = (script: string[]): boolean => {
  if (script.length !== 3) return false;
  if (script[0] !== "bash" && script[0] !== "sh") return false;
  if (script[1] !== "-c") return false;
  const cmd = script[2];
  return cmd.includes("package.json") && cmd.includes("cat ");
};

const convertArg = async (a: FigArg): Promise<NervArg> => {
  const names = namesOf(a.name);

  // Fig accepts nested name aliases: [["auto", "automatic"], "always"] —
  // each top-level slot can be either a string, a {name, description,
  // displayName, insertValue, icon, priority} object, or an array of
  // either. Flatten, preserve rich fields when present, emit bare
  // strings when nothing extra to carry.
  const richSuggestion = (s: any): NervSuggestion | NervSuggestion[] | null => {
    if (typeof s === "string") return s;
    if (Array.isArray(s)) {
      return s.flatMap((x: any) => {
        const r = richSuggestion(x);
        return r === null ? [] : Array.isArray(r) ? r : [r];
      });
    }
    if (s && typeof s === "object" && typeof s.name === "string") {
      const hasExtra =
        s.description != null ||
        s.displayName != null ||
        s.insertValue != null ||
        s.icon != null ||
        s.priority != null;
      if (!hasExtra) return s.name;
      return {
        name: s.name,
        ...(s.description != null ? { description: s.description } : {}),
        ...(s.displayName != null ? { displayName: s.displayName } : {}),
        ...(s.insertValue != null ? { insertValue: s.insertValue } : {}),
        ...iconField(s.icon),
        ...(s.priority != null ? { priority: s.priority } : {}),
      };
    }
    return null;
  };
  const suggestions: NervSuggestion[] = (a.suggestions ?? []).flatMap((s: any) => {
    const r = richSuggestion(s);
    if (r === null) return [];
    return Array.isArray(r) ? r : [r];
  });

  return {
    name: names[0] ?? null,
    description: a.description ?? null,
    is_optional: a.isOptional ?? false,
    is_variadic: a.isVariadic ?? false,
    suggestions,
    template: convertTemplate(a.template),
    generators: await convertGenerators(a.generators),
    // Fig getQueryTerm: string → as-is; string[] → join (each entry is
    // a delim char); function → defer to M1 (Tier C closure exec).
    ...(typeof a.getQueryTerm === "string"
      ? { getQueryTerm: a.getQueryTerm }
      : Array.isArray(a.getQueryTerm)
        ? { getQueryTerm: a.getQueryTerm.join("") }
        : {}),
    // Fig filterStrategy: pass through known values. Fuzzy is M1
    // opt-in only — engine silently downgrades to prefix in v1.0.
    ...(typeof a.filterStrategy === "string"
      ? { filterStrategy: a.filterStrategy }
      : {}),
  };
};

const convertOpt = async (o: FigOpt): Promise<NervOpt> => {
  const names = namesOf(o.name);
  return {
    names,
    description: o.description ?? null,
    args: await argsOf(o.args),
    exclusive_on: o.exclusiveOn ?? [],
    depends_on: o.dependsOn ?? [],
    is_required: o.isRequired ?? false,
    is_repeatable: typeof o.isRepeatable === "boolean" ? o.isRepeatable : false,
    hidden: o.hidden ?? false,
    ...(o.isPersistent === true ? { isPersistent: true } : {}),
    ...(typeof o.priority === "number" ? { priority: o.priority } : {}),
    // Fig allows a string separator (e.g. `:`) — we collapse to bool.
    // Any truthy value (including non-empty string) means `=` required.
    ...(o.requiresSeparator ? { requiresSeparator: true } : {}),
    ...iconField(o.icon),
  };
};

// Strip Fig's icon-registry URL refs (`fig://icon?type=...`) — they
// mean nothing in a terminal. Trim short visible glyphs only (≤4
// bytes). Returns undefined when the input is missing or unusable.
const sanitizeIcon = (raw: unknown): string | undefined => {
  if (typeof raw !== "string") return undefined;
  const s = raw.trim();
  if (!s || s.startsWith("fig://") || s.length > 4) return undefined;
  return s;
};

// Spread-friendly wrapper: call sanitizeIcon once per emit site
// instead of twice (one in guard, one in value). Returns `{}` to
// drop the field cleanly via object spread.
const iconField = (raw: unknown): { icon?: string } => {
  const ic = sanitizeIcon(raw);
  return ic !== undefined ? { icon: ic } : {};
};

type Ctx = {
  baseDir: string;
  visited: Set<string>;
  depth: number;
};

/// loadSpec inlining depth cap.
/// depth=0: never inline; subdir specs ship as stub subcommands.
/// depth=1: top-level loadSpec strings resolved. `aws ec2 <verb>`,
///   `gcloud compute <verb>` work; nested loadSpec inside the verb's
///   own subspec is not followed.
/// depth=2 (current): pulls the next level for the rare specs that
///   chain (dotnet, pnpx, gcloud subgroups). Measured cost on the
///   715-spec corpus: ~5% disk growth vs depth=1, well under the
///   gzip path's 10× compression. The previous "depth=4 → 100 MB"
///   warning was for a different (unfiltered + uncompressed) build;
///   today's pipeline absorbs depth=2 comfortably.
const MAX_DEPTH = 2;

// Specs whose top-level uses `generateSpec: async (...)` to pick
// between alternate spec trees at runtime. We can't run the closure
// safely in general (vendor specs may shell out), so this is a
// curated allow-list. Each entry resolves the closure with a mock
// executeShellCommand that returns success (status 0, empty stdout)
// — the heuristic Fig itself uses to prefer the "modern" branch.
const GENERATE_SPEC_ALLOW = new Set(["z"]);

const tryResolveGenerateSpec = async (
  s: FigSpec | any,
  fallbackName?: string,
): Promise<FigSpec | any> => {
  if (typeof (s as any)?.generateSpec !== "function") return s;
  const name = namesOf(s.name)[0] ?? fallbackName ?? "";
  if (!GENERATE_SPEC_ALLOW.has(name)) return s;
  const mockExecute = async () => ({ status: 0, stdout: "", stderr: "" });
  try {
    const child = await (s as any).generateSpec([], mockExecute, {
      currentWorkingDirectory: process.cwd(),
    });
    if (child) return child;
  } catch (e: any) {
    console.error(
      `[ts-to-json]  generateSpec(${name}) failed: ${e?.message ?? e}`,
    );
  }
  return s;
};

const convertSpec = async (
  raw: FigSpec,
  ctx: Ctx,
  fallbackName?: string
): Promise<NervSpec> => {
  // Resolve dynamic `generateSpec` selectors (allow-listed) before
  // normalising — otherwise we'd emit an empty spec.
  const s = await tryResolveGenerateSpec(raw, fallbackName);
  // Inline a referenced subspec when `loadSpec: "<relative/path>"` is
  // a literal string. Fig's runtime would lazy-load these; for static
  // JSON we eagerly inline up to MAX_DEPTH. Cycles + over-deep nesting
  // are detected and skipped (the spec becomes a stub subcommand with
  // its original name + description but empty subtree).
  if (typeof s.loadSpec === "string" && ctx.depth < MAX_DEPTH) {
    const childPath = resolve(ctx.baseDir, `${s.loadSpec}.ts`);
    if (!ctx.visited.has(childPath)) {
      try {
        ctx.visited.add(childPath);
        const child = await loadOneAt(childPath, {
          baseDir: dirname(childPath),
          visited: ctx.visited,
          depth: ctx.depth + 1,
        });
        if (child) {
          const allNames = namesOf(s.name);
          const primary = allNames[0] ?? child.name ?? fallbackName ?? "";
          return {
            name: primary,
            aliases: allNames.slice(1),
            description: s.description ?? child.description,
            subcommands: child.subcommands,
            options: child.options,
            args: child.args,
            requires_double_dash: false,
            hidden: s.hidden ?? false,
          };
        }
      } catch (e: any) {
        // Missing subspec or import failure — fall through to stub.
        console.error(
          `[ts-to-json]  loadSpec "${s.loadSpec}" failed: ${e?.message ?? e}`
        );
      }
    }
  }

  const allNames = namesOf(s.name);
  const primary = allNames[0] ?? fallbackName ?? "";
  const aliases = allNames.slice(1);
  const subcommands: NervSpec[] = [];
  for (const sc of s.subcommands ?? []) {
    subcommands.push(await convertSpec(sc, ctx));
  }
  return {
    name: primary,
    aliases,
    description: s.description ?? null,
    subcommands,
    options: await Promise.all((s.options ?? []).map(convertOpt)),
    args: await argsOf(s.args),
    requires_double_dash: false,
    hidden: s.hidden ?? false,
    ...(typeof (s as any).priority === "number"
      ? { priority: (s as any).priority }
      : {}),
    ...iconField((s as any).icon),
    ...(s.parserDirectives?.flagsArePosixNoncompliant === true
      ? { flagsArePosixNoncompliant: true }
      : {}),
  };
};

// Well-known gap fill: the k8s-family CLIs declare a root
// `-n / --namespace` GLOBAL flag whose arg ships with NO generator
// upstream (e.g. vendor src/kubectl.ts ~3857 — bare
// `args: { name: "namespace" }`), so faithful conversion leaves it
// empty and `kubectl get pods -n <Tab>` completes nothing
// (first-5-min.md step 11). Inject the obvious Tier B template —
// the engine's existing generator machinery (200ms timeout + LRU
// cache) handles the rest. Applied to every `-n`/`--namespace`
// option in the tree whose arg has no generator of its own, so
// `kubectl config set-context --namespace=` etc. light up too.
//
// Specs that get this pass: each verified to use the flag with k8s
// namespace semantics at the ROOT (global flag) level. `-n` elsewhere
// (git, aws, sfdx, …) means something else entirely — do NOT add a
// spec here without checking its root option description.
const K8S_NAMESPACE_SPEC_STEMS = new Set([
  "kubectl",
  "helm",
  "helmfile",
  "kubecolor",
  "argo",
]);

const KUBECTL_NAMESPACES_SCRIPT = [
  "kubectl",
  "get",
  "namespaces",
  "--no-headers",
  "-o",
  "custom-columns=:metadata.name",
];

export const enrichK8sNamespaces = (
  spec: NervSpec,
  isRoot: boolean = true
): void => {
  for (const opt of spec.options) {
    if (!opt.names.some((n) => n === "-n" || n === "--namespace")) continue;
    // The root `-n` is a GLOBAL flag (usable after any subcommand —
    // `kubectl get pods -n staging`), but upstream doesn't mark it
    // isPersistent, so the parser refuses to bind it mid-chain.
    // Record the real CLI semantics. Root level only — subcommand-
    // local `--namespace` (config set-context) stays local.
    if (isRoot) opt.isPersistent = true;
    for (const arg of opt.args) {
      if (arg.generators.length === 0) {
        arg.generators.push({
          type: "template",
          script: KUBECTL_NAMESPACES_SCRIPT,
        });
      }
    }
  }
  for (const sub of spec.subcommands) enrichK8sNamespaces(sub, false);
};

const loadOne = async (file: string): Promise<NervSpec | null> => {
  const abs = resolve(file);
  return loadOneAt(abs, {
    baseDir: dirname(abs),
    visited: new Set([abs]),
    depth: 0,
  });
};

const loadOneAt = async (file: string, ctx: Ctx): Promise<NervSpec | null> => {
  const mod = await import(file);
  const exported = mod.default ?? mod.completionSpec;
  if (!exported) {
    console.error(`[ts-to-json] no default export: ${file}`);
    return null;
  }
  const stem = basename(file, extname(file));
  // Detect aws service file (vendor/withfig-autocomplete/src/aws/<service>.ts).
  // Used by `convertOneGenerator` to recover `listCustomGenerator(...)`
  // calls into `Generator::AwsList { service, ... }` without threading
  // an extra arg through the whole conversion chain.
  const prev = AWS_SERVICE_HINT;
  AWS_SERVICE_HINT = file.includes("/aws/") ? stem : null;
  try {
    const spec = await convertSpec(exported as FigSpec, ctx, stem);
    if (K8S_NAMESPACE_SPEC_STEMS.has(stem)) enrichK8sNamespaces(spec);
    return spec;
  } finally {
    AWS_SERVICE_HINT = prev;
  }
};

let AWS_SERVICE_HINT: string | null = null;

const collectInputs = async (input: string): Promise<string[]> => {
  const st = await stat(input);
  if (st.isFile()) return [input];
  const entries = await readdir(input);
  return entries
    .filter((e) => e.endsWith(".ts"))
    .map((e) => join(input, e));
};

const parseArgs = () => {
  const args = process.argv.slice(2);
  let input: string | undefined;
  let output: string | undefined;
  let only: string[] = [];
  let verbose = false;
  for (let i = 0; i < args.length; i++) {
    switch (args[i]) {
      case "--input":
        input = args[++i];
        break;
      case "--output":
        output = args[++i];
        break;
      case "--only":
        only.push(args[++i]);
        break;
      case "--verbose":
        verbose = true;
        break;
    }
  }
  if (!input || !output) {
    console.error("Usage: bun convert.ts --input <file|dir> --output <file|dir> [--only NAME ...] [--verbose]");
    process.exit(2);
  }
  return { input, output, only, verbose };
};

const main = async () => {
  const { input, output, only, verbose } = parseArgs();
  const inputs = await collectInputs(input);
  const outputIsDir = inputs.length > 1 || !output.endsWith(".json");
  if (outputIsDir) {
    await mkdir(output, { recursive: true });
  } else {
    await mkdir(dirname(output), { recursive: true });
  }

  let loaded = 0;
  let skipped = 0;
  let failed = 0;
  for (const file of inputs) {
    const stem = basename(file, extname(file));
    if (only.length > 0 && !only.includes(stem)) {
      if (verbose) console.log(`skip ${stem}`);
      skipped++;
      continue;
    }
    try {
      const spec = await loadOne(file);
      if (!spec) {
        failed++;
        continue;
      }
      const outPath = outputIsDir ? join(output, `${stem}.json`) : output;
      await Bun.write(outPath, JSON.stringify(spec, null, 2) + "\n");
      loaded++;
      if (verbose) console.log(`load ${stem}`);
    } catch (e: any) {
      failed++;
      console.error(`FAIL ${stem}: ${e?.message ?? e}`);
    }
  }
  console.log(`ts-to-json: ${loaded} loaded, ${skipped} skipped, ${failed} failed`);
  if (failed > 0) process.exit(1);
};

// Skip the CLI when this module is imported elsewhere (e.g.
// `convert.test.ts` pulling in `detectAwsJsonPath`). `import.meta.main`
// is true only when bun runs this file directly.
if (import.meta.main) {
  main();
}
