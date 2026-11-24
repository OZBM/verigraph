/* A saturating 8-bit blend of two 1 MiB images: kernel.vgs against a plain C loop. */
#include "../bench.h"
#include "vg_rt.h"

#define N (1u << 20)

extern int32_t VG_ENTRY(vg_ctx *ctx, const uint8_t a[N], const uint8_t b[N], uint8_t out[N]);

static uint8_t a[N], b[N], out_vg[N], out_c[N];
static vg_ctx ctx;

__attribute__((noinline)) static void c_blend(const uint8_t *x, const uint8_t *y, uint8_t *z) {
    for (size_t i = 0; i < N; ++i) {
        unsigned s = (unsigned)x[i] + y[i];
        z[i] = (uint8_t)(s > 255 ? 255 : s);
    }
}

static void run_vg(void) {
    ctx.arena_used = 0;
    if (VG_ENTRY(&ctx, a, b, out_vg) != 0) abort();
    BENCH_BARRIER();
}

static void run_c(void) {
    c_blend(a, b, out_c);
    BENCH_BARRIER();
}

int main(void) {
    uint64_t s = 0xB1E2026;
    for (size_t i = 0; i < N; i += 8) {
        uint64_t r = bench_mix(&s);
        memcpy(a + i, &r, 8);
        r = bench_mix(&s);
        memcpy(b + i, &r, 8);
    }
    vg_rt_init(&ctx, 1u << 23, VG_TARGET);
    run_vg();
    run_c();
    uint64_t fv = bench_fnv(out_vg, N), fc = bench_fnv(out_c, N);
    int identical = fv == fc && fc == (uint64_t)EXPECTED_FNV;
    fprintf(stderr, "blend: fnv vg %016llx c %016llx expected %016llx\n", (unsigned long long)fv,
            (unsigned long long)fc, (unsigned long long)EXPECTED_FNV);
    bench_report("blend", identical, run_vg, run_c);
    return 0;
}
