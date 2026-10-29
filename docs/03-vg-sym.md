# 03 · VG-Sym, the text projection

VG-Sym is the text that AI agents write and humans review. `vg asm` turns it into canonical VG-IR, and `vg disasm` turns any node back into VG-Sym, so the binary graph stays the source of truth.

## Authoring rules

- Name things with labels such as `@soft_knee`; never write a hash. `vg asm` computes every hash and pins each resolved reference in `vg.lock`.
- Ports are single-assignment: each `%n` is assigned once, and outputs leave only through `RETURN`.
- **Define before use** (ADR 0005): a port is used only after the statement that assigns it, and a label only after its NODE, SPEC or `@MESH_QUERY`. Write kernels first and the root last; then any prefix of the file can be checked.
- A port may carry its type: `%3: Result<u8, u32> = OK(b);`. The checker compares it with the inferred type; OK and ERR require it (ADR 0004).
- Every argument is a port, a parameter name, a typed literal (`1.0:f32`, `[1, 2]:Array<u8, 2>`) or a label. Conditions are ops too; there are no inline expressions.
- Kernels passed to SIMD_MAP or FOLD_BOUNDED capture nothing implicitly. Extra values are explicit broadcast arguments.
- Every FOLD_BOUNDED names an INVARIANT, and every REC_CALL has a DECREASES clause.
- Contracts may use bounded `forall` over indices; the combinator proof rules discharge them without quantifier search.
- PRE, POST, COST and BUDGET speak about parameters and `result` only; an INVARIANT may also name ports of the body, and `fold:` states what a fold has computed so far (ADR 0008).
- Several clauses of one kind form one conjunction in source order, and its top-level conjuncts are numbered from 0: `@soft_knee/POST.0` is the first (docs/15).
- `//` starts a comment that runs to the end of the line. Comments are dropped by `vg asm` (kept in DEBUG with `--keep-debug`).

## Grammar

```ebnf
Module     ::= "module" Ident ";" Item*
Item       ::= MeshQuery | NodeDef | SpecDef
MeshQuery  ::= "@MESH_QUERY" Label "{" "signature" ":" FnType ";" "effects" ":" Effects ";"
               ("pre" ":" Formula ";")? "post" ":" Formula ";" ("intent" ":" String ";")? "}"
SpecDef    ::= "SPEC" Label NodeBody          (* executable reference spec *)
NodeDef    ::= "NODE" Label Attrs? NodeBody
NodeBody   ::= "{" "SIGNATURE" ":" Params "->" Results ";"
                   "EFFECTS" ":" Effects ";"
                   "CONTRACT" "{" Clause* "}"
                   Learnable?
                   "BODY" "{" Stmt+ "}" "}"
Attrs      ::= "[" Attr ("," Attr)* "]"
Attr       ::= "intent" ":" String | "export" | "inline"
Params     ::= "(" (Ident ":" Type ("," Ident ":" Type)*)? ")"
Results    ::= "(" Type ("," Type)* ")"
FnType     ::= "(" (Type ("," Type)*)? ")" "->" Results
Clause     ::= "PRE" ":" Formula ";" | "POST" ":" Formula ";"
             | "INVARIANT" Label "(" Ident "," Ident ")" ":" Formula ";"
             | "DECREASES" ":" Formula ";" | "COST" ":" Formula ";" | "BUDGET" ":" Formula ";"
Learnable  ::= "LEARNABLE" "{" (Ident ":" FloatType "in" "[" Signed "," Signed "]"
               ("loss" ":" Label)? ";")+ "}"      (* a loss is needed once a tuner exists *)
Stmt       ::= Binding ("," Binding)* "=" Op "(" Args? ")" ";"
             | "RETURN" "(" Value ("," Value)* ")" ";"
Binding    ::= Port (":" Type)?                 (* optional type annotation, ADR 0004 *)
Op         ::= (OpName | Effect) ("." Mode)?      (* FILE_READ is an effect and an op *)
Mode       ::= Ident | ScalarType   (* checked, wrap, sat, s, u, f, branch, or CAST.u32; the validator checks it *)
Args       ::= Arg ("," Arg)*
Arg        ::= (Ident "=")? (Value | Label)
Value      ::= Port | Ident | Literal
Port       ::= "%" Int                          (* one token: %12 *)
Label      ::= "@" Ident                        (* one token: @soft_knee *)
Literal    ::= Signed ":" ScalarType | "true" | "false" | Array ":" Type
Signed     ::= "-"? Number
Array      ::= "[" Element ("," Element)* "]"  (* row-major: [[1, 2], [3, 4]] *)
Element    ::= Signed | "true" | "false" | Array
Effects    ::= "[" (Effect ("," Effect)*)? "]"     (* [] means PURE *)
Effect     ::= "ARENA" | "CLOCK" | "FILE_READ" | "FILE_WRITE" | "NET_READ" | "NET_WRITE"
             | "GPU" | "NONDET" | "PARAM_UPDATE" | "TELEMETRY" | "HOST_CALL" | "SPAWN"
Type       ::= ScalarType | "Bool" | "Eff" | Handle
             | ("Array" | "Tensor") "<" Type "," Shape ">" | "Slice" "<" Type ">"
             | "Tuple" "<" Type ("," Type)* ">" | "Result" "<" Type "," Type ">"
             | "Arena" "<" Int ">" | "{" Ident ":" Type "|" Formula "}"
Shape      ::= Dim | "[" Dim ("," Dim)* "]"
Dim        ::= Int | Ident
Handle     ::= "FileIn" | "FileOut" | "Socket" | "Clock" | "GpuQueue"
ScalarType ::= IntType | FloatType
IntType    ::= "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64"
FloatType  ::= "f16" | "bf16" | "f32" | "f64"
Formula    ::= Or ("==>" Formula)?
Or         ::= And ("||" And)*
And        ::= Cmp ("&&" Cmp)*
Cmp        ::= Sum (("==" | "!=" | "<" | "<=" | ">" | ">=") Sum)?
Sum        ::= Prod (("+" | "-") Prod)*
Prod       ::= Unary (("*" | "/" | "%") Unary)*
Unary      ::= ("!" | "-") Unary | Postfix
Postfix    ::= Primary ("[" Formula ("," Formula)* "]" | "." Int)*
Primary    ::= Value | Number | "result" | Call | "(" Formula ")"
             | "forall" "(" Range ("," Range)* ")" "{" Formula "}"
Range      ::= Ident "in" Sum ".." Sum
Call       ::= (("spec" | "fold") ":")? Ident "(" (Formula ("," Formula)*)? ")"
```

Built-in formula functions: `len`, `is_finite`, `is_nan`, `abs`, `min`, `max`, `as_bytes`, `same`, `is_ok`, `ok`, `err`. `spec:s(…)` calls the SPEC node `@s`, and `fold:f(init, xs, k, b…)` is the fold of kernel `@f` over the first k elements of xs (ADR 0008); docs/13 defines every built-in, and floats compare as IEEE values under `==`, while `same` compares identity. COST and BUDGET clauses use the resource names `heap_bytes`, `arena_bytes`, `stack_bytes`, `ops` and `latency_p99_us`. In a MeshQuery, unnamed parameters are `arg0`, `arg1`, and so on. In an INVARIANT, the two names are the step count and the accumulator. CAST names its target type as its mode: `%1 = CAST.u32(x);`.

**Formula types.** Integer terms (integer values, `len(x)`, range variables, integer numbers) are mathematical integers, so `acc + 1` never wraps; `len(x)` is a natural number below 2^64. Integer `/` truncates toward zero and `%` takes the sign of the dividend, as DIV and MOD do. Float terms follow IEEE semantics, and an untyped number that meets a float term as the other operand of an operator or built-in takes that float type, rounded to nearest-even exactly as a typed literal would be: with `knee: f32`, `knee <= 0.99` compares against `0.99:f32`, which is 0.9900000095367431640625. Typed literals work in formulas too. Anything else is a type error: a decimal number with no float term to meet, an integer term meeting a float term, or two float types meeting. LEARNABLE bounds take their parameter's type the same way. docs/05 says how these become solver queries.

**Literals.** A typed literal's integer must fit its type, and a decimal rounds to its float type, nearest-even. In a formula, a `-` before a typed literal belongs to it, so `-128:i8` is the i8 minimum. An array literal lists every element, row-major, then its type: `[1116352408, 1899447441, …]:Array<u32, 64>`; bind a large one to a port once with `%0 = IDENTITY([…]:Array<u32, 64>);`.

**Lexical rules.** Lexing is deterministic and takes the longest match; tools/vgsym.lark uses Lark's basic lexer, so vg-core's lexer can agree with it token for token. A word is a maximal run of `[A-Za-z0-9_]` that starts with a letter or `_`. Every word the grammar spells out (`module`, `true`, `result`, `in`, `post`, `f32`, `NODE`, `RETURN`, `FILE_READ`, `Bool`…) is a reserved keyword, never an `Ident` or `OpName`; op modes are plain identifiers that the validator checks, so `s`, `u` and `f` stay free as names. `Ident ::= [a-z_][A-Za-z0-9_]*` · `OpName ::= [A-Z][A-Z0-9_]*` · `Int ::= [0-9]+` · `Decimal ::= [0-9]+ ("." [0-9]+ ([eE] [+-]? [0-9]+)? | [eE] [+-]? [0-9]+)` · `Number ::= Int | Decimal`, never signed, so `x-1` is `x`, `-`, `1` · a port (`%12`) and a label (`@soft_knee`) are single tokens, so `x%2` is `x` followed by the port `%2`: write `x % 2` · `t.0.1` lexes as `t`, `.`, `0.1`: write `(t.0).1` · `String` is double-quoted with `\"` and `\\` escapes. tools/check_examples.py pins these cases and parses every VG-Sym example in docs/ and the synthesis skill; a grammar check of every example in this kit runs in CI from P1 on.

## Worked example: the audio limiter

The source spec's example, corrected. The full file is examples/audio_limiter.vgs; its plain-language request is examples/audio_limiter.prompt.md.

```text
module audio_limiter;

@MESH_QUERY @sha256 {
  signature: (Slice<u8>) -> (Array<u8, 32>);
  effects: [];
  post: result == spec:sha256(arg0);
  intent: "SHA-256 digest of a byte slice";
}

NODE @soft_knee [intent: "Soft-knee clip of one sample into [-1, 1]"] {
  SIGNATURE: (x: f32, knee: f32) -> (f32);
  EFFECTS: [];
  CONTRACT {
    PRE:  is_finite(x) && knee >= 0.5 && knee <= 0.99;
    POST: result >= -1.0 && result <= 1.0 && ((x >= 0.0) == (result >= 0.0));
  }
  BODY {
    %0  = FABS(x);
    %1  = FSUB(%0, knee);
    %2  = FMAX(%1, 0.0:f32);           // excess over the knee, >= 0
    %3  = FSUB(1.0:f32, knee);         // exact for knee in [0.5, 1]
    %4  = FDIV(%2, %3);                // t; overflows to +inf for huge x
    %5  = FADD(1.0:f32, %4);
    %6  = FDIV(1.0:f32, %5);
    %7  = FSUB(1.0:f32, %6);           // u = 1 - 1/(1+t), in [0, 1]
    %8  = FMA(%3, %7, knee);           // knee + (1 - knee) * u
    %9  = FCOPYSIGN(%8, x);
    %10 = LE.f(%0, knee);
    %11 = SELECT(%10, x, %9);          // linear below the knee
    %12 = FCLAMP(%11, -1.0:f32, 1.0:f32);
    RETURN(%12);
  }
}

NODE @limit_frame [intent: "Limit a 512x2 stereo frame and digest it", export] {
  SIGNATURE: (frame: Tensor<f32, [512, 2]>) -> (Tensor<f32, [512, 2]>, Array<u8, 32>);
  EFFECTS: [];
  CONTRACT {
    PRE:  forall(i in 0..512, c in 0..2) { is_finite(frame[i, c]) };
    POST: forall(i in 0..512, c in 0..2) { result.0[i, c] >= -1.0 && result.0[i, c] <= 1.0
                                           && ((frame[i, c] >= 0.0) == (result.0[i, c] >= 0.0)) }
          && result.1 == spec:sha256(as_bytes(result.0));
    COST: heap_bytes == 0 && arena_bytes == 0;
    BUDGET: latency_p99_us <= 500;
  }
  LEARNABLE {
    knee: f32 in [0.5, 0.99];          // frozen in the MVP; a loss arrives with the tuner (P9, R-07)
  }
  BODY {
    %0 = SIMD_MAP(@soft_knee, frame, knee);
    %1 = AS_BYTES(%0);
    %2 = CALL_HASH(@sha256, %1);
    RETURN(%0, %2);
  }
}
```

`Tensor<f32, [512, 2]>` with static dimensions is part of the MVP for this example; symbolic dimensions and tensor ops are [ALPHA].

## The counterexample-and-repair story (M1 must reproduce it)

The first draft of `@soft_knee` (examples/soft_knee_draft.vgs) computed u as t/(1+t). Verification fails: at x = 3.0e38 and knee = 0.5, t overflows to +inf and inf/inf is NaN, so `result >= -1.0` is false. The repair, 1 − 1/(1+t), is exactly 1 at t = +inf. A float32 simulation of the repaired kernel over 400,000 inputs, extremes included, found no POST violation; the verifier must prove it.

The sign clause in POST matters too: a clamp that mapped NaN to −1.0 would satisfy the range check while flipping loud positive samples negative. Range checks alone let wrong answers through; relational facts catch them.
