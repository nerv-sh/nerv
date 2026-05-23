# ts-to-json

Converts `withfig/autocomplete` TypeScript specs into Nerv-shape JSON
that `nerv-engine::spec_loader` can consume.

## Status

- Tier A (pure static): full conversion
- Tier B (generator.template with literal shell command): preserved
- Tier C (generator.custom or generator.script with postProcess):
  recorded as a marker; deferred to M1 + rquickjs for execution

## Setup

```bash
bun install                 # or pnpm install
```

Bun is preferred — it imports TypeScript natively without a separate
build step.

## Usage

```bash
# Single file
bun run convert:one

# Whole vendored library (715 top-level specs)
bun run convert:all

# Arbitrary path
NODE_PATH=$PWD/node_modules bun convert.ts \
    --input  ../../vendor/withfig-autocomplete/src/ \
    --output ../../crates/nerv-engine/tests/fixtures/converted/ \
    --only git --only docker
```

Output is `<stem>.json` per spec.

## Output location

The default `convert:all` script writes to
`crates/nerv-engine/tests/fixtures/converted/`, which is `.gitignore`d
(45+ MB of generated JSON).

To install locally:

```bash
cp crates/nerv-engine/tests/fixtures/converted/*.json \
   ~/Library/Caches/nerv/specs/
nerv stop && nerv start
```

## Refs

- `docs/spec-conversion-policy.md` — Tier A/B/C policy
- `PLAN.md` §10 M0-6 — spec_loader / build-specs / TS→JSON pipeline
- Upstream specs: `vendor/withfig-autocomplete/src/*.ts` (ISC)
