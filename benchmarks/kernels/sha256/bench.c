/* SHA-256 over 1 MiB of seeded bytes: examples/sha256.vgs (@sha256_impl) against a plain C
 * FIPS 180-4 implementation. */
#include "../bench.h"
#include "vg_rt.h"

extern int32_t VG_ENTRY(vg_ctx *ctx, const vg_slice_u8 *m, uint8_t out[32]);

#define N (1u << 20)

static uint8_t data[N];
static vg_ctx ctx;
static vg_slice_u8 slice;
static uint8_t out_vg[32], out_c[32];

static const uint32_t K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};

#define ROR(x, n) (((x) >> (n)) | ((x) << (32 - (n))))

static void compress(uint32_t h[8], const uint8_t *b) {
    uint32_t w[64];
    for (int t = 0; t < 16; ++t)
        w[t] = (uint32_t)b[4 * t] << 24 | (uint32_t)b[4 * t + 1] << 16 | (uint32_t)b[4 * t + 2] << 8 | b[4 * t + 3];
    for (int t = 16; t < 64; ++t) {
        uint32_t s0 = ROR(w[t - 15], 7) ^ ROR(w[t - 15], 18) ^ (w[t - 15] >> 3);
        uint32_t s1 = ROR(w[t - 2], 17) ^ ROR(w[t - 2], 19) ^ (w[t - 2] >> 10);
        w[t] = w[t - 16] + s0 + w[t - 7] + s1;
    }
    uint32_t a = h[0], bb = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
    for (int t = 0; t < 64; ++t) {
        uint32_t t1 = hh + (ROR(e, 6) ^ ROR(e, 11) ^ ROR(e, 25)) + ((e & f) ^ (~e & g)) + K[t] + w[t];
        uint32_t t2 = (ROR(a, 2) ^ ROR(a, 13) ^ ROR(a, 22)) + ((a & bb) ^ (a & c) ^ (bb & c));
        hh = g; g = f; f = e; e = d + t1; d = c; c = bb; bb = a; a = t1 + t2;
    }
    h[0] += a; h[1] += bb; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
}

__attribute__((noinline)) static void c_sha256(const uint8_t *m, size_t n, uint8_t out[32]) {
    uint32_t h[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                     0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    size_t i = 0;
    for (; i + 64 <= n; i += 64) compress(h, m + i);
    uint8_t tail[128] = {0};
    size_t r = n - i;
    memcpy(tail, m + i, r);
    tail[r] = 0x80;
    size_t len = r < 56 ? 64 : 128;
    uint64_t bits = (uint64_t)n * 8;
    for (int k = 0; k < 8; ++k) tail[len - 1 - k] = (uint8_t)(bits >> (8 * k));
    compress(h, tail);
    if (len == 128) compress(h, tail + 64);
    for (int k = 0; k < 8; ++k)
        for (int j = 0; j < 4; ++j) out[4 * k + j] = (uint8_t)(h[k] >> (24 - 8 * j));
}

static void run_vg(void) {
    ctx.arena_used = 0;
    if (VG_ENTRY(&ctx, &slice, out_vg) != 0) abort();
    BENCH_BARRIER();
}

static void run_c(void) {
    c_sha256(data, N, out_c);
    BENCH_BARRIER();
}

int main(void) {
    uint64_t s = 0x5A2026;
    for (size_t i = 0; i < N; i += 8) {
        uint64_t r = bench_mix(&s);
        memcpy(data + i, &r, 8);
    }
    slice.ptr = data;
    slice.len = N;
    vg_rt_init(&ctx, 1u << 23, VG_TARGET);
    run_vg();
    run_c();
    static const char *want = EXPECTED_DIGEST;
    char hex[65];
    for (int k = 0; k < 32; ++k) snprintf(hex + 2 * k, 3, "%02x", out_c[k]);
    int identical = memcmp(out_vg, out_c, 32) == 0 && strcmp(hex, want) == 0;
    fprintf(stderr, "sha256: c %s, vg %s the C digest, expected %s\n", hex,
            memcmp(out_vg, out_c, 32) == 0 ? "equals" : "differs from", want);
    bench_report("sha256", identical, run_vg, run_c);
    return 0;
}
