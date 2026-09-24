import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import {
  convertTemplate,
  curatedExtensions,
  detectAwsJsonPath,
  detectAwsListCustom,
  detectCargoMetadataPackages,
  detectFilepathsGenerator,
  detectGitBranchScript,
  enrichK8sNamespaces,
  groupBranchesLocalFirst,
} from "./convert";

describe("convertTemplate", () => {
  test("a folders+filepaths array keeps files (rm, trash, subl)", () => {
    expect(convertTemplate(["folders", "filepaths"])).toBe("filepaths");
    expect(convertTemplate(["filepaths", "folders"])).toBe("filepaths");
  });

  test("a single kind passes through", () => {
    expect(convertTemplate("folders")).toBe("folders");
    expect(convertTemplate(["folders"])).toBe("folders");
    expect(convertTemplate(["history"])).toBe("history");
    expect(convertTemplate(undefined)).toBeNull();
  });
});

// Run the real converter on the live vendor file: the closure source it
// sniffs is what bun transpiles (`true` → `!0`, reflowed), not the .ts
// text. A subprocess because the vendor files import `@fig/*`, which
// resolves only through NODE_PATH, and `bun test` ignores NODE_PATH.
const convertVendor = (name: string): any => {
  const dir = mkdtempSync(`${tmpdir()}/nerv-convert-test-`);
  const out = `${dir}/${name}.json`;
  const r = Bun.spawnSync(
    ["bun", "convert.ts", "--input", `../../vendor/withfig-autocomplete/src/${name}.ts`, "--output", out],
    { cwd: import.meta.dir, env: { ...process.env, NODE_PATH: `${import.meta.dir}/node_modules` } },
  );
  try {
    if (r.exitCode !== 0) throw new Error(r.stderr.toString());
    return JSON.parse(readFileSync(out, "utf8"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};
const named = (list: any[], name: string): any =>
  list.find((n) => [n.names ?? n.name].flat().includes(name));
const GIT = ["git", "--no-optional-locks", "branch", "--no-color", "--sort=-committerdate"];

describe("git branch generators (converted vendor git.ts)", () => {
  const git = convertVendor("git");
  const branch = named(git.subcommands, "branch");
  const checkoutGens = named(git.subcommands, "checkout").args[0].generators;

  test("branch -d/-D lists only branches git can delete", () => {
    for (const flag of ["-d", "-D"]) {
      expect(named(branch.options, flag).args[0].generators).toEqual([
        {
          type: "template",
          script: [...GIT, "--format=%(if)%(worktreepath)%(then)%(else)%(refname:short)%(end)"],
        },
      ]);
    }
  });

  test("checkout lists local branches before remote ones", () => {
    expect(checkoutGens[0]).toEqual({
      type: "template",
      script: [
        "git",
        "--no-optional-locks",
        "branch",
        "-a",
        "--no-color",
        "--sort=-committerdate",
        "--sort=refname:rstrip=-2",
      ],
    });
  });

  test("branch -m keeps the current branch and the date order", () => {
    // `localBranches` (object form): renaming the current branch is valid.
    expect(named(branch.options, "-m").args[0].generators).toEqual([
      { type: "template", script: GIT },
    ]);
  });

  test("checkout tags are left alone", () => {
    expect(checkoutGens[1]).toEqual({
      type: "template",
      script: ["git", "--no-optional-locks", "tag", "--list", "--sort=-committerdate"],
    });
  });
});

describe("git-flow typeBranches (converted vendor git-flow.ts)", () => {
  test("keeps its local branch script", () => {
    const finish = named(named(convertVendor("git-flow").subcommands, "feature").subcommands, "finish");
    expect(finish.args[0].generators).toEqual([{ type: "template", script: GIT }]);
  });
});

describe("detectGitBranchScript", () => {
  test("an unrelated closure is not a branch generator", () => {
    expect(detectGitBranchScript('async () => run("git", ["tag"])')).toBeNull();
  });
});

describe("groupBranchesLocalFirst", () => {
  const all = ["git", "branch", "-a", "--sort=-committerdate"];

  test("adds the local-first key after the date sort", () => {
    expect(groupBranchesLocalFirst(all)).toEqual([...all, "--sort=refname:rstrip=-2"]);
  });

  test("leaves local-only and non-git scripts alone", () => {
    expect(groupBranchesLocalFirst(["git", "branch", "--sort=-committerdate"])).toEqual([
      "git",
      "branch",
      "--sort=-committerdate",
    ]);
    expect(groupBranchesLocalFirst(["echo", "branch", "-a", "--sort=-committerdate"])).toEqual([
      "echo",
      "branch",
      "-a",
      "--sort=-committerdate",
    ]);
  });
});

describe("curatedExtensions", () => {
  test("injects git flow (loadSpec git-flow) at the top level", () => {
    const got = curatedExtensions("git", new Set(), 0);
    expect(got).toHaveLength(1);
    expect(got[0]).toMatchObject({ name: "flow", loadSpec: "git-flow" });
  });

  test("skips a name the spec already declares", () => {
    const got = curatedExtensions("git", new Set(["flow"]), 0);
    expect(got).toHaveLength(0);
  });

  test("only applies at depth 0, and only to known specs", () => {
    expect(curatedExtensions("git", new Set(), 1)).toHaveLength(0);
    expect(curatedExtensions("other", new Set(), 0)).toHaveLength(0);
  });
});

describe("detectAwsJsonPath", () => {
  test("captures parent_key + id_field from canonical aws postProcess", () => {
    const postProcess = function (out: string) {
      return postPrecessGenerator(out, "OpenIDConnectProviderList", "Arn");
    };
    expect(detectAwsJsonPath(postProcess)).toEqual({
      parent_key: "OpenIDConnectProviderList",
      id_field: "Arn",
    });
  });

  test("captures parent_key alone when childKey is omitted", () => {
    const postProcess = function (out: string) {
      return postPrecessGenerator(out, "Buckets");
    };
    expect(detectAwsJsonPath(postProcess)).toEqual({
      parent_key: "Buckets",
      id_field: null,
    });
  });

  test("tolerates single quotes", () => {
    const postProcess = function (out: string) {
      return postPrecessGenerator(out, 'Functions', 'FunctionName');
    };
    expect(detectAwsJsonPath(postProcess)).toEqual({
      parent_key: "Functions",
      id_field: "FunctionName",
    });
  });

  test("returns null for unrelated postProcess closures", () => {
    const postProcess = function (out: string) {
      return out.split("\n").map((s) => ({ name: s }));
    };
    expect(detectAwsJsonPath(postProcess)).toBeNull();
  });

  test("returns null for non-function input", () => {
    expect(detectAwsJsonPath(null)).toBeNull();
    expect(detectAwsJsonPath(undefined)).toBeNull();
    expect(detectAwsJsonPath("string")).toBeNull();
  });
});

describe("detectAwsListCustom", () => {
  test("captures array-form flags (lambda style)", () => {
    const custom = async function (tokens: string[], exec: any) {
      return listCustomGenerator(
        tokens,
        exec,
        "list-layer-versions",
        ["--layer-name"],
        "LayerVersions",
        "Version",
      );
    };
    expect(detectAwsListCustom(custom, "lambda")).toEqual({
      verb: "list-layer-versions",
      lookup_flags: ["--layer-name"],
      parent_key: "LayerVersions",
      id_field: "Version",
    });
  });

  test("captures multi-flag array form", () => {
    const custom = async function (tokens: string[], exec: any) {
      return listCustomGenerator(
        tokens,
        exec,
        "describe-anomaly-detectors",
        ["--namespace", "--metric-name"],
        "AnomalyDetectors",
        "Stat",
      );
    };
    expect(detectAwsListCustom(custom, "cloudwatch")).toEqual({
      verb: "describe-anomaly-detectors",
      lookup_flags: ["--namespace", "--metric-name"],
      parent_key: "AnomalyDetectors",
      id_field: "Stat",
    });
  });

  test("captures single-string flag form (iam style)", () => {
    const custom = async function (tokens: string[], exec: any) {
      return listCustomGenerator(
        tokens,
        exec,
        "get-policy-version",
        "--policy-arn",
        "Versions",
        "VersionId",
      );
    };
    expect(detectAwsListCustom(custom, "iam")).toEqual({
      verb: "get-policy-version",
      lookup_flags: ["--policy-arn"],
      parent_key: "Versions",
      id_field: "VersionId",
    });
  });

  test("captures call with no childKey", () => {
    const custom = async function (tokens: string[], exec: any) {
      return listCustomGenerator(
        tokens,
        exec,
        "list-buckets",
        [],
        "Buckets",
      );
    };
    expect(detectAwsListCustom(custom, "s3")).toEqual({
      verb: "list-buckets",
      lookup_flags: [],
      parent_key: "Buckets",
      id_field: null,
    });
  });

  test("returns null without a service hint", () => {
    const custom = async function (tokens: string[], exec: any) {
      return listCustomGenerator(tokens, exec, "list-buckets", [], "Buckets");
    };
    expect(detectAwsListCustom(custom, null)).toBeNull();
  });

  test("returns null for closures that don't call listCustomGenerator", () => {
    const custom = async () => [{ name: "static" }];
    expect(detectAwsListCustom(custom, "lambda")).toBeNull();
  });

  test("returns null for non-function input", () => {
    expect(detectAwsListCustom(null, "lambda")).toBeNull();
    expect(detectAwsListCustom("not a fn", "lambda")).toBeNull();
  });

  test("recovers verb across multiline call sites", () => {
    const custom = async function (tokens: string[], exec: any) {
      return listCustomGenerator(
        tokens,
        exec,
        "list-stack-resources",
        [
          "--stack-name",
        ],
        "StackResourceSummaries",
        "LogicalResourceId",
      );
    };
    expect(detectAwsListCustom(custom, "cloudformation")).toEqual({
      verb: "list-stack-resources",
      lookup_flags: ["--stack-name"],
      parent_key: "StackResourceSummaries",
      id_field: "LogicalResourceId",
    });
  });
});

describe("detectFilepathsGenerator", () => {
  // The real @fig/autocomplete-generators filepaths closure spawns
  // `ls` with the literal flag array `["-1ApL"]`. The recognizer
  // anchors on that exact 7-char substring (incl. quotes).
  // Bun's runtime aggressively dead-code-eliminates closure bodies
  // (unused `const`s vanish from toString output, equality of two
  // string literals constant-folds away). To exercise the recognizer
  // from a Bun runtime test we hand it a literal Function constructed
  // from source — this preserves the source verbatim.
  const fnFromSrc = (body: string) =>
    new Function("_t", "_exec", `async function inner(){${body}}; return inner();`);

  test("detects folders-only closure (cd-style)", () => {
    const g = {
      custom: fnFromSrc(
        `const data = await _exec({ command: "ls", args: ["-1ApL"] });
         const showFolders = "only";
         return data.stdout.split("\\n").filter((s) => s.endsWith("/"));`,
      ),
    };
    expect(detectFilepathsGenerator(g)).toEqual({
      kind: "filepaths",
      foldersOnly: true,
    });
  });

  test("detects file-and-folder closure (cat-style)", () => {
    const g = {
      custom: fnFromSrc(
        `const data = await _exec({ command: "ls", args: ["-1ApL"] });
         return data.stdout.split("\\n").map((s) => ({ name: s }));`,
      ),
    };
    expect(detectFilepathsGenerator(g)).toEqual({
      kind: "filepaths",
      foldersOnly: false,
    });
  });

  test("detects via triple-equal showFolders compare", () => {
    const g = {
      custom: fnFromSrc(
        `const showFolders = pickMode();
         const data = await _exec({ command: "ls", args: ["-1ApL"] });
         return data.stdout.split("\\n").filter((s) =>
           showFolders === "only" ? s.endsWith("/") : true,
         );`,
      ),
    };
    expect(detectFilepathsGenerator(g)).toEqual({
      kind: "filepaths",
      foldersOnly: true,
    });
  });

  test("returns null when the signature ls flag is missing", () => {
    const g = {
      custom: fnFromSrc(
        `const data = await _exec({ command: "ls" });
         return data.stdout.split("\\n").map((s) => ({ name: s }));`,
      ),
    };
    expect(detectFilepathsGenerator(g)).toBeNull();
  });

  test("returns null when custom is absent", () => {
    expect(detectFilepathsGenerator({})).toBeNull();
    expect(detectFilepathsGenerator({ template: "filepaths" })).toBeNull();
  });

  test("returns null for null / non-object input", () => {
    expect(detectFilepathsGenerator(null)).toBeNull();
    expect(detectFilepathsGenerator(undefined)).toBeNull();
    expect(detectFilepathsGenerator("filepaths")).toBeNull();
  });

  test("returns null when custom is present but not a function", () => {
    expect(detectFilepathsGenerator({ custom: "ls -1ApL" })).toBeNull();
    expect(detectFilepathsGenerator({ custom: 42 })).toBeNull();
  });
});

describe("detectCargoMetadataPackages", () => {
  const cargoScript = ["cargo", "metadata", "--format-version", "1", "--no-deps"];

  test("captures the packageGenerator shape", () => {
    const postProcess = (data: string) => {
      const manifest = JSON.parse(data);
      return manifest.packages.map((pkg: any) => ({
        name: pkg.name,
        description: pkg.version,
      }));
    };
    expect(detectCargoMetadataPackages(cargoScript, postProcess)).toEqual({
      parent_key: "packages",
      id_field: "name",
    });
  });

  test("captures the dependencyGenerator shape (no --no-deps)", () => {
    const postProcess = (data: string) => {
      const metadata = JSON.parse(data);
      return metadata.packages.map((pkg: any) => ({ name: pkg.name }));
    };
    expect(
      detectCargoMetadataPackages(
        ["cargo", "metadata", "--format-version", "1"],
        postProcess,
      ),
    ).toEqual({ parent_key: "packages", id_field: "name" });
  });

  test("returns null when the script is not cargo metadata", () => {
    const postProcess = (d: string) => JSON.parse(d).packages;
    expect(detectCargoMetadataPackages(["cargo", "build"], postProcess)).toBeNull();
    expect(
      detectCargoMetadataPackages(["rustc", "--print", "cfg"], postProcess),
    ).toBeNull();
  });

  test("returns null when the postProcess does not read .packages", () => {
    const postProcess = (d: string) =>
      Object.keys(JSON.parse(d).features || {});
    expect(detectCargoMetadataPackages(cargoScript, postProcess)).toBeNull();
  });

  test("returns null for non-function postProcess", () => {
    expect(detectCargoMetadataPackages(cargoScript, null)).toBeNull();
    expect(detectCargoMetadataPackages(cargoScript, undefined)).toBeNull();
  });
});

describe("enrichK8sNamespaces", () => {
  const bareArg = () => ({
    name: "namespace",
    description: null,
    is_optional: false,
    is_variadic: false,
    suggestions: [],
    template: null,
    generators: [] as any[],
  });
  const spec = (over: any = {}) => ({
    name: "kubectl",
    aliases: [],
    description: null,
    subcommands: [],
    options: [],
    args: [],
    requires_double_dash: false,
    hidden: false,
    ...over,
  });

  test("injects the namespaces template into a bare -n arg", () => {
    const s = spec({
      options: [
        {
          names: ["-n", "--namespace"],
          description: null,
          args: [bareArg()],
          exclusive_on: [],
          depends_on: [],
          is_required: false,
          is_repeatable: false,
          hidden: false,
        },
      ],
    });
    enrichK8sNamespaces(s as any);
    expect(s.options[0].args[0].generators).toEqual([
      {
        type: "template",
        script: [
          "kubectl",
          "get",
          "namespaces",
          "--no-headers",
          "-o",
          "custom-columns=:metadata.name",
        ],
      },
    ]);
    // Root -n is kubectl's global flag — must become persistent so
    // the parser binds it after `get pods`.
    expect((s.options[0] as any).isPersistent).toBe(true);
  });

  test("recurses into subcommand options (config set-context --namespace)", () => {
    const s = spec({
      subcommands: [
        spec({
          name: "config",
          subcommands: [
            spec({
              name: "set-context",
              options: [
                {
                  names: ["--namespace"],
                  description: null,
                  args: [bareArg()],
                  exclusive_on: [],
                  depends_on: [],
                  is_required: false,
                  is_repeatable: false,
                  hidden: false,
                },
              ],
            }),
          ],
        }),
      ],
    });
    enrichK8sNamespaces(s as any);
    const opt = s.subcommands[0].subcommands[0].options[0];
    expect(opt.args[0].generators).toHaveLength(1);
    expect(opt.args[0].generators[0].type).toBe("template");
    // Subcommand-local --namespace must NOT become persistent.
    expect((opt as any).isPersistent).toBeUndefined();
  });

  test("leaves args with an existing generator untouched", () => {
    const arg = bareArg();
    arg.generators.push({ type: "kubectl_resources" });
    const s = spec({
      options: [
        {
          names: ["-n"],
          description: null,
          args: [arg],
          exclusive_on: [],
          depends_on: [],
          is_required: false,
          is_repeatable: false,
          hidden: false,
        },
      ],
    });
    enrichK8sNamespaces(s as any);
    expect(arg.generators).toEqual([{ type: "kubectl_resources" }]);
  });

  test("ignores unrelated options (-o, --name)", () => {
    const arg = bareArg();
    const s = spec({
      options: [
        {
          names: ["-o", "--output"],
          description: null,
          args: [arg],
          exclusive_on: [],
          depends_on: [],
          is_required: false,
          is_repeatable: false,
          hidden: false,
        },
      ],
    });
    enrichK8sNamespaces(s as any);
    expect(arg.generators).toEqual([]);
  });
});
