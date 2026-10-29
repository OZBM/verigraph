# 0004 · Result ops and port type annotations

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 02, 03, 14 and tools/vgsym.lark.

## Context

`Result<T, E>` is an MVP type, and the synthesis skill and OmniLift both return `Result::Err`, but no op built or inspected a Result. A constructor such as OK(v) also cannot infer E from its operand, and types are otherwise inferred locally, op by op.

## Decision

- Five structure ops: OK (0x0008), ERR (0x0009), IS_OK (0x000A), GET_OK (0x000B, obligation GET_OK.is_ok) and GET_ERR (0x000C, obligation GET_ERR.is_err). Formulas gain `is_ok(r)`, `ok(r)` and `err(r)`.
- A statement may annotate any result port with its type: `%3: Result<Slice<u8>, u32> = ERR(2:u32);`. The checker compares the annotation with the inferred type (VG-T0115). OK and ERR require it, because it supplies the half of the type their operand cannot.
- Grammar: `Stmt ::= Binding ("," Binding)* "=" Op "(" Args? ")" ";"` with `Binding ::= Port (":" Type)?`.

## Consequences

Errors become ordinary values that verified code can build, test and return. Annotations are optional everywhere else, but an agent can use them to have its intended type checked at the binding, which also gives a constrained decoder the expected type early. Case analysis stays explicit: test IS_OK, then call a kernel whose PRE assumes it through `ASSERT_GUARD.branch`.
