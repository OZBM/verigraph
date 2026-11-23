#!/bin/sh
# Compare the tools on this host with toolchain.lock. Prints one line per tool and exits 1
# if any tool is missing, unpinned or at a different version than the lock records.
set -u
cd "$(dirname "$0")/.."

found() {
  case "$1" in
    rustc)    rustc --version 2>/dev/null | awk '{print $2}' ;;
    # The LLVM clang (19 or newer) that builds wasm32-wasip1; Homebrew's llvm formula by default.
    clang)    v=$("${LLVM_CLANG:-/opt/homebrew/opt/llvm/bin/clang}" --version 2>/dev/null | head -n 1)
              echo "$v" | sed -n 's/.*clang version \([0-9][0-9.]*\).*/\1/p' ;;
    # The host clang that builds aarch64-apple-darwin and the x86_64 module objects. Apple clang
    # reports its own version line; keep it apart from LLVM's.
    host-clang) v=$(clang --version 2>/dev/null | head -n 1)
              case "$v" in
                "Apple clang"*) echo "apple-$(echo "$v" | awk '{print $4}')" ;;
                *clang\ version*) echo "$v" | sed -n 's/.*clang version \([0-9][0-9.]*\).*/\1/p' ;;
              esac ;;
    z3)       z3 --version 2>/dev/null | awk '{print $3}' ;;
    cvc5)     cvc5 --version 2>/dev/null | head -n 1 | sed -n 's/.*version \([0-9][0-9.]*\).*/\1/p' ;;
    bitwuzla) bitwuzla --version 2>/dev/null | head -n 1 ;;
    # The WASI sysroot and compiler-rt builtins (Homebrew's wasi-libc and wasi-runtimes).
    wasi-libc|wasi-runtimes) d=$(readlink "/opt/homebrew/opt/$1" 2>/dev/null) && basename "$d" ;;
    # zig cc links x86_64-unknown-linux-gnu against the glibc it carries.
    zig)      zig version 2>/dev/null ;;
    wasmtime) wasmtime --version 2>/dev/null | awk '{print $2}' ;;
  esac
}

status=0
while read -r tool pinned; do
  case "$tool" in ''|'#'*) continue ;; esac
  have=$(found "$tool")
  if [ -z "$have" ]; then
    echo "MISSING   $tool: not installed (lock: $pinned)"; status=1
  elif [ "$pinned" = none ]; then
    echo "UNPINNED  $tool: found $have, lock has none"; status=1
  elif [ "$have" != "$pinned" ]; then
    echo "MISMATCH  $tool: found $have, lock has $pinned"; status=1
  else
    echo "OK        $tool $have"
  fi
done < toolchain.lock
exit $status
