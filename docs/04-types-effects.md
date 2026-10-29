# 04 · Types, effects and capabilities

Types say what a value is, effects say what a node may do, and capability handles say what it may touch. A node with an empty effect set is pure, and vg-types enforces all three before any proof runs.

## Types

| Type | Meaning | Phase |
| --- | --- | --- |
| Bool, u8–u64, i8–i64 | Fixed-width bitvectors | [MVP] |
| f16, bf16, f32, f64 | IEEE-754 floats with one NaN (docs/13); f16 and bf16 are storage-only in the MVP and reach arithmetic only through CAST | [MVP] |
| Array<T, N> | Fixed-length value, N static | [MVP] |
| Slice<T> | Length-carrying view into the call's arena or an input buffer | [MVP] |
| Tuple<…>, Result<T, E> | Products and error results; Results are built by OK and ERR and read by IS_OK, GET_OK and GET_ERR (docs/14) | [MVP] |
| {x: T \| φ} | Refinement type; φ becomes an obligation wherever a value enters it. In the MVP only at the top of a SIGNATURE, where a refined parameter adds to PRE and may mention the parameters before it, and a refined result adds to POST | [MVP] |
| Arena<N> | Explicit, scoped region of N bytes; the MVP uses one implicit arena per exported call instead (ADR 0003) | [ALPHA] |
| Eff | Effect token that orders side effects | [MVP] |
| FileIn, FileOut, Socket, Clock, GpuQueue | Capability handles; only the runtime creates them | [MVP]: files and clock |
| Tensor<T, [d…]> | Shape-checked n-dimensional array; static dims in the MVP, symbolic dims in the alpha | [MVP] static; [ALPHA] |
| Sum types, generics | Tagged unions for MATCH_DISJOINT; type parameters monomorphized at link time | [ALPHA] |
| Distribution<T> | Sampling as a typed value | [R-07] |

## Effects (u32 mask)

| Bit | Effect | What the node may do | OS sandbox grant |
| --- | --- | --- | --- |
| 0x0001 | ARENA | Allocate in the call's arena (docs/13); the byte budget lives in COST | None |
| 0x0002 | CLOCK | Read a monotonic clock | None |
| 0x0004 | FILE_READ | Read through a FileIn handle it was given | Read access to that file |
| 0x0008 | FILE_WRITE | Write through a FileOut handle | Write access to that file |
| 0x0010 | NET_READ | Read from a Socket handle | Network access to that peer |
| 0x0020 | NET_WRITE | Write to a Socket handle | Network access to that peer |
| 0x0040 | GPU | Dispatch bounded kernels on a GpuQueue | GPU device access |
| 0x0080 | NONDET | Randomness, sampling, scheduling races | None |
| 0x0100 | PARAM_UPDATE | Write learnable parameters | Write to the parameter file |
| 0x0200 | TELEMETRY | Emit structured telemetry events | Write to the log sink |
| 0x0400 | HOST_CALL | Call trusted host code across the FFI boundary | Per binding |
| 0x0800 | SPAWN | Start named processes, such as clang and the solvers | Exec of those binaries |

Bits 0x1000 and up are reserved and must be zero.

## Rules

- **PURE is the empty set**, and DETERMINISTIC means NONDET is absent. Neither is a bit, so the source spec's contradictory lists, such as `[PURE, SIMD_EXEC]` or `[DETERMINISTIC, STOCHASTIC_DIFF]`, cannot be written.
- **Transitivity:** a caller's mask must contain every callee's mask (`caller & callee == callee`). vg-types checks this at every CALL_HASH, and SIMD_MAP and FOLD_BOUNDED count their kernels as callees.
- **SIMD is a target feature**, chosen by codegen, not an effect. A scalar caller may call a vectorized kernel.
- **Budgets live in COST**, not in the mask: `ARENA` plus `COST: arena_bytes <= 65536`.
- **Authority comes from handles**, not bits. A node can read a file only if a FileIn handle reaches it through the dataflow, and the runtime creates handles at the root from the program's manifest. A mask bit without a handle grants nothing; a handle without the bit is a type error.
- **Unique values update in place.** An UPDATE whose collection is the result of an allocating op with exactly one use compiles to a plain store; any other UPDATE of a slice, including one of a view or an input, copies it into the arena (ADR 0003). A slice returned by a node views an input or the arena, never a value of the node's own frame (VG-T0120).
- **Eff is linear.** Each Eff value is used exactly once: by an effect op, a call or RETURN. A node whose mask has any bit other than ARENA takes Eff as its first parameter and returns the next token as its first result, so a caller's effects and its callees' share one chain.
- **Kernels are pure and fixed-size.** The kernels of SIMD_MAP, FOLD_BOUNDED and GENERATE have an empty mask, fixed-size types and one result (docs/14), so loops never allocate and never perform effects. ASSERT_GUARD's branches are ordinary callees and may do both.
- **Where handles and Eff may appear.** As top-level parameters and results, as call arguments and as effect-op operands; never inside a tuple, a Result, a collection or a kernel signature (VG-T0119).
- **Collections.** `Array<T, N>` and `Tensor<T, [d…]>` have static lengths (N, or the product of the dims); a `Slice<T>` carries its length. LEN, INDEX, UPDATE, SLICE and FOLD_BOUNDED take arrays and slices, and SIMD_MAP also maps a tensor element by element. SLICE of an array is a slice view, which is how an array reaches a `Slice<T>` parameter: there are no implicit conversions. AS_BYTES of a fixed-width scalar, array, tensor or slice is a `Slice<u8>` view of its bytes, little-endian and row-major, with no copy, so `@limit_frame` can pass `AS_BYTES(%0)` to `@sha256`; the formula built-in `as_bytes` means the same.
- **Effect ops** take and return `Eff`; vg-types rejects any effect op whose token does not come from the node's Eff input or a previous effect op.

## Roots and manifests

An executable's root is an exported node whose parameters are one Eff followed by handles only, and whose results are Eff and `Result<u32, u32>`: Ok exits with status 0, and Err(e) writes e to the telemetry sink and exits with status 1. Data enters through FileIn handles; `stdin` and `stdout` are files like any other. `vg build --root @main` writes the manifest that binds each handle parameter:

```toml
format = 1
root = "@main"                       # resolved through vg.lock
[[handle]]
param = "input"                      # a root parameter name
kind = "FileIn"
path = { argv = 1 }                  # or { fixed = "/abs/path" }, or "stdin"
[[handle]]
param = "out"
kind = "FileOut"
path = "stdout"
[[handle]]
param = "clock"
kind = "Clock"
```

vg-rt opens exactly these handles before it calls the root, and nothing else is reachable: a node can touch only what a handle reaching it through the dataflow names.

## Sandbox manifests

vg-codegen derives an OS sandbox from the root node's handles and effects, so the guarantee holds for the whole process, not only inside the language. The design follows the capability model of the language itself: vg-rt **opens every handle first, then drops all other authority**, so the process keeps exactly the file descriptors its manifest names, whatever paths argv supplied.

- **Linux:** after opening the handles, vg-rt applies a Landlock ruleset that handles every filesystem right and grants none, so no new file can be opened, plus the TCP rights when the root has no NET effect (Landlock ABI v4, Linux 6.7; earlier kernels fall back to the seccomp filter for the network), and a seccomp filter that makes `socket` fail with EPERM. Landlock does not restrict descriptors that are already open.
- **macOS:** the same order with `sandbox_init` and a deny-by-default profile generated from the manifest. Apple has deprecated that API, and App Sandbox entitlements, its supported replacement, take effect at launch, so they can grant only fixed paths or the app container: a command-line tool has no file picker for "user-selected" grants. P4.6 decides between the two on the pinned macOS and records the evidence; until then a macOS root binds only fixed paths.
- **WASM:** wasmtime gets `--dir` only for the granted paths and no sockets, and WASI preview 1 is capability-based already.

G4 requires one test per target in which the binary tries an ungranted action and the OS refuses it; the test fails, never skips, when the kernel lacks the needed Landlock ABI.
