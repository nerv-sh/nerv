import { describe, expect, test } from "bun:test";
import {
  detectAwsJsonPath,
  detectAwsListCustom,
  detectFilepathsGenerator,
} from "./convert";

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
