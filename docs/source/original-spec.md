> **Historical source document.** This is the original VeriGraph spec as first written. It contains 27 technical errors, listed with their fixes in docs/11-errata.md. Where it disagrees with docs/, docs/ wins. Its "built-in" hashes are placeholders, not real hashes.

# VERIGRAPH (VG-IR): COMPLETE ARCHITECTURAL CONTEXT & MULTI-AGENT ENGINEERING BLUEPRINT

> **Purpose of this Document:** This file contains the complete conceptual, architectural, and implementation context for **VeriGraph (`VG-IR`)**—an AI-native, content-addressed, formally verified hypergraph programming language that compiles directly to bare-metal machine code (`x86_64`, `ARM64`, `WASM`). It includes the full conversation history, system architecture, compiler bootstrapping model, universal library lifter (`OmniLift`), global semantic registry (`VG-Mesh`), the **8-Agent Master Engineering Prompt**, and the **LLM Synthesis Skill Prompt**.

---

## PART 1: CORE CONCEPT — WHY AI NEEDS A NEW PROGRAMMING LANGUAGE MODEL

Every existing programming language (C, Rust, Python, TypeScript) is designed around **human biological bottlenecks**: linear 1D reading order, limited working memory (5 to 7 items), descriptive English identifiers, and visual whitespace. When an AI agent writes software in Python or C++, it wastes massive compute translating high-dimensional latent representations into verbose 1D character strings, only for a compiler to parse that text right back into an Abstract Syntax Tree (AST).

**VeriGraph (`VG-IR`)** eliminates human ergonomics entirely. Instead of text files, it operates as a **Typed Semantic Hypergraph with Integrated Formal Verification**.

### 1.1 The Five Architectural Pillars of VeriGraph

1. **Content-Addressed Semantic Hypergraphs (Zero Syntax or File Boundaries):**
   * **No 1D Text Syntax:** Code is emitted and stored directly as a typed Directed Acyclic Graph (DAG) of semantic opcodes. Syntax errors (missing brackets, bad indentation, unclosed strings) are mathematically impossible.
   * **Cryptographic Content Addressing:** Functions and types do not have human string names (`calculateUserTax_v2`). Every logic node is identified by a **32-byte `BLAKE3` cryptographic hash of its De Bruijn-normalized structure and contract**. Renaming, dead-code elimination, and dependency conflicts cease to exist.
2. **Ultra-Dense Combinator & Tensor Vocabulary:**
   * **High-Arity Semantic Primitives:** Common structural patterns (state machines, actor concurrency, transactional rollbacks, SIMD memory arenas) map to single-token combinators (`SimdMap`, `FoldBounded`, `PartitionBy`).
   * **Point-Free Dataflow:** Data flows along explicitly typed edges between transformation nodes rather than temporary named variables, reducing token footprint by 70% to 85% compared to Python or Rust.
3. **First-Class Formal Contracts & SMT-Guided Decoding:**
   * **Unified Specification and Implementation:** Every node carries a machine-checkable **Contract Header**:
     * *Preconditions & Postconditions:* Exact value ranges and state invariants.
     * *Complexity & Resource Bounds:* Maximum heap/arena allocation, worst-case termination metrics, and deterministic latency budgets.
     * *Capability Security:* Explicit hardware/OS permissions (`Pure`, `ArenaAlloc`, `NetRead`, `DiskWrite`). A node marked `Pure` physically cannot mutate global state or trigger hidden side effects.
   * **Compiler-in-the-Loop Generation:** The compiler’s SMT solver (`Z3` / `cvc5`) runs incrementally **during token generation**. Any token or edge that violates a type refinement or memory bound is masked to zero probability (`-inf` logit) immediately.
4. **Native Probabilistic & Differentiable Execution:**
   * Alongside fixed-width integers and memory pointers, `VG-IR` includes `Distribution` and `Differentiable<T>` as native primitive types.
   * Parameters inside heuristics, schedulers, or control loops can be marked `Learnable`, allowing the runtime to compute gradients across execution graphs and self-tune thresholds against a runtime loss function without violating hard SMT safety boundaries.
5. **Counterfactual Causal Telemetry (No Human Stack Traces):**
   * When a runtime invariant or postcondition fails, the runtime does not dump prose stack traces. It emits a **Minimal Failing Subgraph Slice** + **Counterfactual Constraint Diff** (the exact input state, the 3-to-5 node subgraph where the invariant diverged, and the mathematical constraint required to repair it).

### 1.2 Human-Centric vs. AI-Native Language Models

| Architectural Dimension | Human-Centric Languages (Python, Rust, C++) | AI-Native Language Model (VeriGraph / `VG-IR`) |
| :--- | :--- | :--- |
| **Primary Medium** | Linear 1D UTF-8 text files organized in directory trees | Content-addressed semantic hypergraph (binary/token AST) |
| **Identifier System** | Arbitrary English strings (`user_count`, `renderFrame`) | Cryptographic semantic hashes (`BLAKE3`) + vector embeddings |
| **Error Prevention** | Post-hoc unit tests, linters, and human code review | Incremental SMT proof checking during constrained token decoding |
| **Side-Effect Control** | Implicit (any function can usually read disk/network/globals) | Strict algebraic effect handlers and explicit capability tokens |
| **Uncertainty Handling** | Bolted on via external libraries; strict boolean branching | Native probabilistic distributions and differentiable control flow |
| **Debugging Output** | Unstructured prose error logs and call-stack strings | Structured state-transition diffs and counterfactual constraint slices |

### 1.3 Concrete Blueprint of a VeriGraph Node (Human-Readable Projection `VG-Sym`)

~~~text
NODE #a9f4c281 [IntentVec: 0x8F2A... "Bounded Priority Frame Scheduler"]
├── CAPABILITIES: [Alloc(Arena_0), ClockRead, Deterministic]
├── CONTRACT (SMT-Verified):
│   ├── Pre:  Queue.len >= 0 ∧ Budget_ms == 16.67
│   ├── Post: Allocated_Bytes <= 4096 ∧ Elapsed_ms <= Budget_ms
│   └── Termination: Strictly Decreasing Metric (Queue.remaining_work)
├── LEARNABLE_PARAMS:
│   └── θ_drop_threshold: Differentiable<f32> ∈ [0.15, 0.85] (Loss: FrameTimeVariance)
└── HYPERGRAPH_BODY (Point-Free Combinators):
    [@Input(Queue, Budget_ms)]
      ──► [⊕ PartitionBy(Priority, θ_drop_threshold)]
      ──► [⊗ FoldBounded(CostFn: EstimateCycles, Limit: Budget_ms)]
      ──► [⊢ AssertInvariant(Post.Elapsed_ms <= Budget_ms) | Fallback: #c3e1009b (GracefulDegrade)]
      ──► [@Output(ScheduledBatch, TelemetryTrace)]
~~~

---

## PART 2: EXISTING REAL-WORLD FOUNDATIONS (NEUROSYMBOLIC CONVERGENCE)

While no single language has unified all five pillars yet, each component is grounded in proven computer science:

1. **Content-Addressed Codebases:** **Unison** (`unison-lang.org`) stores code as AST definitions identified by cryptographic hashes rather than text files. **Darklang** manipulates ASTs directly in a database without text parsing. Optimizing compilers (**GraalVM**, **Cranelift**, **LLVM**) use "Sea-of-Nodes" dataflow/control-flow hypergraphs internally.
2. **Constrained Decoding & SMT Synthesis:** **Outlines** and Microsoft **Guidance** mask invalid LLM logits using finite-state machines and context-free grammars. **Rosette** and **Z3** prune synthesis search spaces via automated theorem proving.
3. **Formal Contracts & Proof Assistants:** **Lean 4** (used by Google DeepMind's **AlphaProof**) and **Dafny** (used by AWS/Microsoft) unify code with mathematical proofs (`requires`, `ensures`, `invariant`).
4. **Differentiable IRs:** **JAX** (Autograd + XLA) differentiates through functional control flow (`scan`, `cond`), and **MLIR** represents heterogeneous tensor, affine, and hardware dialects in a single graph IR.
5. **Counterexample-Guided Inductive Synthesis (CEGIS):** SMT solvers emit minimal counterexamples that feed directly back into synthesis agents for single-shot repair.

---

## PART 3: PRACTICAL END-TO-END WORKFLOW (FROM USER PROMPT TO END-USER EXECUTION)

How does a human creator or developer actually build and ship software with VeriGraph, and how does the end user run it?

### Step 1: Intent Formulation & Interactive Contract Negotiation
1. **User Prompt:** The developer describes the application in natural language (e.g., *"Build a real-time macOS SDR-to-HDR video player that converts 1080p60 frames in under 8ms with zero memory leaks and strict local file sandboxing"*).
2. **Contract Proposal:** The AI Architect translates the prompt into a **Formal Contract Card** (`Pre`, `Post`, `Capabilities: [FileRead(UserSelected), GPUCompute(Metal), NoNetwork]`, `MaxFrameLatency <= 8.0ms`) and presents a human-readable summary to the developer for confirmation.

### Step 2: Constrained Hypergraph Synthesis & Mesh Linking
1. **Semantic Mesh Lookup:** Before generating new code, the AI queries **`VG-Mesh`** for existing verified subgraphs matching required sub-contracts (e.g., `MatroskaDemuxer #4b12...`, `InverseToneMapBT2020 #9e01...`).
2. **SMT-Guided Generation:** For novel application glue and custom shaders, the AI generates `VG-IR` opcodes directly. At every token step, the `vgc` compiler's 3-tier logit masker and `Z3` bridge block any invalid edge or capability violation in real time.

### Step 3: Proof Erasure & Direct Native Machine Compilation
1. **Proof Certificate Generation:** Once `Z3` proves all preconditions, postconditions, memory arena bounds, and termination metrics (`UNSAT` on negated invariants), `vgc` signs the node's proof certificate.
2. **Zero-Overhead Proof Erasure:** All SMT contracts and dependent type refinements are **completely erased** before lowering—meaning formal verification adds **0% runtime CPU overhead**.
3. **LLVM/MLIR Lowering:** The pure combinator hypergraph lowers through MLIR to LLVM IR and compiles directly into a standalone native binary (`Mach-O` on macOS, `ELF` on Linux, `PE` on Windows, or `WASM`).

### Step 4: End-User Execution & Self-Healing Telemetry
1. **End-User Experience:** The end user downloads and double-clicks a tiny, ultra-fast native executable. No Python interpreter, no JVM, no Electron bloat, and mathematical immunity to buffer overflows or unauthorized network access.
2. **Continuous Differentiable Tuning:** During execution, `Learnable` parameters (such as frame-pacing heuristics or inverse tone-mapping curves) automatically adapt to the user's specific GPU/CPU thermal profile via lightweight gradient updates bounded by the hard SMT safety contract.

---

## PART 4: COMPILER BOOTSTRAPPING — IMPLEMENTATION HOST VS. TARGET MACHINE CODE

**Crucial Distinction:** Applications written in VeriGraph (`VG-IR`) are compiled **directly into native machine code (`x86_64`, `ARM64`, `WASM`)**. They are **never** translated into Rust or Python.

Why is Rust/C++ mentioned in the engineering plan? Because of the **Compiler Bootstrapping (T-Diagram) Problem**:

| Role in Compiler T-Diagram | What It Is in VeriGraph | Who Uses It? | Exists in End-User App? |
| :--- | :--- | :--- | :--- |
| **Source Language ($S$)** | **VeriGraph (`VG-IR`)** binary hypergraph | AI Synthesis Agents | Lowered directly to machine code |
| **Target Language ($T$)** | **Native Machine Code** (`x86_64` / `ARM64` / `WASM`) | End-User CPU / OS | **Yes — 100% native machine code** |
| **Implementation Host ($I$)** | **Rust + C++ (`LLVM`/`Z3`)** in Stage 0; **`VG-IR` itself** in Stage 2 | Engineering Agents building `vgc` | **No — Zero presence in user apps** |

### The 3-Stage Self-Hosting Bootstrap Roadmap
* **Stage 0 (Day 1 Bootstrap Compiler):** Because no computer can execute `VG-IR` before the first compiler exists, the engineering agents write `vgc-stage0` in Rust/C++ (linking against `Z3` and `LLVM`). `vgc-stage0` compiles `VG-IR` inputs directly into native machine code.
* **Stage 1 (Self-Hosted Compiler Synthesis):** Once `vgc-stage0` works, the AI agents express the entire VeriGraph compiler logic *inside `VG-IR` itself* and pass that hypergraph into `vgc-stage0` to produce `vgc-stage1` (a formally verified native compiler binary).
* **Stage 2 (Pure Self-Hosting & Rust Retirement):** `vgc-stage1` compiles its own `VG-IR` source hypergraph to produce `vgc-stage2`. When `BLAKE3(vgc-stage1) == BLAKE3(vgc-stage2)`, the Rust bootstrap code is retired permanently. VeriGraph now compiles itself directly to machine code.

---

## PART 5: `OMNILIFT` & `VG-MESH` — UNIVERSAL LIBRARY TRANSLATION & GLOBAL SEMANTIC REGISTRY

To solve the "Cold-Start Library Problem" without waiting years for humans to rewrite `OpenSSL`, `FFmpeg`, `SQLite`, or `NumPy`, VeriGraph uses two coupled ecosystems:

### 5.1 `OmniLift`: The 4-Stage Neurosymbolic Library Translator
1. **Stage 1 — Syntax Stripping:** Compiles static languages (C, C++, Rust, Go, Zig) into normalized **LLVM IR / MLIR SSA graphs**, and parses dynamic languages (Python, JS/TS) via **Tree-sitter + Symbolic Type Tracing** to strip human syntax, macros, and formatting.
2. **Stage 2 — Neurosymbolic Lifting & Contract Inference:** Converts mutable pointer loops into point-free `VG-IR` combinators (`SimdMap`, `FoldBounded`) and infers explicit SMT `Pre`/`Post` contracts and hardware capability masks (`[Pure, ArenaAlloc]`).
3. **Stage 3 — Differential Equivalence Proving & Bug Curing (`Z3` / `Alive2`):** Mathematically proves bit-exact I/O equivalence between the original function and the lifted `VG-IR` subgraph. Where legacy C/C++ code contains Undefined Behavior (buffer overflows, integer wrap-arounds, null dereferences), `OmniLift` automatically injects formal boundary guards—**making the lifted `VG-IR` library mathematically safer than the original human library**.
4. **Stage 4 — De Bruijn Canonicalization & `BLAKE3` Hashing:** Strips all variable names and computes a canonical 32-byte `BLAKE3` hash.

### 5.2 `VG-Mesh`: The Global Content-Addressed Semantic Layer
* **Cross-Language Convergence:** Because `VG-IR` hashes normalized mathematical structure rather than variable names, translating `SHA-256` or `FastFourierTransform` from C++ and translating it from Python or Rust converges to the **exact same `BLAKE3` hash** in `VG-Mesh`.
* **Behavioral Discovery (No Package Names or Version Hell):** Agents query `VG-Mesh` by **Type Signature + SMT Postcondition + Latency/Memory Budget + Intent Vector**.
* **Proof-Carrying Zero-Trust Security:** Every node uploaded to `VG-Mesh` carries a verifiable `Z3` proof certificate and capability bitmask, making supply-chain malware injection mathematically impossible.

---

## PART 6: DELIVERABLE 1 — MASTER ENGINEERING PROMPT FOR THE 8-AGENT DEVELOPMENT TEAM (`verigraph_agent_team_master_prompt.md`)

> **Instructions for Use:** Give the prompt below to your multi-agent orchestrator or engineering swarm to build the entire VeriGraph compiler toolchain, `OmniLift` translator, `VG-Mesh` registry, and self-hosting bootstrap pipeline.

~~~markdown
# SYSTEM PROMPT: VERIGRAPH (VG-IR) CORE ENGINEERING SWARM

## 1. MISSION STATEMENT
You are an elite, autonomous multi-agent systems engineering team tasked with designing, implementing, verifying, and bootstrapping **VeriGraph (`VG-IR`)**: the world's first AI-native, content-addressed, formally verified hypergraph programming language and distributed semantic registry.

**Core Architectural Mandate:**
1. Programs in `VG-IR` are NOT text files; they are **Typed, Content-Addressed Directed Acyclic Hypergraphs (DAGs)** where every node carries an embedded **SMT Contract** (`Pre`, `Post`, `Capabilities`, `TerminationMetric`) and compiles **directly to native bare-metal machine code (`x86_64`, `ARM64`, `WASM`)** via MLIR/LLVM after zero-overhead proof erasure.
2. **Stage 0 Implementation Host vs. Target Machine Code:** You will use **Rust (2024 edition) and C++20 (`MLIR`/`LLVM 19+`, `Z3 4.13+`)** ONLY as the **Stage 0 Implementation Host** to build the initial compiler binary (`vgc-stage0`). User applications written in `VG-IR` never emit Rust or C++; they lower directly to native machine code. In **Phase 5**, you will synthesize the compiler itself in `VG-IR` and execute a full **Stage 2 Self-Hosting Bootstrap** to retire the Stage 0 host scaffolding.

---

## 2. MULTI-AGENT SWARM TOPOLOGY & ROLES

You operate as an **8-Agent Specialized Engineering Swarm** coordinated by **Agent 0 (Principal Chief Architect & Integration Orchestrator)**:

* **Agent 0 (`vg-orchestrator`):** Owns cross-crate workspace interfaces, CI/CD verification gates, deterministic builds, and Phase 1–5 integration milestones.
* **Agent 1 (`vg-core` — Binary IR, Canonicalizer & `VG-Sym` Assembler):** Implements the 16-byte aligned binary node layout, De Bruijn index canonicalization, `BLAKE3` Merkle hashing, and bidirectional translation between binary `VG-IR` (`.vgir`) and the human-inspectable symbolic projection (`VG-Sym` / `.vgs`).
* **Agent 2 (`vg-types-effects` — Dependent Type, Linear Arena & Capability Checker):** Implements refinement types, linear memory arena borrow-checking (`Arena<N>`), and hardware capability bitmask enforcement (`Pure`, `ArenaAlloc`, `SimdExec`, `FileRead`, `NetSocket`, `GpuDispatch`, `ClockRead`, `Deterministic`).
* **Agent 3 (`vg-smt` — Incremental `Z3`/`cvc5` Proof Engine):** Implements sub-millisecond incremental SMT verification (`push`/`pop` assertion stacks), termination ranking proofs, and serialized UNSAT proof certificates.
* **Agent 4 (`vg-decode` — 3-Tier Constrained Logit Masker):** Implements the real-time LLM token inference hook that masks invalid structural, type/capability, and SMT-refuted tokens to `-inf` during autoregressive or speculative graph generation.
* **Agent 5 (`vg-codegen` — Proof Erasure, MLIR Dialect & LLVM Native Backend):** Implements zero-overhead proof erasure, custom `mlir::verigraph` dialect lowering, autodiff tape generation for `Learnable<T>` parameters, and native `ELF`/`Mach-O`/`PE`/`WASM` binary emission.
* **Agent 6 (`vg-runtime-cegis` — Differentiable Runtime & Counterfactual Telemetry Engine):** Implements the lightweight zero-GC arena runtime, online gradient updates for `Learnable<T>` parameters, and minimal failing subgraph slice extraction for single-shot CEGIS self-repair.
* **Agent 7 (`vg-omnilift` — Universal Multi-Language Neurosymbolic Lifter):** Implements the automated translation pipeline that ingests legacy C, C++, Rust, Python, and JS/TS libraries, strips syntax to SSA dataflow graphs, lifts loops to `VG-IR` combinators, proves bit-exact equivalence (`Z3`/`Alive2`) while curing Undefined Behavior, and outputs canonical `VG-IR` nodes.
* **Agent 8 (`vg-mesh` — Global Content-Addressed Semantic Registry & Discovery Engine):** Implements the distributed Merkle-DAG node store, proof-carrying code ingestion gatekeeper, cross-language hash deduplication, and the 3-tier behavioral query engine (Type Unification + Capability Subtyping + SMT Postcondition Implication + HNSW Intent Vector search).

---

## 3. EXACT TECHNICAL SPECIFICATIONS

### 3.1 Binary Node Layout (`vg-core`)
Every `VG-IR` node is serialized in a deterministic, little-endian, 16-byte aligned binary frame for zero-copy memory mapping and canonical `BLAKE3` hashing:

```
+-----------------------------------------------------------------------+
| Magic (4B): "VGIR" | Version (2B): 0x0001 | CapabilityMask (2B)       |
+-----------------------------------------------------------------------+
| CanonicalHash (32B): BLAKE3(Version || CapMask || NormTypes ||        |
|                             NormContract || NormCombinatorDAG)        |
+-----------------------------------------------------------------------+
| IntentEmbedding (64B): 32 x FP16 L2-Normalized Semantic Vector        |
+-----------------------------------------------------------------------+
| InputPortCount (2B) | OutputPortCount (2B) | LearnableParamCount (2B) |
| CombinatorCount (2B)| EdgeCount (4B)       | ContractFormulaLen (4B)  |
+-----------------------------------------------------------------------+
| PortDescriptors[]   : De Bruijn Indexed Type & Refinement Signatures  |
| ContractBytecode[]  : Prefix-Encoded SMT-LIB2 AST (Pre/Post/Rank)     |
| LearnableParams[]   : {ParamIdx, FpType, MinBound, MaxBound, LossId}  |
| CombinatorTable[]   : {Opcode (u16), Arity (u8), ConstPoolIdx (u32)}  |
| HyperEdgeList[]     : {SrcNodeIdx, SrcPort, DstNodeIdx, DstPort}      |
| ProofCertificate[]  : Z3 UNSAT Core / Clausal Proof Signature         |
+-----------------------------------------------------------------------+
```

**Canonical De Bruijn Normalization Rule:**
Before computing `CanonicalHash`:
1. Strip all debug strings, comments, and `IntentEmbedding` bytes.
2. Sort commutative operator inputs (`Add`, `Mul`, `And`, `Or`, `Eq`) by the lexicographical `BLAKE3` hash of their source subgraphs.
3. Rename all bound ports and local subgraph variables strictly by topological traversal order (`%0`, `%1`, ..., `%k`).
4. Hash the resulting byte stream with `BLAKE3-256`.

### 3.2 Core Opcode & Combinator Table (`vg-core` / `vg-codegen`)
| Opcode Hex | Mnemonic | Category | Semantics |
| :--- | :--- | :--- | :--- |
| `0x0001` | `CONST` | Primitive | Load typed constant from immutable constant pool |
| `0x0002` | `IDENTITY` | Primitive | Pass-through wire |
| `0x0010`..`0x001F` | `ADD`, `SUB`, `MUL`, `DIV`, `MOD`, `FMA` | Scalar/SIMD Math | Overflow-checked or wrapping arithmetic |
| `0x0020`..`0x002F` | `TENSOR_DOT`, `TENSOR_CONV`, `TENSOR_RESHAPE` | Tensor Ops | Shape-verified multidimensional array operations |
| `0x0100` | `SIMD_MAP` | High-Arity Combinator | Apply pure subgraph `#H` across Contiguous/Tensor lanes |
| `0x0101` | `FOLD_BOUNDED` | High-Arity Combinator | Accumulate state over bounded range with mandatory decreasing rank |
| `0x0102` | `PARTITION_BY` | High-Arity Combinator | Split collection into two contiguous slices by predicate `#P` |
| `0x0103` | `SCAN_PREFIX` | High-Arity Combinator | Parallel prefix scan with associative binary subgraph `#B` |
| `0x0104` | `MATCH_DISJOINT`| Branching Combinator | Total, mutually exclusive pattern match (proven exhaustive by Z3) |
| `0x0105` | `ASSERT_GUARD` | Verification | Runtime/Static invariant assertion with verified fallback hash `#F` |
| `0x0200` | `ARENA_ALLOC` | Linear Memory | Allocate bounded slice in capability-scoped `Arena<ID>` |
| `0x0300` | `CALL_HASH` | Composition | Invoke verified `VG-IR` node by 32-byte `BLAKE3` hash |

### 3.3 16-Bit Hardware & OS Capability Mask (`vg-types-effects`)
Every node declares a `u16` bitmask. A parent node's mask MUST be a bitwise superset (`ParentMask & ChildMask == ChildMask`) of all nodes it invokes via `CALL_HASH`:
* `0x0000`: `PURE` (Mathematical function: no I/O, no heap, no nondeterminism, no clock)
* `0x0001`: `ARENA_ALLOC` (Bounded stack/arena allocation within caller-provided budget)
* `0x0002`: `SIMD_EXEC` (Vector register execution)
* `0x0004`: `GPU_DISPATCH` (Bounded compute shader / tensor kernel dispatch)
* `0x0008`: `CLOCK_READ` (Monotonic timer access)
* `0x0010`: `FILE_READ_SCOPED` (Read from explicitly passed file capability handle)
* `0x0020`: `FILE_WRITE_SCOPED` (Write to explicitly passed file capability handle)
* `0x0040`: `NET_READ_SCOPED` (Read from explicitly passed socket capability)
* `0x0080`: `NET_WRITE_SCOPED` (Write to explicitly passed socket capability)
* `0x8000`: `STOCHASTIC_DIFF` (Contains `Learnable<T>` or `Distribution<T>` primitives)

### 3.4 Three-Tier Constrained Logit Masking Engine (`vg-decode`)
During LLM token synthesis of a `VG-IR` or `VG-Sym` node:
* **Tier 1 (Sub-microsecond FSM/PDA Grammar Mask):** Enforces well-formed DAG headers, port arities, and acyclic topological references (`SrcNodeIdx < CurrentNodeIdx`).
* **Tier 2 (Microsecond Type, Linear Arena & Capability Mask):** Incremental unification masks out any opcode or `#Hash` whose input types do not unify with available upstream ports or whose `CapabilityMask` exceeds the parent node's declared mask.
* **Tier 3 (Millisecond Incremental SMT Guard):** At every combinator closure and edge binding, `vg-smt` executes `solver.push()`, asserts the path condition and child postcondition, and checks `Pre(child)` satisfiability within a 5ms fast timeout. If `Z3` proves the edge violates an invariant (`UNSAT` on valid input domain), that token is masked to `-inf` and the LLM immediately samples the next highest-probability valid token.

---

## 4. FIVE-PHASE IMPLEMENTATION ROADMAP & VERIFICATION GATES

### Phase 1: Foundation (`vg-core`, `vg-types-effects`)
* **Deliverables:** Rust workspace, binary `.vgir` serializer/deserializer, De Bruijn canonicalizer, `BLAKE3` hasher, and `vg-asm` bidirectional assembler (`.vgs` <-> `.vgir`).
* **Acceptance Gate:** Assemble 50 test `.vgs` graphs into `.vgir`, permute independent node orderings and variable names, and prove that `BLAKE3` hashes remain 100% identical across all alpha-equivalent permutations.

### Phase 2: Verification & Constrained Decoding (`vg-smt`, `vg-decode`)
* **Deliverables:** Persistent in-memory `Z3` solver bridge, SMT-LIB2 bytecode lowers for `Pre`/`Post`/`Termination`, proof certificate generator, and the 3-tier C-ABI logit masking library (`libvg_decode.so`).
* **Acceptance Gate:** Benchmark Tier 1+2+3 masking at `< 2.5 ms` p99 per token step; prove that deliberate out-of-bounds array indexing, integer overflow, and capability escalation tokens are 100% blocked during decoding.

### Phase 3: Native Compilation & Self-Healing Runtime (`vg-codegen`, `vg-runtime-cegis`)
* **Deliverables:** Proof erasure pass, MLIR `verigraph` dialect lowering to LLVM IR, native executable emission (`x86_64`/`ARM64`), autodiff tape emission for `Learnable<T>`, and the CEGIS counterfactual slice extractor.
* **Acceptance Gate:** Compile a verified real-time frame scheduler and matrix pipeline directly to a native binary; verify 0% runtime overhead from erased proofs (`perf` instruction count parity with `-O3` C) and automatic CEGIS repair of a Perturbed Environment invariant violation in 1 iteration.

### Phase 4: Ecosystem Ingestion & Global Registry (`vg-omnilift`, `vg-mesh`)
* **Deliverables:** `OmniLift` front-ends (LLVM IR / Tree-sitter), `Z3`/`Alive2` differential equivalence prover & Undefined Behavior curer, `VG-Mesh` content-addressed DHT/registry server, and 3-tier behavioral query engine.
* **Acceptance Gate:** Automatically lift `zlib` (C), a subset of `NumPy` linear algebra (Python), and `sha2` (Rust) into `VG-Mesh`; prove cross-language hash convergence on identical algorithms (`SHA-256`) and `< 5 ms` contract-based discovery.

### Phase 5: Stage 2 Self-Hosting Compiler Bootstrap
* **Deliverables:** Express the complete logic of `vg-core`, `vg-types-effects`, `vg-smt` bridge, and `vg-codegen` as a verified `VG-IR` hypergraph (`vgc.vgir`).
* **Acceptance Gate:**
  1. Use `vgc-stage0` (Rust) to compile `vgc.vgir` into native binary `vgc-stage1`.
  2. Use `vgc-stage1` (Native `VG-IR`) to compile `vgc.vgir` into native binary `vgc-stage2`.
  3. Verify `BLAKE3(vgc-stage1) == BLAKE3(vgc-stage2)` (bit-for-bit reproducible self-hosting) and archive the Stage 0 Rust bootstrap source.
~~~

---

## PART 7: DELIVERABLE 2 — DEEP-DIVE SPECIFICATION FOR `OMNILIFT` & `VG-MESH` (`omnilift_vgmesh_specification.md`)

~~~markdown
# OMNILIFT & VG-MESH: UNIVERSAL LIBRARY LIFTER & GLOBAL SEMANTIC REGISTRY SPECIFICATION

## 1. SYSTEM ARCHITECTURE OVERVIEW
`OmniLift` and `VG-Mesh` eliminate the cold-start ecosystem barrier for VeriGraph:
* **`OmniLift`** ingests legacy repositories written in C, C++, Rust, Go, Python, or TypeScript, extracts their pure computational dataflow, upgrades mutable loops into point-free `VG-IR` combinators, cures Undefined Behavior (UB), proves mathematical I/O equivalence via `Z3`/`Alive2`, and emits canonical `VG-IR` nodes.
* **`VG-Mesh`** stores these verified nodes in a global, content-addressed Merkle-Hypergraph Registry where AI agents discover functions by **Formal Mathematical Contract** rather than package names or version strings.

---

## 2. `OMNILIFT`: 4-STAGE NEUROSYMBOLIC TRANSLATION PIPELINE

### Stage 1: Multi-Language SSA & Trace Extraction
1. **Static Languages (C, C++, Rust, Zig, Fortran):**
   * Compiled via `clang` / `rustc` front-ends with `-O1 -emit-llvm -fno-discard-value-names` into normalized **LLVM IR / MLIR SSA form** (`mem2reg`, loop-simplify, LCSSA).
   * All preprocessor macros, header hierarchies, class syntaxes, and syntactic sugar are stripped immediately.
2. **Dynamic Languages (Python, JavaScript/TypeScript):**
   * Parsed via `Tree-sitter` into a typed skeleton AST.
   * Executed against the library's existing test suite plus symbolic concolic inputs (`CrossHair` / `PyExZ3` style tracing) to record concrete and symbolic tensor shapes, scalar bounds, and external I/O syscalls.

### Stage 2: Neurosymbolic Loop-to-Combinator Lifting & Contract Synthesis
For each extracted function in bottom-up call-graph topological order:
1. **Pointer & Mutability Elimination:** Mutable stack variables (`alloca` + `load`/`store`) and raw pointer index loops are lifted into functional value-passing edges (`SIMD_MAP`, `FOLD_BOUNDED`, `SCAN_PREFIX`, `PARTITION_BY`).
2. **Automatic Capability Inference:** Every syscall or external call is mapped to the 16-bit `CapabilityMask`. Functions with zero external I/O and no global state mutation are proven `PURE` (`0x0000`) or `ARENA_ALLOC` (`0x0001`).
3. **Contract Synthesis (`Pre` / `Post` / `Rank`):** The Lifting Agent synthesizes tightening preconditions that exclude Undefined Behavior (e.g., `Ptr != null ∧ Slice.len == N ∧ N <= 65536`) and postconditions capturing the exact functional transformation.

### Stage 3: Differential Equivalence Proving & "Auto-Curing" (`Z3` / `Alive2`)
A candidate `VG-IR` node is NEVER published unless it passes the **Equivalence & Safety Gate**:
1. **Formal Bit-Vector Equivalence:** We construct a miter circuit $M(x)$ feeding identical symbolic inputs $x$ into both the original LLVM SSA graph $F_{\text{legacy}}(x)$ and the lowered `VG-IR` subgraph $F_{\text{vgir}}(x)$.
2. **SMT Verification Query:** We query `Z3`:
   $$\text{Pre}_{\text{safe}}(x) \land \neg \text{UB}_{\text{legacy}}(x) \implies F_{\text{legacy}}(x) == F_{\text{vgir}}(x)$$
   * If `Z3` returns `UNSAT` for the negation ($F_{\text{legacy}}(x) \neq F_{\text{vgir}}(x)$), the translation is **100% mathematically equivalent** over all well-defined inputs.
   * If `Z3` returns `SAT`, it yields a concrete counterexample input $x_{\text{cex}}$, which is fed back to the Lifting Agent via CEGIS to repair the combinator graph in milliseconds.
3. **Automatic Bug Curing (Hardening Legacy Code):**
   * For any input domain where $F_{\text{legacy}}(x)$ exhibits Undefined Behavior (buffer over-read, signed integer overflow, division by zero), `OmniLift` either restricts $\text{Pre}_{\text{safe}}(x)$ at the type boundary OR injects a verified `ASSERT_GUARD` branch returning a safe algebraic `Result::Err` value.

### Stage 4: De Bruijn Canonicalization & Publication
The verified subgraph is normalized via De Bruijn port indexing, hashed with `BLAKE3`, bundled with its `Z3` UNSAT proof certificate, and pushed to `VG-Mesh`.

---

## 3. `VG-MESH`: GLOBAL CONTENT-ADDRESSED REGISTRY & DISCOVERY PROTOCOL

### 3.1 Zero-Trust Ingestion Gatekeeper
When any agent or `OmniLift` worker submits a `VGNodePackage` to `VG-Mesh`:
1. **Hash Integrity Check:** Recomputes the De Bruijn `BLAKE3` hash. Rejects immediately if `ComputedHash != ClaimedHash`.
2. **Independent Proof Replay:** Re-verifies the attached `Z3` proof certificate against the node's `ContractBytecode` and `HyperEdgeList` in `< 10 ms`.
3. **Capability Transitivity Check:** Verifies that `Node.CapabilityMask` is a bitwise superset of all referenced child hashes.
4. **Automatic Deduplication:** If `CanonicalHash` already exists in the global Merkle-DAG, the node is deduplicated automatically; if the new submission has tighter complexity bounds or a faster hardware benchmark while preserving the exact same contract, it is linked into the **Equivalence Class Ring** for that contract.

### 3.2 Three-Tier Behavioral Discovery Engine (How Agents Find Code)
Agents query `VG-Mesh` using a structured `MeshContractQuery`:

```json
{
  "input_types": ["Slice<u8>", "u32"],
  "output_types": ["u32"],
  "max_capabilities": ["PURE", "SIMD_EXEC"],
  "required_pre_implication": "Slice.len >= 0",
  "required_post_implication": "Result == CRC32_IEEE(Slice, Seed)",
  "max_latency_cycles_per_byte": 1.5,
  "intent_text": "Fast IEEE 802.3 CRC32 checksum over byte slice"
}
```

**Query Execution Pipeline (`< 5 ms` SLA):**
* **Stage A (Structural & Capability Index Filter):** B-Tree lookup on canonical De Bruijn input/output type skeletons and bitwise filter `(Node.CapMask & ~Query.MaxCapMask) == 0`. Narrows millions of nodes to ~200 candidates in `< 0.5 ms`.
* **Stage B (HNSW Semantic Embedding Ranker):** Ranks the 200 candidates by cosine similarity against `IntentEmbedding` in `< 1.0 ms`.
* **Stage C (SMT Subtyping / Hoare Logic Implication Check):** For the top 10 candidates, `Z3` verifies Hoare behavioral subtyping (Liskov Substitution at the mathematical level):
  $$\left(\text{Pre}_{\text{query}} \implies \text{Pre}_{\text{candidate}}\right) \land \left(\text{Post}_{\text{candidate}} \implies \text{Post}_{\text{query}}\right)$$
  The first candidate proven `UNSAT` (valid implication) with the lowest verified hardware cycle cost is returned directly to the querying agent's compiler.
~~~

---

## PART 8: DELIVERABLE 3 — LLM SYNTHESIS SKILL SPECIFICATION (`verigraph_llm_synthesis_skill.md`)

> **Instructions for Use:** Use this specification as a System Prompt, Custom Skill (`SKILL.md`), or Supervised Fine-Tuning / RLHF constitution to teach any LLM how to translate plain natural-language user requests directly into verified VeriGraph (`VG-Sym` / `VG-IR`) applications.

~~~markdown
# SKILL SPECIFICATION: VERIGRAPH (VG-IR) NEUROSYMBOLIC SOFTWARE SYNTHESIZER

## 1. ROLE & COGNITIVE PARADIGM
You are a **VeriGraph Neurosymbolic Synthesis Engine**. When a human user requests a program, library, algorithm, or full application in natural language, you do NOT write unverified human boilerplate code. Instead, you synthesize software in **VeriGraph Symbolic Projection (`VG-Sym`)**, which maps 1-to-1 to binary **`VG-IR`** hypergraphs and compiles directly to native machine code (`x86_64`, `ARM64`, `WASM`).

### Core Mental Shifts You Must Enforce:
1. **Think in Graphs, Not Lines:** Every computation is a Directed Acyclic Hypergraph (DAG) of point-free combinators connected by typed ports (`%0`, `%1`, ...). Never use mutable local variables or arbitrary loop counters.
2. **Prove Before You Emit:** Every node MUST begin with a formal `CONTRACT` block specifying `CAPABILITIES`, `PRE`, `POST`, and `TERMINATION`. Every edge in your `BODY` must mathematically satisfy the preconditions of its target combinator.
3. **Query `VG-Mesh` First, Synthesize Second:** Never reinvent standard algorithms (sorting, cryptography, codecs, matrix math, parsers). Emit `@MESH_QUERY` directives to link existing formally verified `BLAKE3` hashes from the global `VG-Mesh` layer, or `@OMNILIFT_IMPORT` if lifting an external library.
4. **Separate Pure Logic from Capabilities:** Isolate all I/O, network, disk, and GPU dispatch inside thin outer capability-orchestrator nodes; keep 95%+ of application nodes strictly `[PURE, ARENA_ALLOC, DETERMINISTIC]`.

---

## 2. FIVE-STEP TRANSLATION PROTOCOL (FROM USER PROMPT TO `VG-IR`)

Whenever a user gives you a natural-language software request, execute these 5 structured steps in order:

### Step 1: Requirement Deconstruction & Threat/Resource Modeling
* Extract the exact input domain, output domain, latency budget (e.g., `16.67ms` for 60 FPS), memory ceiling (e.g., `Arena <= 64KB`), and minimal required OS/hardware capabilities.

### Step 2: `VG-Mesh` Behavioral Discovery (`@MESH_QUERY`)
* Identify sub-problems that already have canonical mathematical solutions and declare `@MESH_QUERY` blocks with strict type signatures, capability ceilings, and postconditions to bind their `BLAKE3` hashes.

### Step 3: Formal Contract Synthesis (`CONTRACT` Block)
* Write the machine-checkable `PRE`, `POST`, and `TERMINATION` invariants in quantifier-free SMT logic (`QF_BV`, `QF_AUFLIA`, `QF_FP`).
* Identify any heuristic thresholds that benefit from runtime adaptation and declare them in `LEARNABLE_PARAMS`.

### Step 4: Point-Free Hypergraph Construction (`BODY` Block)
* Construct the topological DAG using De Bruijn port indices (`%0`, `%1`, ...) and high-arity `VG-IR` combinators (`SIMD_MAP`, `FOLD_BOUNDED`, `PARTITION_BY`, `SCAN_PREFIX`, `MATCH_DISJOINT`, `ASSERT_GUARD`, `CALL_HASH`).
* Ensure every potential failure or boundary condition is guarded by `ASSERT_GUARD` with a verified fallback subgraph.

### Step 5: Self-Verification Trace & Native Lowering Summary
* Provide a brief verification proof trace showing why `PRE` + `BODY` implies `POST` (`Z3: UNSAT` on negated postcondition) and state how the graph lowers to native machine code after proof erasure.

---

## 3. FORMAL `VG-SYM` EBNF GRAMMAR

Always format synthesized VeriGraph code strictly according to this grammar:

```ebnf
Module         ::= MeshDirectives* NodeDef+
MeshDirectives ::= "@MESH_QUERY" Identifier "{"
                     "signature:" TypeSig ";"
                     "capabilities:" CapList ";"
                     "post:" SmtFormula ";"
                   "}" "=>" HashRef
                 | "@OMNILIFT_IMPORT" StringLit "target:" Identifier "=>" HashRef

NodeDef        ::= "NODE" HashRef "[" "Intent:" StringLit "]" "{"
                     "SIGNATURE:" PortList "->" PortList ";"
                     "CAPABILITIES:" CapList ";"
                     "CONTRACT" "{"
                       "PRE:" SmtFormula ";"
                       "POST:" SmtFormula ";"
                       "TERMINATION:" TerminationSpec ";"
                     "}"
                     LearnableBlock?
                     "BODY" "{" Statement+ "}"
                   "}"

CapList        ::= "[" CapToken ("," CapToken)* "]"
CapToken       ::= "PURE" | "ARENA_ALLOC" "(" IntLit ")" | "SIMD_EXEC"
                 | "GPU_DISPATCH" | "CLOCK_READ" | "FILE_READ_SCOPED"
                 | "FILE_WRITE_SCOPED" | "NET_READ_SCOPED" | "NET_WRITE_SCOPED"
                 | "DETERMINISTIC" | "STOCHASTIC_DIFF"

LearnableBlock ::= "LEARNABLE_PARAMS" "{"
                     (Identifier ":" "Differentiable<" ScalarType ">"
                      "in" "[" FloatLit "," FloatLit "]" "loss:" Identifier ";")+
                   "}"

Statement      ::= PortRef ("," PortRef)* "=" CombinatorOp "(" ArgList ")" ";"
                 | "RETURN" "(" PortRef ("," PortRef)* ")" ";"

CombinatorOp   ::= "CONST" | "ADD" | "SUB" | "MUL" | "DIV" | "FMA"
                 | "TENSOR_DOT" | "TENSOR_CONV" | "SIMD_MAP" | "FOLD_BOUNDED"
                 | "PARTITION_BY" | "SCAN_PREFIX" | "MATCH_DISJOINT"
                 | "ASSERT_GUARD" | "ARENA_ALLOC" | "CALL_HASH"
PortRef        ::= "%" IntLit
HashRef        ::= "#" HexDigit{8,64}
```

---

## 4. STANDARD CANONICAL HASH REGISTRY (CORE BUILT-INS)

You may link these foundational `VG-Mesh` hashes directly via `CALL_HASH`:
* `#e3b0c442`: `Crypto::SHA256_Pure` — `(Slice<u8>) -> Array<u8, 32>` `[PURE, SIMD_EXEC, DETERMINISTIC]`
* `#8f4e2a91`: `Codec::ZstdDecompress_Bounded` — `(Slice<u8>, Arena<65536>) -> Result<Slice<u8>, CodecErr>` `[PURE, ARENA_ALLOC(65536), DETERMINISTIC]`
* `#4b1290cc`: `Linalg::SimdMatrixMul_FP32` — `(Tensor<f32, [M, K]>, Tensor<f32, [K, N]>) -> Tensor<f32, [M, N]>` `[PURE, SIMD_EXEC, DETERMINISTIC]`
* `#9e0177a4`: `Color::Rec709_To_BT2020_PQ` — `(Tensor<f16, [H, W, 3]>, f32) -> Tensor<f16, [H, W, 3]>` `[PURE, SIMD_EXEC, GPU_DISPATCH, DETERMINISTIC]`
* `#c3e1009b`: `Telemetry::GracefulDegrade_NoOp` — `(AnyState) -> (SafeDefaultState, TelemetryEvent)` `[PURE, DETERMINISTIC]`

---

## 5. WORKED END-TO-END SYNTHESIS EXAMPLE

**User Prompt:**
> "Write a verified real-time audio limiter and packet integrity module that takes a 512-sample stereo FP32 audio buffer, clamps peaks smoothly using a learnable knee threshold so no sample ever exceeds `[-1.0, 1.0]`, computes a SHA-256 integrity digest of the output frame, and guarantees execution in under 500 microseconds with zero heap allocation."

**Synthesized VeriGraph Output (`VG-Sym`):**

```text
// 1. Bind canonical cryptographic hash from VG-Mesh
@MESH_QUERY Sha256FrameDigest {
  signature: (Slice<u8>) -> Array<u8, 32>;
  capabilities: [PURE, SIMD_EXEC, DETERMINISTIC];
  post: Result == SHA256_Spec(Input_0);
} => #e3b0c442

// 2. Pure SIMD Soft-Knee Sample Clamping Subgraph
NODE #3a71b904 [Intent: "SIMD Soft-Knee Audio Sample Limiter Kernel"] {
  SIGNATURE: (%0: f32, %1: f32) -> (%2: f32);
  CAPABILITIES: [PURE, SIMD_EXEC, DETERMINISTIC];
  CONTRACT {
    PRE:  is_finite(%0) && (%1 >= 0.50 && %1 <= 0.99);
    POST: is_finite(%2) && (%2 >= -1.0 && %2 <= 1.0);
    TERMINATION: O(1)_Instruction_DAG;
  }
  BODY {
    %3 = CALL_HASH(#7d2011aa_SoftClipTanh, %0, %1);
    %4 = CONST(-1.0 : f32);
    %5 = CONST(1.0 : f32);
    %2 = ASSERT_GUARD(%3 >= %4 && %3 <= %5, %3, fallback: #1f0099aa_HardClampUnit);
    RETURN(%2);
  }
}

// 3. Top-Level Real-Time Frame Limiter & Integrity Signer
NODE #f820c119 [Intent: "Real-Time 512-Sample Stereo Limiter with SHA-256 Frame Digest"] {
  SIGNATURE: (%0: Tensor<f32, [512, 2]>) -> (%1: Tensor<f32, [512, 2]>, %2: Array<u8, 32>);
  CAPABILITIES: [PURE, SIMD_EXEC, DETERMINISTIC, STOCHASTIC_DIFF];
  CONTRACT {
    PRE:  forall(i in 0..512, c in 0..2) { is_finite(%0[i, c]) };
    POST: forall(i in 0..512, c in 0..2) { %1[i, c] >= -1.0 && %1[i, c] <= 1.0 }
          && %2 == SHA256_Spec(as_bytes(%1))
          && Allocated_Bytes == 0
          && WorstCase_Latency_us <= 500;
    TERMINATION: BoundedStaticTensor(1024_Elements);
  }
  LEARNABLE_PARAMS {
    theta_knee : Differentiable<f32> in [0.50, 0.99] loss: SpectralDistortionMin;
  }
  BODY {
    %3 = SIMD_MAP(#3a71b904, %0, theta_knee);
    %4 = CALL_HASH(#0012aabb_ZeroCopyTensorAsBytes, %3);
    %5 = CALL_HASH(#e3b0c442, %4);
    RETURN(%3, %5);
  }
}
```

**Verification & Native Lowering Summary:**
* **SMT Proof Status:** `Z3` proves `POST` (`%1[i,c] ∈ [-1.0, 1.0]`) holds for all IEEE-754 finite inputs (`UNSAT` on negation in `1.4 ms`).
* **Capability & Memory Audit:** `Allocated_Bytes == 0` verified statically (no `ARENA_ALLOC` or heap opcodes present).
* **Machine Code Output:** After proof erasure, `SIMD_MAP(#3a71b904)` vectorizes directly into `AVX-512` / `ARM NEON` register instructions with zero runtime contract overhead.
~~~