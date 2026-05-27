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
const detectFilepathsGenerator = (
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

type FigArg = {
  name?: string | string[];
  description?: string;
  isOptional?: boolean;
  isVariadic?: boolean;
  suggestions?: Array<string | { name?: string }>;
  template?: string | string[];
  generators?: any | any[];
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
};

type NervArg = {
  name: string | null;
  description: string | null;
  is_optional: boolean;
  is_variadic: boolean;
  suggestions: string[];
  template: string | null;
  generators: NervGenerator[];
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
};

type NervGenerator =
  | { type: "template"; script: string[] }
  | { type: "script"; script: string[]; has_post_process: boolean }
  | { type: "custom"; description_hint: string | null }
  | { type: "package_json_scripts" }
  | { type: "filepaths"; folders_only: boolean }
  | { type: "zoxide_query" }
  | { type: "ssh_hosts" }
  | { type: "makefile_targets" }
  | { type: "man_pages" }
  | { type: "package_json_deps" }
  | { type: "kubectl_resources" };

/** Normalize a Fig `name` field (string | string[]) into our names array.
 *  Fig sometimes embeds `null` or sparse holes — filter to non-empty strings. */
const namesOf = (n: string | string[] | undefined | null): string[] => {
  if (n == null) return [];
  const list = Array.isArray(n) ? n : [n];
  return list.filter((x): x is string => typeof x === "string" && x.length > 0);
};

/** Normalize `args` (object | array | undefined) into our array form. */
const argsOf = (a: FigArg | FigArg[] | undefined): NervArg[] => {
  if (a == null) return [];
  const list = Array.isArray(a) ? a : [a];
  return list.map(convertArg);
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

const convertGenerators = (g: any | any[] | undefined): NervGenerator[] => {
  if (g == null) return [];
  const list = Array.isArray(g) ? g : [g];
  return list.map(convertOneGenerator).filter((x): x is NervGenerator => x !== null);
};

const convertOneGenerator = (g: any): NervGenerator | null => {
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
  }

  if (typeof g === "function") {
    // Custom generator function — Tier C, deferred to M1 rquickjs.
    return { type: "custom", description_hint: null };
  }
  if (typeof g !== "object" || g == null) return null;

  // `custom: async (...)=>{...}` form
  if (typeof g.custom === "function") {
    return { type: "custom", description_hint: null };
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

const convertArg = (a: FigArg): NervArg => {
  const names = namesOf(a.name);

  // Fig accepts nested name aliases: [["auto", "automatic"], "always"] —
  // each top-level slot can be either a string, a {name} object, or an
  // array of either. Flatten everything to a primary name (first slot)
  // so our suggestions field stays Vec<String>.
  const suggestions: string[] = (a.suggestions ?? []).flatMap((s: any) => {
    if (typeof s === "string") return [s];
    if (Array.isArray(s)) {
      return s.flatMap((x: any) => {
        if (typeof x === "string") return [x];
        if (x && typeof x === "object" && typeof x.name === "string") return [x.name];
        return [];
      });
    }
    if (s && typeof s === "object" && typeof s.name === "string") return [s.name];
    return [];
  });

  return {
    name: names[0] ?? null,
    description: a.description ?? null,
    is_optional: a.isOptional ?? false,
    is_variadic: a.isVariadic ?? false,
    suggestions,
    template: convertTemplate(a.template),
    generators: convertGenerators(a.generators),
  };
};

const convertOpt = (o: FigOpt): NervOpt => {
  const names = namesOf(o.name);
  return {
    names,
    description: o.description ?? null,
    args: argsOf(o.args),
    exclusive_on: o.exclusiveOn ?? [],
    depends_on: o.dependsOn ?? [],
    is_required: o.isRequired ?? false,
    is_repeatable: typeof o.isRepeatable === "boolean" ? o.isRepeatable : false,
    hidden: o.hidden ?? false,
  };
};

type Ctx = {
  baseDir: string;
  visited: Set<string>;
  depth: number;
};

/// loadSpec inlining depth cap.
/// depth=0: never inline; subdir specs ship as stub subcommands.
/// depth=1 (current): top-level loadSpec strings resolved. e.g.
///   `aws ec2 <verb>` works; `aws ec2 run-instances <flag>` still
///   stops at the verb's options without going into nested loadSpec.
///   Pairs with the spec_loader gzip path so the resulting 100MB+
///   plain JSON shrinks to ~10MB on disk.
/// depth=2+: very large output (aws hit 100 MB at depth 4 even
///   with cycle detection). Defer until lazy-eviction lands so
///   memory doesn't grow with the cache.
const MAX_DEPTH = 1;

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
    options: (s.options ?? []).map(convertOpt),
    args: argsOf(s.args),
    requires_double_dash: false,
    hidden: s.hidden ?? false,
  };
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
  return convertSpec(exported as FigSpec, ctx, stem);
};

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

main();
