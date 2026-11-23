# Verification

`vg verify` proves that every node of a module keeps its contract on every input that satisfies its
PRE. This chapter explains what it proves, how to read its answers and what to do about each one.
docs/05-contracts-verification.md and ADRs 0006 to 0010 are the normative rules.

## How it works

For each NODE (SPECs are references, not obligations), the verifier lists **obligations**: one goal
per POST conjunct, per op that can fail, per call, per combinator, plus the non-vacuity of PRE. Each
becomes an SMT query, with integers as bit-vectors, floats as IEEE floating point, and collections as
arrays. A solver runs as a separate process and must answer `unsat` for the goal's negation; that
answer, and only that one, is a proof. A timeout or an `unknown` is never a proof. z3 takes most
queries and Bitwuzla the queries with floats; in release mode (`vg verify --mode release`) a second
solver must answer `unsat` too, and if it answers `sat` instead the verdict is `error` (VG-V0009)
and the build stops. cvc5 takes only 32- and 64-bit floats and Bitwuzla no `bf16`, so a release
proof about `bf16`, which only z3 takes, stays `unknown`.

Before any solver runs, a **pre-screen** runs the node in vg-interp on a fixed set of inputs: boundary
values (0, ±1, MIN, MAX, −0.0, subnormals, ±∞, NaN, the literals PRE compares against, slice lengths 0
to 3 and 16), then 256 random combinations seeded from the node's hash, so every machine tries the same
inputs. An input that meets PRE and then traps or breaks a POST conjunct is a counterexample at once
(`"evidence": "prescreen"`), and the first input that meets PRE shows the contract is not vacuous. A
pass proves nothing; the solvers still decide every other obligation. Nodes with effects, and inputs
whose static op count is over 20,000, are not pre-screened.

Each proof is kept as a record in `<root>/.vg/proofs`, named by the obligation's hash, so an
unchanged obligation is not solved again (`"evidence": "cached"`). A record is used only while the
solvers it names report the same version, and in release mode only when two solvers made it. The
records are trusted like the build directory: delete `.vg/proofs` to prove everything again.

Nodes are verified one at a time. A call is reasoned about through the callee's contract only, never
its body, so a node's proof stays valid when a callee's body changes and its contract does not. Inside
a node, each op's obligations see only the facts of the ops before it.

## Obligations

| ID | Goal |
| --- | --- |
| `@n/POST.i` | Conjunct i of POST holds on the results. |
| `@n/NONVACUOUS` | Some input satisfies PRE (and the refinements and LEARNABLE box). |
| `@n/%p/<OP>.<kind>` | The op at port p cannot fail: `DIV.nonzero`, `ADD.no_overflow`, `INDEX.in_bounds`, `CAST.in_range`, `FCLAMP.bounds`, `FCOPYSIGN.sign`, … (docs/14). |
| `@n/%p/CALL.pre.i` | Conjunct i of the callee's PRE holds at the call. |
| `@n/%p/MAP.pre.i`, `@n/%p/GEN.pre.i` | The kernel's PRE holds for any element or index. |
| `@n/%p/FOLD.base`, `FOLD.pre.i`, `FOLD.step` | The invariant holds at step 0; it implies the kernel's PRE; each step keeps it. |
| `@n/%p/GUARD.then.pre.i`, `GUARD.else.pre.i` | Each branch's PRE holds under its condition. |
| `<clause>.wd.j` | Term j of that conjunct is well defined: `@n/POST.0.wd.0`. |
| `@n/COST.i` | Conjunct i of COST. Version 0 reports it as `unknown`. |

Conjuncts are numbered from 0 after flattening the top-level `&&` of all clauses of one kind, in
source order. In version 0, `%p` is the op's index in canonical order, which can differ from the port
you wrote (see [Troubleshooting](troubleshooting.md)).

## Verdicts

| Verdict | Code | Meaning | What to do |
| --- | --- | --- | --- |
| `proven` | | The solver proved the goal. | Nothing. |
| `counterexample` | VG-V0001 | An input satisfies PRE and breaks the goal, and vg-interp reproduced the failure with the real callees (`"reproduced": "concrete"`). | Fix the body, or add the missing assumption to PRE. |
| `contract_gap` | VG-V0002 | The goal fails with callee results that the callee's contract allows, but the real callee behaves. | Strengthen the callee's POST, or this node's PRE. Not the body. |
| `vacuous` | VG-V0003 | No input satisfies PRE, so every other goal holds trivially. | Weaken PRE. |
| `unknown` | VG-V0004 | The solver timed out. | Raise `--timeout`, split the conjunct, add an invariant conjunct, or restate the goal. |
| `unknown` | VG-V0005 | The solver answered `unknown`. | As for a timeout. |
| `unknown` | VG-V0006 | The solver's model breaks PRE where the query only instantiates it in part (a `forall`, `spec:`, `fold:` or a formula division): the solver found no input that really meets PRE. | Split the obligation, add an INVARIANT conjunct, or guard the op. |
| `unknown` | VG-V0007 | The model cannot be replayed: it is too large, or the node has effects. | Simplify the contract. |
| `error` | VG-V0008 | A verifier error: for instance, a model that does not reproduce in a node with nothing abstracted, where the query encodes the node exactly. | Report it, with the module. |
| `error` | VG-V0009 | Release mode: the second solver contradicts the first. | Report it, with the module. |
| `unknown` | VG-V0005 | Release mode: the second solver did not confirm the proof, or no second solver takes the query. | Install cvc5 and Bitwuzla, raise `--timeout`, or avoid `bf16` in the goal. |

A node is `verified` only when all its obligations are proven, and `"ok": true` only when every node
is. Never weaken a POST to make it pass unless you agree that it was wrong.

## Reading a counterexample

A refuted obligation carries a report:

```json
"report": {
  "inputs": { "a": 0, "b": 0 },
  "node": "@safe_div",
  "obligation": "@safe_div/%0/DIV.nonzero",
  "reproduced": "concrete"
}
```

The inputs use the JSON value forms of [the command-line reference](cli.md#json-values), so you can
paste them into `vg run` to watch the failure:

```bash
vg run safe_div.vgs @safe_div '{"a": 0, "b": 0}'
```

Every `counterexample` is reproduced: the verifier replays each model in vg-interp before it reports
one, and a model that does not reproduce becomes a `contract_gap`, an `unknown` or an `error`
instead. When an input slice is involved, the verifier first looks for a model whose slices have at
most 4, then 16, then 256 elements, so counterexamples stay small.

## Contract gaps

`@add2` calls `@inc` twice. `@inc` adds one, but its contract only promises `result > x`:

```vgsym
// expect: contract_gap @add2/POST.0
module gap;

NODE @inc {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  x < 1000;
    POST: result > x;
  }
  BODY {
    %0 = ADD.checked(x, 1:u32);
    RETURN(%0);
  }
}

NODE @add2 {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  x < 100;
    POST: result == x + 2;
  }
  BODY {
    %0 = CALL_HASH(@inc, x);
    %1 = CALL_HASH(@inc, %0);
    RETURN(%1);
  }
}
```

The verifier may pick `inc(x) = x + 5`, which the contract allows, so `@add2/POST.0` is refuted; the
replay with the real `@inc` gives `x + 2`, so the verdict is `contract_gap`, not a bug in `@add2`.
`@add2/%1/CALL.pre.0` is a gap for the same reason: nothing says the first result stays below 1000.
The fix is in the callee's contract:

```vgsym
// expect: verify
module gap_fixed;

NODE @inc {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  x < 1000;
    POST: result == x + 1;
  }
  BODY {
    %0 = ADD.checked(x, 1:u32);
    RETURN(%0);
  }
}

NODE @add2 {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  x < 100;
    POST: result == x + 2;
  }
  BODY {
    %0 = CALL_HASH(@inc, x);
    %1 = CALL_HASH(@inc, %0);
    RETURN(%1);
  }
}
```

## Strong contracts

- A range check alone lets wrong answers through. Add relational facts: sign, order, equality to a
  SPEC.
- For a fold, `acc == fold:step(init, xs, k, …)` is always inductive; add only the facts the POST
  needs.
- Floats have NaN and infinities. State finiteness in PRE where data enters, and prefer forms that
  stay finite at the extremes: `1 − 1/(1 + t)` rather than `t/(1 + t)`. That is the repair of the
  soft-knee story in docs/03.
- Guard every term that may be undefined: `d != 0 ==> result == x / d`.

## When the solver says unknown

- Nonlinear integer arithmetic is hard for SMT solvers. `result == a / b` for a u32 division times
  out; a weaker but useful fact such as `result <= a && (b == 1 ==> result == a)` proves at once.
- Split a long conjunct into several, so each gets its own query and timeout.
- Give the solver the step it misses as an extra conjunct or invariant fact.
- Raise the timeout: `vg verify FILE --timeout 60000` (milliseconds per obligation; default 10000).

## Verified builds

`vg build` verifies the root node and everything it calls, then compiles it:

- When every obligation is proven, the build uses the proven facts to optimize and emits **no
  runtime checks**.
- Otherwise it refuses. With `--unverified`, it builds anyway with a **runtime check on every partial
  op**; a failing check exits with code 4. The MCP tool `vg_build` never builds an unverified node.

## Limits of version 0

- COST obligations are proven for `ops` and `arena_bytes`, but a callee's `ops` cost is not modelled,
  so a COST bound that depends on one stays `unknown` (VG-V0005).
- Model-guided replay is partial: a model is replayed with the real callees only, so in a node that
  calls something, a model that does not reproduce is reported as a `contract_gap` even when an
  encoding bug is the cause. In a node with nothing abstracted it is reported as VG-V0008.
- The pre-screen does not yet check FOLD_BOUNDED invariants step by step; the solver checks them.
- Obligation IDs name ops by canonical index rather than by the port written in the source.
