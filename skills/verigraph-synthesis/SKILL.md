---
name: verigraph-synthesis
description: Write verified VeriGraph (VG-Sym) programs from plain-language requests, using the vg MCP tools to check, verify, repair, build and publish them.
---

# VeriGraph synthesis

You turn a plain-language request into VG-Sym nodes that the VeriGraph toolchain proves correct and compiles to native code. You never ship a node that `vg_verify` has not proven. This skill describes the language as specified in docs/03-vg-sym.md; the build agents keep it in sync with what the toolchain implements, and CI checks every example below.

## Workflow

1. **Contract card first.** Before any code, write the card below. Once the contract parses, show the user the `contract_text` that `vg_check` renders, and get their confirmation of that text: it is what will be proven, word for word.
2. **Mesh first.** For hashing, codecs, parsing, sorting or linear algebra, call `vg_mesh_query` with a type signature, the allowed effects and a POST. Bind what it finds with `@MESH_QUERY` blocks.
3. **Draft.** Pure kernels first; effects only in a thin root node.
4. **Check.** `vg_check` until clean. Every diagnostic has a stable code (docs/15-agent-interface.md); apply its `fix` when it carries one.
5. **Verify.** `vg_verify`, then act on each verdict:
   - `counterexample`: real inputs break the obligation. Read the slice and the diff, fix the cause rather than the symptom, and verify again.
   - `contract_gap`: the obligation does not follow from a callee's contract, though the callee may behave. Strengthen that callee's POST (or this node's PRE), not the body.
   - `unknown`: nothing was refuted or proven. Split the obligation, add an INVARIANT conjunct, or guard the op.
   - `vacuous`: no input satisfies PRE. Weaken it.
   Never weaken a POST to pass unless the user agrees it was wrong.
6. **Escalate.** After 5 rounds without progress on one obligation, change approach (another algorithm, or a guard with a verified fallback) or ask the user.
7. **Ship.** `vg_build` for the requested targets, `vg_run` the tests and benchmarks, and `vg_mesh_publish` when asked.

## Contract card

```text
Purpose:      one sentence
Inputs:       name: Type, ...
Outputs:      Type, ...
Effects:      [] unless the request needs I/O, clock, network or GPU
PRE:          what is assumed where data enters
POST:         functional facts plus relational facts (sign, order, equality to a spec)
COST:         provable resources (heap_bytes, arena_bytes, stack_bytes, ops)
BUDGET:       measured targets (latency_p99_us), never claimed as proven
LEARNABLE:    parameter, box [lo, hi], loss (if any)
Mesh queries: sub-problems you will look up instead of writing
```

## Rules

- Labels only (`@name`). Never write or guess a hash.
- Single-assignment ports; outputs only through `RETURN`; no inline expressions.
- Define before use: a port only after the statement that assigns it, a label only after its NODE, SPEC or `@MESH_QUERY`. Kernels first, root last.
- Every integer op names its mode (`.checked`, `.wrap`, `.sat`) and every ordered comparison its kind (`.s`, `.u`, `.f`, matching the operand type). Nothing has a default.
- A port may carry its type (`%3: u32 = …`) so the checker confirms what you meant; OK and ERR need it: `%4: Result<u8, u32> = ERR(1:u32);`.
- Kernels capture nothing; pass broadcast values explicitly. Positional arguments first (kernel, collection, broadcasts), named ones last (`init=`, `invariant=`, `else=`).
- Every FOLD_BOUNDED names an INVARIANT; every REC_CALL has DECREASES.
- Use `.checked` arithmetic unless wrapping or saturation is the intended behavior.
- The graph is strict: SELECT evaluates both inputs. Make operands safe before dividing, then select. When no safe operand exists (an INDEX into a slice that may be empty), put the step in a kernel and call it through `ASSERT_GUARD.branch(c, @step, args…, else=@other)`, which runs only the branch it takes. Plain `ASSERT_GUARD` is for a safety check whose else branch means failure: it counts as residual cost and reports at runtime.
- Floats: NaN and ±inf exist. State finiteness in PRE where inputs enter, and prefer forms that stay finite at the extremes (1 − 1/(1+t), not t/(1+t)). In formulas `==` is IEEE equality (NaN equals nothing, −0.0 equals 0.0); use `same(a, b)` for identity. FCOPYSIGN needs a sign operand that is not NaN.
- Strong POSTs: a range check alone lets wrong answers through; add relational facts. For a fold, the exact invariant `acc == fold:step(init, xs, k, …)` is inductive once `@step`'s POST pins `result`; add only the extra facts the POST needs.
- Well-defined contracts: guard every division, index, `ok`/`err` and `spec:` call that might be undefined, as in `d != 0 ==> result == x / d`; `vg_verify` checks this (`.wd` obligations).
- A returned slice must view an input or arena memory; to return bytes of a value computed in the node, return the value itself.
- Effects: the smallest set; handles only from the root; `[]` (pure) for everything that can be.
- LEARNABLE parameters: declare the box; every contract must hold over the whole box.

## Patterns

**Fold with an invariant.** `@count_above` proves only a bound. `@count_above_exact` also states the exact count, because `fold:count_step(0:u32, xs, k, thr)` names what the fold has computed after k steps.

```text
NODE @count_step {
  SIGNATURE: (acc: u32, x: f32, thr: f32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  acc < 4294967295;
    POST: (x > thr ==> result == acc + 1) && (!(x > thr) ==> result == acc);
  }
  BODY {
    %0 = GT.f(x, thr);
    %1 = SELECT(%0, 1:u32, 0:u32);
    %2 = ADD.checked(acc, %1);
    RETURN(%2);
  }
}

NODE @count_above [intent: "Count samples above a threshold"] {
  SIGNATURE: (xs: Slice<f32>, thr: f32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  len(xs) < 4294967295;
    POST: result <= len(xs);
    INVARIANT @at_most_k(k, acc): acc <= k;
  }
  BODY {
    %0 = FOLD_BOUNDED(@count_step, xs, thr, init=0:u32, invariant=@at_most_k);
    RETURN(%0);
  }
}

NODE @count_above_exact [intent: "Count samples above a threshold, exactly"] {
  SIGNATURE: (xs: Slice<f32>, thr: f32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  len(xs) < 4294967295;
    POST: result == fold:count_step(0:u32, xs, len(xs), thr) && result <= len(xs);
    INVARIANT @counted(k, acc): acc == fold:count_step(0:u32, xs, k, thr) && acc <= k;
  }
  BODY {
    %0 = FOLD_BOUNDED(@count_step, xs, thr, init=0:u32, invariant=@counted);
    RETURN(%0);
  }
}
```

**Safe division in a strict graph**

```text
NODE @safe_div {
  SIGNATURE: (x: i32, d: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {
    PRE:  !(x == -2147483648 && d == -1);
    POST: (d == 0 ==> result == 0) && (d != 0 ==> result == x / d);
  }
  BODY {
    %0 = NE(d, 0:i32);
    %1 = SELECT(%0, d, 1:i32);       // the divisor is never 0
    %2 = DIV.checked(x, %1);         // obligations: %1 != 0, and not MIN / -1
    %3 = SELECT(%0, %2, 0:i32);
    RETURN(%3);
  }
}
```

**Guarded step in a strict graph.** No dummy index is safe for an empty slice, so the INDEX goes in a kernel that `ASSERT_GUARD.branch` calls only when the slice is non-empty; the kernel's PRE is checked under that condition. An empty slice is a normal input here, so this is a branch, not a safety guard.

```text
NODE @first {
  SIGNATURE: (xs: Slice<u32>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  len(xs) > 0;
    POST: result == xs[0];
  }
  BODY {
    %0 = INDEX(xs, 0:u64);           // obligation 0 < len(xs), from PRE
    RETURN(%0);
  }
}

NODE @zero {
  SIGNATURE: (xs: Slice<u32>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    POST: result == 0;
  }
  BODY {
    RETURN(0:u32);
  }
}

NODE @first_or_zero [intent: "First element of a slice, or 0 when it is empty"] {
  SIGNATURE: (xs: Slice<u32>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    POST: (len(xs) > 0 ==> result == xs[0]) && (len(xs) == 0 ==> result == 0);
  }
  BODY {
    %0 = LEN(xs);
    %1 = GT.u(%0, 0:u64);
    %2 = ASSERT_GUARD.branch(%1, @first, xs, else=@zero);   // @first runs only when %1 holds
    RETURN(%2);
  }
}
```

**Errors as values.** Build both outcomes, then select; GET_OK and GET_ERR need an IS_OK test first, through `ASSERT_GUARD.branch` and a kernel whose PRE is `is_ok(r)`.

```text
NODE @parse_percent [intent: "Accept a percentage from 0 to 100, or error code 1"] {
  SIGNATURE: (b: u8) -> (Result<u8, u32>);
  EFFECTS: [];
  CONTRACT {
    POST: (b <= 100 ==> is_ok(result) && ok(result) == b)
       && (b > 100 ==> !is_ok(result) && err(result) == 1);
  }
  BODY {
    %0 = LE.u(b, 100:u8);
    %1: Result<u8, u32> = OK(b);
    %2: Result<u8, u32> = ERR(1:u32);
    %3 = SELECT(%0, %1, %2);
    RETURN(%3);
  }
}
```

**Map with a pure kernel, plus a Mesh query**: see examples/audio_limiter.vgs.

**Validated entry:** the root node is the only place with effects and handles. It reads external data, checks it against PRE with explicit ops, and returns `Result` instead of trusting the input.

## Tools

`vg mcp` serves all eight tools of docs/15-agent-interface.md over stdio (also available on the command line; see docs/guide/mcp.md and docs/guide/cli.md):

- `vg_check`: takes `source` or `path`; returns `ok`, `module`, `nodes` (`label`, `kind`, `node_hash`, `signature`, `effects`, `obligations`, `contract_text`), and up to 20 `diagnostics` sorted by `(line, col)` with `span: {line, col, len}`. Always show `contract_text` to the user for confirmation before proving or shipping.
- `vg_verify`: takes `source` or `path`, plus optional `mode` (`"edit"` 2 s, `"ci"` 60 s, `"release"` 60 s with two independent solvers), `timeout_ms`, and `detail` (`0`, `1` default, `2`). Proves functional, domain, combinator, well-definedness (`.wd`), non-vacuity, and static `COST` obligations (`ops`, `arena_bytes`, `heap_bytes`), replaying counterexamples in `vg-interp` and attaching `FailureReport` slices and diffs.
- `vg_explain`: takes `source` or `path` and `obligation` (such as `@soft_knee/POST.0`); returns the obligation's `FailureReport` and a plain-language `prose` explanation.
- `vg_mesh_query` and `vg_mesh_publish`: query verified nodes in the project's Mesh registry by contract (`input_types`, `output_types`, `max_effects`, `pre`, `post`, `max_cost`, `intent`, `spec`, `limit`), and publish verified modules through the six-check ingestion gate. Pin resolved `@MESH_QUERY` bindings in `vg.lock` (`vg mesh resolve`).
- `vg_run`: runs a node in `vg-interp` on docs/15 JSON `inputs` keyed by parameter name and checks `pre` and `post`.
- `vg_build`: verifies the root closure first (never builds an unproven node over MCP) and compiles native/WASM artifacts (`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `wasm32-wasip1`) along with `<label>.manifest.toml`.
- `vg_lift`: lifts a C or Rust function (`path`, `function`, `language`) into VG-Sym via OmniLift with Alive2 bounded refinement checking and differential fuzzing.
