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
  | { type: "custom"; description_hint: string | null };

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
  if (typeof g === "function") {
    // Custom generator function — Tier C, deferred to M1 rquickjs.
    return { type: "custom", description_hint: null };
  }
  if (typeof g !== "object" || g == null) return null;

  // `custom: async (...)=>{...}` form
  if (typeof g.custom === "function") {
    return { type: "custom", description_hint: null };
  }

  // `script:` form — can be string, string[], or async function.
  if (g.script !== undefined) {
    if (typeof g.script === "function") {
      // Dynamic script function — Tier C.
      return {
        type: "script",
        script: [],
        has_post_process: typeof g.postProcess === "function",
      };
    }
    const scriptArr = Array.isArray(g.script)
      ? g.script.map((s: any) => String(s))
      : typeof g.script === "string"
        ? splitShellCommand(g.script)
        : [];
    if (typeof g.postProcess === "function") {
      return {
        type: "script",
        script: scriptArr,
        has_post_process: true,
      };
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

const convertSpec = (s: FigSpec, fallbackName?: string): NervSpec => {
  const allNames = namesOf(s.name);
  const primary = allNames[0] ?? fallbackName ?? "";
  const aliases = allNames.slice(1);
  return {
    name: primary,
    aliases,
    description: s.description ?? null,
    subcommands: (s.subcommands ?? []).map((sc) => convertSpec(sc)),
    options: (s.options ?? []).map(convertOpt),
    args: argsOf(s.args),
    requires_double_dash: false,
    hidden: s.hidden ?? false,
  };
};

const loadOne = async (file: string): Promise<NervSpec | null> => {
  const abs = resolve(file);
  const mod = await import(abs);
  const exported = mod.default ?? mod.completionSpec;
  if (!exported) {
    console.error(`[ts-to-json] no default export: ${file}`);
    return null;
  }
  const stem = basename(file, extname(file));
  return convertSpec(exported as FigSpec, stem);
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
