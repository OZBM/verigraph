# Tutorial

This tutorial writes one node, lets the verifier find its bug, repairs it, then runs and compiles it.
It takes about ten minutes. You need `vg` and z3 ([Installation](install.md)).

## 1. Write a node

Save this module as `safe_div.vgs`:

```vgsym
// expect: reject @safe_div/%0/DIV.nonzero
module tutorial;

NODE @safe_div [intent: "Divide two u32"] {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    POST: result <= a;
  }
  BODY {
    %0 = DIV.checked(a, b);
    RETURN(%0);
  }
}
```

- `module tutorial;` names the module. A module holds nodes, written before they are used.
- `NODE @safe_div` defines a node. Labels start with `@`; the toolchain computes every hash.
- `SIGNATURE` gives the parameters and the result types. `EFFECTS: []` says the node is pure.
- `CONTRACT` holds the clauses to prove. `POST` speaks about the parameters and `result`.
- `BODY` is a list of single-assignment statements. `%0` is a port; `DIV.checked` is the DIV op in
  its checked mode, which every integer op must name. `RETURN` gives the results.

## 2. Check it

```bash
vg check safe_div.vgs
```

```json
{
  "diagnostics": [],
  "nodes": [
    {
      "cost_ops": 1,
      "effects": [],
      "kind": "node",
      "label": "@safe_div",
      "node_hash": "0184a4e7c65f7801c369245a78a70ff6ddc2899719080a7795ef79f9481b35c3"
    }
  ],
  "ok": true
}
```

`vg check` parses the module, canonicalizes each node, computes its hash, and checks its types and
effects. It does not prove anything yet.

## 3. Verify it

```bash
vg verify safe_div.vgs
```

```json
{
  "nodes": [{ "label": "@safe_div", "verified": false }],
  "ok": false,
  "results": [
    {
      "code": "VG-V0001",
      "id": "@safe_div/%0/DIV.nonzero",
      "message": "refuted by the solver's model",
      "report": {
        "inputs": { "a": 0, "b": 0 },
        "node": "@safe_div",
        "obligation": "@safe_div/%0/DIV.nonzero",
        "reproduced": "concrete"
      },
      "solver": "z3",
      "verdict": "counterexample"
    },
    { "id": "@safe_div/NONVACUOUS", "message": "satisfiable", "solver": "z3", "verdict": "proven" },
    {
      "code": "VG-V0001",
      "id": "@safe_div/POST.0",
      "message": "refuted by the solver's model",
      "report": {
        "inputs": { "a": 4294967294, "b": 0 },
        "node": "@safe_div",
        "obligation": "@safe_div/POST.0",
        "reproduced": "concrete"
      },
      "solver": "z3",
      "verdict": "counterexample"
    }
  ]
}
```

Each result is one **obligation**, a goal the solver must prove:

- `@safe_div/%0/DIV.nonzero`: the op at port `%0` must not divide by zero. The solver found `b = 0`.
- `@safe_div/NONVACUOUS`: some input satisfies PRE, so the contract is not vacuous. Proven.
- `@safe_div/POST.0`: the first conjunct of POST. With `b = 0` the division traps, so POST cannot hold.

`"reproduced": "concrete"` means vg-interp ran the node on those inputs and saw it fail: this is a
real bug, not an artifact of the solver.

## 4. Repair it

The node needs `b != 0`. Callers must then guarantee it, so it belongs in PRE. While you are at it, say
more about the result:

```vgsym
// expect: verify
module tutorial;

NODE @safe_div [intent: "Divide two u32"] {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  b != 0;
    POST: result <= a && (b == 1 ==> result == a);
  }
  BODY {
    %0 = DIV.checked(a, b);
    RETURN(%0);
  }
}
```

`vg verify safe_div.vgs` now proves four obligations, `POST.0` and `POST.1` being the two conjuncts of
POST, and ends with `"ok": true`.

A POST of `result == a / b` would be true too, but z3 times out on it (`unknown`, VG-V0004): integer
division in a contract is hard for the solver. [Verification](verification.md) explains how to handle
an `unknown`.

## 5. Run it

```bash
vg run safe_div.vgs @safe_div '{"a": 7, "b": 2}'
```

```json
{ "ok": true, "outputs": [3], "post": true, "pre": true }
```

Inputs are a JSON object keyed by parameter name. Integers up to 32 bits are JSON numbers; 64-bit
integers and floats are strings ([Command-line reference](cli.md#json-values)). The result says
whether PRE held on the inputs and POST on the outputs. Breaking PRE shows what the contract protects
against:

```bash
vg run safe_div.vgs @safe_div '{"a": 7, "b": 0}'
```

```json
{
  "ok": false,
  "pre": false,
  "trap": { "message": "DIV.nonzero(u32): division of 7 by 0", "op": 0, "opcode": "DIV" }
}
```

## 6. Build it and call the binary

```bash
vg build safe_div.vgs @safe_div -o target/vg
```

```json
{
  "artifacts": [
    {
      "blake3": "cc69a5c8f3ebc733f33fc4e3079da684658e3aaf2f754e16a77de57556fca3d0",
      "bytes": 37120,
      "linked": true,
      "path": "target/vg/safe_div-aarch64-apple-darwin",
      "residual_guards": 0,
      "target": "aarch64-apple-darwin"
    }
  ],
  "ok": true,
  "verified": true
}
```

`vg build` verifies the node and everything it calls, then compiles it. Since the node is proven, the
binary carries no runtime checks. It reads each parameter from stdin as little-endian bytes and writes
the results the same way:

```bash
printf '\007\000\000\000\002\000\000\000' | target/vg/safe_div-aarch64-apple-darwin | xxd
```

```text
00000000: 0300 0000                                ....
```

That is 7 / 2 = 3 as a little-endian u32. The executable checks PRE on entry: with `b = 0` it writes
nothing and exits with code 1. [Command-line reference](cli.md#built-executables) lists the exit codes.

## 7. The same in Antigravity IDE

With the [Antigravity integration](antigravity.md) installed, open `safe_div.vgs`. `vg check` runs
when the file opens and on every save; the ✓ button in the editor title verifies the file, and the
draft's two counterexamples appear as errors on the `NODE @safe_div` line, with their inputs. The ▶
button runs a node, and the package button builds it. You can also ask the agent: *"Verify
safe_div.vgs and repair what fails"*; it calls the same toolchain through the `vg_verify` tool.

## 8. Next

`examples/soft_knee_draft.vgs` is a harder case: a float audio limiter whose POST fails for huge
inputs, where an intermediate value overflows to infinity and the result becomes NaN.

```bash
vg verify examples/soft_knee_draft.vgs
```

The three POST conjuncts come back with concrete counterexamples, such as `x = -2.510954e38`.
`examples/quickstart.vgs` holds the repaired node, and docs/03-vg-sym.md tells the whole story. Then
read [The VG-Sym language](language.md) and [Verification](verification.md).
