# 0005 · Prefix-checkable VG-Sym: define before use

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 03, 07 and 15.

## Context

The grammar allowed a port or a label to be used before the statement or item that defines it. Then no checker can judge a partial program, an agent's first error may surface far from its cause, and the Tier 2 masks of docs/07 cannot know which ports exist at the current token.

## Decision

- A port is used only after the statement that defines it (VG-S0002).
- A label is used only after its NODE, SPEC or `@MESH_QUERY` (VG-S0004). Mutually recursive groups, when REC_CALL arrives in the alpha, will be declared together.
- Canonical printing already satisfies both rules: `vg disasm` prints kernels before their users and ops in post-order.

## Consequences

Any prefix of a module can be checked as far as it goes, so errors appear at the earliest token that causes them, and Tier 2 can mask exactly (docs/15). Writing order matches the skill's advice, kernels first and root last. Reordering statements stays an equivalence move whenever it respects the rule.
