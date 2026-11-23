# Verification

`vg verify` proves that every node of a module keeps its contract on every input that satisfies its
PRE. This chapter explains what it proves, how to read its answers and what to do about each one.
docs/05-contracts-verification.md and ADRs 0006 to 0010 are the normative rules.

## How it works

For each NODE (SPECs are references, not obligations), the verifier lists **obligations**: one goal
per POST conjunct, per op that can fail, per call, per combinator, plus the non-vacuity of PRE. Each
becomes an SMT query, with integers as bit-vectors, floats as IEEE floating point, and collections as
arrays. z3 runs as a separate process and must answer `unsat` for the goal's negation; that answer,
and only that one, is a proof. A timeout or an `unknown` is never a proof.

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
| `counterexample` | VG-V0001 | An input satisfies PRE and breaks the goal. With `"reproduced": "concrete"`, vg-interp ran the node on it and saw the failure. | Fix the body, or add the missing assumption to PRE. |
| `contract_gap` | VG-V0002 | The goal fails with callee results that the callee's contract allows, but the real callee behaves. | Strengthen the callee's POST, or this node's PRE. Not the body. |
| `vacuous` | VG-V0003 | No input satisfies PRE, so every other goal holds trivially. | Weaken PRE. |
| `unknown` | VG-V0004 | The solver timed out. | Raise `--timeout`, split the conjunct, add an invariant conjunct, or restate the goal. |
| `unknown` | VG-V0005 | The solver answered `unknown`. | As for a timeout. |
| `unknown` | VG-V0006, VG-V0007 | The model could not be used: an assumption was only partly instantiated, or the model is too large to replay. | Simplify the contract. |
| `error` | VG-V0008 | A verifier error. | Report it, with the module. |

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

`"reproduced": "no"` on a counterexample means the replay could not run, which happens for effectful
nodes: treat it as a strong hint, not a demonstration.

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

- COST obligations are reported as `unknown` (VG-V0005, "not encoded yet").
- z3 is the only solver; cvc5 and Bitwuzla, the small-model search and the proof cache are not there
  yet, so every run proves everything again.
- Obligation IDs name ops by canonical index rather than by the port written in the source.
