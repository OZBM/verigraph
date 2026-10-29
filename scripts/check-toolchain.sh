#!/bin/sh
# Compare the tools on this host with toolchain.lock. Prints one line per tool and exits 1
# if any tool is missing, unpinned or at a different version than the lock records.
set -u
cd "$(dirname "$0")/.."

found() {
  case "$1" in
    rustc)    rustc --version 2>/dev/null | awk '{print $2}' ;;
    # Apple clang reports its own version line; keep the full line so it is never mistaken for LLVM clang.
    clang)    v=$(clang --version 2>/dev/null | head -n 1)
              case "$v" in
                "Apple clang"*) echo "apple-$(echo "$v" | awk '{print $4}')" ;;
                *clang\ version*) echo "$v" | sed -n 's/.*clang version \([0-9][0-9.]*\).*/\1/p' ;;
              esac ;;
    z3)       z3 --version 2>/dev/null | awk '{print $3}' ;;
    cvc5)     cvc5 --version 2>/dev/null | head -n 1 | sed -n 's/.*version \([0-9][0-9.]*\).*/\1/p' ;;
    bitwuzla) bitwuzla --version 2>/dev/null | head -n 1 ;;
    wasi-sdk) cat "${WASI_SDK_PATH:-/opt/wasi-sdk}/VERSION" 2>/dev/null | head -n 1 ;;
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
