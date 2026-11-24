/* A 256 x 256 f32 matrix product: kernel.vgs against the same i-j-k loop in C, each cell summed
 * in k order, with no contraction on either side (-ffp-contract=off), so the floats agree. */
#include "../bench.h"
#include "vg_rt.h"

extern int32_t VG_ENTRY(vg_ctx *ctx, const float a[65536], const float b[65536], float out[65536]);

static float a[65536], b[65536], out_vg[65536], out_c[65536];
static vg_ctx ctx;

__attribute__((noinline)) static void c_matmul(const float *x, const float *y, float *z) {
    for (int i = 0; i < 256; ++i)
        for (int j = 0; j < 256; ++j) {
            float acc = 0.0f;
            for (int k = 0; k < 256; ++k) acc = acc + x[i * 256 + k] * y[k * 256 + j];
            z[i * 256 + j] = acc;
        }
}

static void run_vg(void) {
    ctx.arena_used = 0;
    if (VG_ENTRY(&ctx, a, b, out_vg) != 0) abort();
    BENCH_BARRIER();
}

static void run_c(void) {
    c_matmul(a, b, out_c);
    BENCH_BARRIER();
}

int main(void) {
    uint64_t s = 0x3A72026;
    /* Values in [-1, 1) on a 2^-23 grid: exact in f32. */
    for (int i = 0; i < 65536; ++i) a[i] = (float)((int64_t)(bench_mix(&s) >> 40) - (1 << 23)) / (float)(1 << 23);
    for (int i = 0; i < 65536; ++i) b[i] = (float)((int64_t)(bench_mix(&s) >> 40) - (1 << 23)) / (float)(1 << 23);
    vg_rt_init(&ctx, 1u << 23, VG_TARGET);
    run_vg();
    run_c();
    uint64_t fv = bench_fnv(out_vg, sizeof out_vg), fc = bench_fnv(out_c, sizeof out_c);
    int identical = fv == fc && fc == (uint64_t)EXPECTED_FNV;
    fprintf(stderr, "matmul: fnv vg %016llx c %016llx expected %016llx\n", (unsigned long long)fv,
            (unsigned long long)fc, (unsigned long long)EXPECTED_FNV);
    bench_report("matmul", identical, run_vg, run_c);
    return 0;
}
