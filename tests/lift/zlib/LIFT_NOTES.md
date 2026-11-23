# Lifting Notes for zlib 1.4

## `crc32_z` (`crc32.c`)

`crc32.c` selects between three implementations at preprocess time:
1. `ARMCRC32` hardware CRC-32 instructions when `__aarch64__` and `__ARM_FEATURE_CRC32` are defined and `W == 8`.
2. A braided multi-word loop (`W == 8`, `N == 5`) using `crc_braid_table` and `crc_word`.
3. The standard byte-wise lookup-table loop over `crc_table[256]`.

Lines 81-84 of `crc32.c` check:
```c
#ifdef Z_TESTW
#  if Z_TESTW-1 != -1
#    define W Z_TESTW
#  endif
```
Passing `-DZ_TESTW=0` evaluates `0 - 1 != -1` as false, leaving both `W` and `ARMCRC32` undefined.
`crc32_z` then compiles purely to the byte-wise table loop (an unrolled 8-byte fast path plus a 1-byte tail loop) over `crc_table[256]`.

## `adler32_z` (`adler32.c`)

Compiled directly with `-Itests/lift/zlib`, keeping hardware division (`MOD(a)` as `a %= 65521U`) and the three length regimes (`len == 1`, `len < 16`, `len >= NMAX = 5552`, and `len < NMAX` with 16-byte inner unroll + 1-byte tail).
