/* CRC-32 over 1 MiB of seeded bytes: kernel.vgs against zlib's table-driven loop. */
#include "../bench.h"
#include "vg_rt.h"

extern int32_t VG_ENTRY(vg_ctx *ctx, const vg_slice_u8 *data, uint32_t *out);

#define N (1u << 20)

static uint8_t data[N];
static uint32_t table[256];
static vg_ctx ctx;
static vg_slice_u8 slice;
static uint32_t out_vg, out_c;

__attribute__((noinline)) static uint32_t c_crc32(const uint8_t *p, size_t n) {
    uint32_t c = 0xFFFFFFFFu;
    for (size_t i = 0; i < n; ++i) {
        c = table[(c ^ p[i]) & 0xFFu] ^ (c >> 8);
    }
    return c ^ 0xFFFFFFFFu;
}

static void run_vg(void) {
    ctx.arena_used = 0;
    if (VG_ENTRY(&ctx, &slice, &out_vg) != 0) abort();
    BENCH_BARRIER();
}

static void run_c(void) {
    out_c = c_crc32(data, N);
    BENCH_BARRIER();
}

int main(void) {
    uint64_t s = 0xC3C2026;
    for (size_t i = 0; i < N; i += 8) {
        uint64_t r = bench_mix(&s);
        memcpy(data + i, &r, 8);
    }
    for (uint32_t n = 0; n < 256; ++n) {
        uint32_t c = n;
        for (int k = 0; k < 8; ++k) c = (c & 1u) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
        table[n] = c;
    }
    slice.ptr = data;
    slice.len = N;
    vg_rt_init(&ctx, 1u << 23, VG_TARGET);
    run_vg();
    run_c();
    int identical = out_vg == out_c && out_c == (uint32_t)EXPECTED_CRC;
    fprintf(stderr, "crc32: vg %08x c %08x expected %08x\n", out_vg, out_c, (uint32_t)EXPECTED_CRC);
    bench_report("crc32", identical, run_vg, run_c);
    return 0;
}
