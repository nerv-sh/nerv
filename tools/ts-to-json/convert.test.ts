import { describe, expect, test } from "bun:test";
import { detectAwsJsonPath, detectAwsListCustom } from "./convert";

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
