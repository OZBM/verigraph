/* One 256 x 192 frame of examples/mandelbrot.vgs (@render) against the same arithmetic in C, op
 * for op, so the RGBA bytes agree. */
#include "../bench.h"
#include "vg_rt.h"

#define PIXELS (256 * 192)

extern int32_t VG_ENTRY(vg_ctx *ctx, float cx, float cy, float scale, uint32_t out[PIXELS]);

static uint32_t out_vg[PIXELS], out_c[PIXELS];
static vg_ctx ctx;
static const float CX = -0.75f, CY = 0.0f, SCALE = 0.013671875f;

static uint32_t palette_entry(uint64_t j) {
    float t = ((float)j + 0.5f) / 16.0f, u = 1.0f - t;
    float t2 = t * t, t3 = t2 * t, u2 = u * u, u3 = u2 * u;
    uint8_t r = (uint8_t)((u * t3) * 2295.0f);
    uint8_t g = (uint8_t)((u2 * t2) * 3825.0f);
    uint8_t b = (uint8_t)((u3 * t) * 2167.5f);
    return (uint32_t)r | (uint32_t)g << 8 | (uint32_t)b << 16 | 0xFF000000u;
}

__attribute__((noinline)) static void c_render(float cx, float cy, float scale, uint32_t *out) {
    uint32_t palette[16];
    for (uint64_t j = 0; j < 16; ++j) palette[j] = palette_entry(j);
    for (uint64_t i = 0; i < PIXELS; ++i) {
        float x = (float)(i % 256), y = (float)(i / 256);
        float cr = cx + (x - 128.0f) * scale, ci = cy - (y - 96.0f) * scale;
        float zr = 0.0f, zi = 0.0f;
        uint32_t n = 0;
        for (int k = 0; k < 64; ++k) {
            float zr2 = zr * zr, zi2 = zi * zi;
            int inside = zr2 + zi2 <= 4.0f;
            float nzr = (zr2 - zi2) + cr;
            float p = zr * zi;
            float nzi = (p + p) + ci;
            if (inside) {
                zr = nzr;
                zi = nzi;
                n = n + 1;
            }
        }
        out[i] = n == 64 ? 0xFF000000u : palette[n & 15];
    }
}

static void run_vg(void) {
    ctx.arena_used = 0;
    if (VG_ENTRY(&ctx, CX, CY, SCALE, out_vg) != 0) abort();
    BENCH_BARRIER();
}

static void run_c(void) {
    c_render(CX, CY, SCALE, out_c);
    BENCH_BARRIER();
}

int main(void) {
    vg_rt_init(&ctx, 1u << 23, VG_TARGET);
    run_vg();
    run_c();
    uint64_t fv = bench_fnv(out_vg, sizeof out_vg), fc = bench_fnv(out_c, sizeof out_c);
    int identical = fv == fc && fc == (uint64_t)EXPECTED_FNV;
    fprintf(stderr, "mandelbrot: fnv vg %016llx c %016llx expected %016llx\n", (unsigned long long)fv,
            (unsigned long long)fc, (unsigned long long)EXPECTED_FNV);
    bench_report("mandelbrot", identical, run_vg, run_c);
    return 0;
}
