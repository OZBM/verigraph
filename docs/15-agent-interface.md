# 15 · Agent interface

The toolchain's answers are the user interface of an AI-first language: an agent reads every error, verdict and counterexample, and acts on it without a human in between. So these answers are specified as tightly as the binary format: names and locations, error codes, obligation IDs, JSON values, the MCP tool schemas, the failure report, the plain-language contract, output budgets and the incremental checker. docs/07 describes the loop that uses them.

## Principles

SWE-agent (Yang et al., NeurIPS 2024) showed that interface design alone changes how well a model performs as an agent, and that concise feedback and guardrails at each action matter most. Every tool here follows seven rules.

1. **Typed and stable.** Every response is JSON that matches the schemas below. A field or code never changes meaning; fields may be added; codes are append-only and never reused.
2. **Located in the agent's words.** Errors point at what the agent wrote: node label, port as written, parameter name, line and column. Canonical indices appear only beside them.
3. **Smallest sufficient evidence.** One counterexample, one slice and one condition, never a solver log.
4. **Deterministic.** The same request on the same store gives the same bytes: object keys sorted, lists in a documented order, no timestamps.
5. **Bounded.** Default output fits the budgets below; more detail is asked for explicitly.
6. **Actionable.** An error says what was expected and what was found and, when the repair is mechanical, carries it as an edit.
7. **Honest.** The verdict vocabulary keeps proven, refuted and unknown apart, and says why an unknown is unknown.

## Locations and names

- A **span** is `{"line": l, "col": c, "len": n}`: 1-based line and column, counted in Unicode scalar values, and a length in the same unit.
- A **node** is named by its label (`@soft_knee`), an **op** by its first result port as written (`%4`), a **parameter** by its name.
- `vg asm` keeps a **source map** from each canonical op to its span and port names: in memory always, in the DEBUG section with `--keep-debug`. vg-smt, vg-telemetry and the runtime guard map (docs/06) report through it. A node with no DEBUG section, such as one fetched from Mesh, is reported in the canonical ports that `vg disasm` prints.

## Obligation IDs

An obligation ID names one proof goal for readers; its hash (docs/02) is the cache key. Reports carry both.

| Form | Obligation |
| --- | --- |
| `@n/POST.i` | conjunct i of POST, with refined result types appended in result order |
| `@n/COST.i` | conjunct i of COST |
| `@n/NONVACUOUS` | PRE, refined parameter types and the LEARNABLE box are satisfiable together |
| `@n/%p/<OP>.<kind>` | an op's own obligation from docs/14, such as `@safe_div/%2/DIV.nonzero` |
| `@n/%p/CALL.pre.i` | conjunct i of the callee's PRE at a CALL_HASH |
| `@n/%p/MAP.pre.i`, `@n/%p/GEN.pre.i` | conjunct i of the kernel's PRE for a fresh element or index |
| `@n/%p/FOLD.base`, `@n/%p/FOLD.pre.i`, `@n/%p/FOLD.step` | the three FOLD_BOUNDED obligations (docs/05) |
| `@n/%p/GUARD.then.pre.i`, `@n/%p/GUARD.else.pre.i` | a branch's PRE under its condition |
| `<clause ID>.wd.j` | term j of that conjunct reads no unspecified value: `@n/POST.0.wd.0` (ADR 0010) |

Conjuncts are numbered from 0, left to right, after flattening the top-level `&&` of the clause; several clauses of one kind join in source order first, and refined parameter types join PRE after its explicit clauses, in parameter order (ADR 0007).

## Diagnostics

```ts
type Diagnostic = {
  code: string;                    // "VG-T0101"
  severity: "error" | "warning";
  node?: string;                   // "@soft_knee"
  op?: string;                     // "%4", as written
  span: Span;
  message: string;                 // one sentence, at most 200 characters
  expected?: string;               // "f32"
  found?: string;                  // "i32"
  fix?: { message: string; edits: { span: Span; replace: string }[] };
};
```

**Codes.** This catalog is the minimum every implementation reports; it doubles as the checklist of static rules for the validator, vg-types and the formula checker. New codes are appended; none is ever renumbered.

| Code | Rule |
| --- | --- |
| VG-P0001 | Unexpected token; `expected` lists what the grammar allows at that point |
| VG-P0002 | Unterminated string |
| VG-P0003 | Integer literal does not fit its type |
| VG-P0004 | Reserved word used as a name |
| VG-P0005 | Character that no token can start with |
| VG-P0006 | Decimal literal overflows its float type |
| VG-S0001 | Port assigned twice |
| VG-S0002 | Port used before the statement that defines it (ADR 0005) |
| VG-S0003 | Unknown port or parameter |
| VG-S0004 | Label used before its NODE, SPEC or `@MESH_QUERY` (ADR 0005) |
| VG-S0005 | Unknown label |
| VG-S0006 | Label defined twice |
| VG-S0007 | Recursion or a call cycle; REC_CALL is [ALPHA] |
| VG-S0008 | RETURN missing, repeated, or not the last statement |
| VG-S0101 | Unknown opcode |
| VG-S0102 | Mode missing, unknown, or not allowed for this opcode |
| VG-S0103 | Wrong number of arguments |
| VG-S0104 | Op not available in the MVP |
| VG-S0105 | Named argument unknown, repeated, or before a positional one |
| VG-S0106 | Number of result ports does not match the op |
| VG-S0107 | FOLD_BOUNDED without `invariant=`, or naming no INVARIANT of this node |
| VG-S0108 | An argument that must be a literal is not one (the PROJ index) |
| VG-S0109 | A kernel or callee argument is not a label |
| VG-S0110 | A SPEC node called from a body; use `spec:` in a formula |
| VG-S0201 | .vgir header invalid: magic, version, flags or reserved bytes |
| VG-S0202 | .vgir section unknown, out of order, misaligned or overlapping |
| VG-S0203 | .vgir index out of range |
| VG-S0204 | .vgir not canonical: re-canonicalizing changes bytes, a hash does not match, or a NaN constant is not canonical |
| VG-T0101 | Operand type does not match; `expected` and `found` say how |
| VG-T0102 | Operands of one op have different types |
| VG-T0103 | Mode does not match the operand type, such as LT.u on i32 |
| VG-T0104 | Integer op on floats, or float op on integers |
| VG-T0105 | f16 or bf16 used in arithmetic or comparison; CAST it first |
| VG-T0106 | Kernel signature does not fit the combinator |
| VG-T0107 | Kernel has effects, several results or a type that is not fixed-size |
| VG-T0108 | RETURN does not match the SIGNATURE results |
| VG-T0109 | Call arguments do not match the callee's parameters |
| VG-T0110 | ASSERT_GUARD branches have different signatures |
| VG-T0111 | Condition is not Bool |
| VG-T0112 | Index or length not u64, or shift amount not u32 |
| VG-T0113 | CAST not allowed between these types |
| VG-T0114 | AS_BYTES of a type with no byte form |
| VG-T0115 | Port annotation disagrees with the inferred type, or OK or ERR without one |
| VG-T0116 | Refinement type nested inside another type; the MVP allows it only at the top of SIGNATURE |
| VG-T0117 | LEARNABLE parameter not a float, or a bound that is not finite, or lo above hi |
| VG-T0118 | Static size above the limits of docs/02 |
| VG-T0119 | Eff or a handle where only plain values are allowed |
| VG-T0120 | A returned slice may view a value of the node's own frame; return the fixed-size value instead (docs/13) |
| VG-E0101 | Op or call needs an effect that EFFECTS lacks; `expected` names it |
| VG-E0102 | Eff token used twice or never used |
| VG-E0103 | Effectful node without Eff as its first parameter and first result |
| VG-E0104 | Handle parameter whose effect bit is absent |
| VG-E0105 | Reserved effect bit set |
| VG-E0106 | Op allocates, but ARENA is missing |
| VG-C0101 | Formula type error: an integer term meets a float term, or two float types meet |
| VG-C0102 | Decimal number with no float term to meet |
| VG-C0103 | Unknown built-in, `spec:` or `fold:` name |
| VG-C0104 | `result` outside POST, or `result.j` out of range |
| VG-C0105 | Port referenced outside an INVARIANT |
| VG-C0106 | Unknown resource in COST or BUDGET, or a measured resource in COST |
| VG-C0107 | `spec:` names a node that is not a SPEC |
| VG-C0108 | `fold:` names something that is not a FOLD-shaped kernel, or its arguments do not fit |
| VG-V0001 | Counterexample, reproduced concretely |
| VG-V0002 | Contract gap: refuted using callee or spec values the model chose, which their contracts allow |
| VG-V0003 | Vacuous contract |
| VG-V0004 | Unknown: timeout |
| VG-V0005 | Unknown: a solver answered unknown |
| VG-V0006 | Unknown: the model violates an assumption that was only partly instantiated |
| VG-V0007 | Unknown: the model is too large to replay |
| VG-V0008 | Verifier error: model-guided replay did not reproduce the violation; the build stops |
| VG-V0009 | Verifier error: solvers disagree; the build stops |
| VG-B0001 | Build of a node that is not verified |
| VG-B0002 | Target toolchain missing or not the pinned version |
| VG-B0003 | BUDGET not met by the benchmark |
| VG-M0001 … VG-M0006 | Mesh ingestion check 1 … 6 failed (docs/09) |
| VG-L0001 | Construct outside the lifting subset |
| VG-L0002 | Alive2 refinement check failed |
| VG-L0003 | Differential fuzzing found a mismatch |
| VG-W0001 | Op dropped by canonicalization: no output, effect or invariant uses it |
| VG-W0002 | INVARIANT declared but unused |
| VG-W0003 | Decimal literal not exact in its type; the message gives the exact value |
| VG-W0004 | Residual guard: a plain ASSERT_GUARD whose condition was not proven |

## Values in JSON

One encoding for inputs, outputs, counterexamples and test vectors, so an agent can paste a counterexample straight into `vg_run`.

| Type | JSON |
| --- | --- |
| Bool | `true`, `false` |
| u8 … u32, i8 … i32 | a number |
| u64, i64 | a decimal string, because JSON numbers lose precision above 2^53 |
| floats | a string: the shortest decimal that rounds back to the same value, as Rust's `{:?}` prints it (`"3e38"`, `"0.1"`, `"-0.0"`, `"inf"`, `"-inf"`), except that NaN is written `"nan"` |
| Array, Tensor, Slice, Tuple | a JSON array; tensors nest, row-major |
| Result | `{"ok": v}` or `{"err": e}` |
| Handle | `{"handle": "FileIn", "id": 0}`, with ids issued by the manifest or the effect trace |

A collection longer than 16 elements is shown sparsely in reports: `{"len": 512, "at": {"0": "0.5", "511": "-1.0"}}`, listing the elements the slice or the formula reads, plus the first and the last.

## Tools

`vg mcp` serves eight tools over stdio. Schemas are TypeScript-style; `Value` is the encoding above, `Hash` 64 hex digits.

```ts
// vg_check: parse, validate, canonicalize, type- and effect-check.
type CheckIn  = { source: string } | { path: string };
type CheckOut = { ok: boolean; module: string;
  nodes: { label: string; kind: "node" | "spec" | "mesh_query"; node_hash: Hash;
           signature: string; effects: string[]; obligations: number;
           contract_text: string }[];          // rendered as below
  diagnostics: Diagnostic[] };                 // sorted by line, then column

// vg_verify: prove every obligation, or the ones selected by ID prefix.
type VerifyIn  = ({ source: string } | { path: string }) & { obligations?: string[];
                   mode?: "edit" | "ci" | "release"; timeout_ms?: number; detail?: 0 | 1 | 2 };
type VerifyOut = { ok: boolean; nodes: { label: string; verified: boolean }[];
  results: { id: string; hash: Hash; time_ms: number;
             verdict: "proven" | "counterexample" | "contract_gap" | "vacuous" | "unknown" | "error";
             code?: string;                    // VG-V…, absent when proven
             solver?: string;
             evidence?: "prescreen" | "re-solved" | "two-solvers" | "certificate" | "cached";
             report?: FailureReport }[] };     // sorted by id

// vg_build: compile verified nodes.
type BuildIn  = { path: string; root?: string; targets: string[] };   // LLVM triples (docs/06)
type BuildOut = { artifacts: { target: string; path: string; bytes: number; blake3: Hash }[];
  manifest?: string; residual_guards: { guard: string; obligation: string }[];
  symbol_map: string; guard_map: string; diagnostics: Diagnostic[] };

// vg_run: run a node in vg-interp or an artifact on this host.
type RunIn  = { path: string; node: string; target?: "interp" | string;
                inputs: Record<string, Value>; theta?: Record<string, Value>;
                trace?: { op: string; result: Value }[] };   // effect trace, docs/16
type RunOut = { outputs: Value[]; telemetry: object[];
                trap?: { op: string; obligation: string; inputs: Record<string, Value> } };

type MeshQueryIn  = MeshContractQuery & { limit?: number };              // docs/09
type MeshQueryOut = { candidates: { node_hash: Hash; spec_hash?: Hash; cost?: string;
                      evidence: string; benchmarks: object[] }[] };
type PublishIn  = { path: string };
type PublishOut = { accepted: Hash[]; rejected: { node: string; check: number; code: string; reason: string }[] };
type LiftIn  = { path: string; function: string; language: "c" | "rust" };
type LiftOut = { node_hash: Hash; report: { bounded_check: string; unroll: number;
                 fuzz_cases: number; ub_found: { kind: string; input: Value[] }[] } };
type ExplainIn  = { obligation: string };      // an ID or a hash
type ExplainOut = { report: FailureReport; prose: string };
```

## Failure report

```ts
type FailureReport = {
  node: string; obligation: string; obligation_hash: Hash;
  clause: string;                              // the conjunct, as written
  verdict: string; code: string;
  inputs: Record<string, Value>; theta?: Record<string, Value>;
  step?: number;                               // k, for an INVARIANT that fails at step k
  oracle?: { site: string; values: Value[] }[];  // callee and spec values the model chose
  slice: { port: string; op: string; value: Value }[];
  diff?: string;                               // the weakest condition on the slice that would make it hold
  reproduced: "concrete" | "model" | "no";
};
```

`slice` lists the backward cone of the failed formula, pruned to the ops whose values matter, in canonical order. `reproduced` is `concrete` when vg-interp reproduced the violation with the real callees, and `model` when it did so only with the `oracle` values (a contract gap, docs/05). The first draft of `@soft_knee` gives:

```json
{
  "node": "@soft_knee", "obligation": "@soft_knee/POST.0", "obligation_hash": "<64 hex>",
  "clause": "result >= -1.0", "verdict": "counterexample", "code": "VG-V0001",
  "inputs": { "knee": "0.5", "x": "3e38" },
  "slice": [
    { "port": "%4",  "op": "FDIV(%2, %3)",              "value": "inf" },
    { "port": "%5",  "op": "FADD(1.0:f32, %4)",         "value": "inf" },
    { "port": "%6",  "op": "FDIV(%4, %5)",              "value": "nan" },
    { "port": "%12", "op": "FCLAMP(%11, -1.0:f32, 1.0:f32)", "value": "nan" }
  ],
  "diff": "for |x| > knee, the obligation holds iff %4 is finite",
  "reproduced": "concrete"
}
```

## The contract in plain language

`contract_text` renders each contract in English by fixed templates, so the human confirms what was actually formalized, not the agent's account of it. Clover (Sun et al., 2024) and the user-intent formalization work of Lahiri (FMCAD 2024) both show that an AI-written specification must be checked against intent separately from checking code against the specification; R-14 studies how far rendering and solver-chosen examples go.

| Construct | Rendering |
| --- | --- |
| `a && b`, `a \|\| b`, `!a` | "a, and b"; "a, or b"; "not (a)" |
| `a ==> b` | "if a, then b" |
| `forall(i in 0..n, c in 0..2) { φ }` | "for every i from 0 to n − 1 and c from 0 to 1: φ" |
| `x <= 0.99` with x: f32 | "x ≤ 0.99 (the f32 value 0.9900000095367431640625)", whenever the literal is not exact |
| `is_finite(x)`, `is_nan(x)`, `same(a, b)` | "x is finite"; "x is NaN"; "a is identical to b, bit for bit" |
| `spec:sha256(m)`, `fold:f(i, xs, k)` | "the SHA-256 reference spec applied to m"; "f folded over the first k elements of xs, starting from i" |
| `result`, `result.j` | "the result", "result j" |

For `@soft_knee`: "Requires: x is finite, and knee ≥ 0.5, and knee ≤ 0.99 (the f32 value 0.9900000095367431640625). Ensures: the result ≥ −1.0, and the result ≤ 1.0, and (x ≥ 0.0) is true exactly when (the result ≥ 0.0) is true."

## Budgets

Tokens are the largest cost of the project (docs/12), so each tool caps its default output; R-08 tunes the caps against repair rounds.

- `vg_check` returns at most 20 diagnostics, sorted by position, and then the count of the rest.
- `vg_verify` at `detail: 1`, the default, attaches a full report to the first 3 failing obligations in ID order and only the verdict and code to the others; `detail: 2` reports all of them, `detail: 0` none.
- Messages stay under 200 characters; values follow the sparse rule above.

## Incremental checker (Tier 2, P5.4)

VG-Sym is prefix-checkable: ports and labels are defined before they are used (ADR 0005), and each op's types follow from its operands, so any prefix of a module can be checked as far as it goes. vg-decode exposes this to constrained decoders (R-01) as a Rust API:

```rust
pub struct Prefix { /* parser, checker and scope state */ }
impl Prefix {
    pub fn new() -> Prefix;
    /// Feed one lexical token. Err when the prefix can no longer become a valid module.
    pub fn push(&mut self, token: &str) -> Result<(), Diagnostic>;
    /// What may come next without a parse, structure, type or effect error.
    pub fn allowed(&self) -> Allowed;
}
pub struct Allowed {
    pub token_classes: Vec<TokenClass>,   // keywords, punctuation, literal kinds
    pub ports: Vec<(String, Type)>,       // defined ports whose type fits the next operand
    pub labels: Vec<(String, FnType)>,    // defined labels that fit the next kernel or callee
    pub opcodes: Vec<(String, Vec<String>)>,  // opcodes and modes allowed here
}
```

`allowed` must be sound and complete for the static rules: every listed continuation keeps the prefix free of errors, and every continuation it leaves out causes one, the property type-constrained decoding relies on (Mündler et al., PLDI 2025). Its p99 latency budget is 1 ms (docs/07). Tier 3 hooks onto the same state: when a token binds an operand, the checker returns the obligations that just became decidable, such as an INDEX bound once both operands are known.
