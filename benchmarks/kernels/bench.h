/* The measurement protocol of docs/16 ("Benchmarks"), shared by every kernel's bench.c: the two
 * implementations are called in one process, alternating for 10 rounds; each round makes 100
 * warm-up calls, then calls until 1 second or 1,000 calls have passed, timing each call with the
 * monotonic clock. The statistic is the median call time per side; the ratio is
 * median(VeriGraph) / median(C), with a seeded bootstrap 95% interval. One JSON line goes to
 * stdout. Inputs come from a seeded SplitMix64, so every machine measures the same data. */
#ifndef VG_BENCH_H
#define VG_BENCH_H

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifndef VG_TARGET
#define VG_TARGET "unknown"
#endif

/* A compiler barrier: the call's results are treated as read and its inputs as changed, so the
 * optimizer can neither drop a call nor hoist it out of the timing loop. */
#define BENCH_BARRIER() __asm__ volatile("" ::: "memory")

static uint64_t bench_mix(uint64_t *s) {
    uint64_t z = (*s += 0x9E3779B97F4A7C15ull);
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
    return z ^ (z >> 31);
}

/* FNV-1a over an output, for expected values too large to write down. */
static uint64_t bench_fnv(const void *p, size_t n) {
    const uint8_t *b = (const uint8_t *)p;
    uint64_t h = 0xcbf29ce484222325ull;
    for (size_t i = 0; i < n; ++i) {
        h = (h ^ b[i]) * 0x100000001b3ull;
    }
    return h;
}

static uint64_t bench_now(void) {
#if defined(__APPLE__)
    /* CLOCK_MONOTONIC ticks in microseconds on macOS; this monotonic clock has nanoseconds. */
    return clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
#endif
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

typedef struct {
    uint64_t *t;
    size_t n, cap;
} bench_samples;

static void bench_push(bench_samples *s, uint64_t v) {
    if (s->n == s->cap) {
        s->cap = s->cap ? 2 * s->cap : 1024;
        s->t = (uint64_t *)realloc(s->t, s->cap * sizeof(uint64_t));
        if (!s->t) {
            abort();
        }
    }
    s->t[s->n++] = v;
}

static void bench_side(void (*f)(void), bench_samples *out) {
    for (int i = 0; i < 100; ++i) {
        f();
    }
    uint64_t start = bench_now();
    for (int i = 0; i < 1000; ++i) {
        uint64_t a = bench_now();
        f();
        uint64_t b = bench_now();
        bench_push(out, b - a);
        if (b - start >= 1000000000ull) {
            break;
        }
    }
}

static int bench_cmp_u64(const void *a, const void *b) {
    uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
    return (x > y) - (x < y);
}

static int bench_cmp_double(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}

static double bench_median(uint64_t *v, size_t n) {
    qsort(v, n, sizeof(uint64_t), bench_cmp_u64);
    return n % 2 ? (double)v[n / 2] : ((double)v[n / 2 - 1] + (double)v[n / 2]) / 2.0;
}

/* Measures and prints one kernel's JSON line. */
static void bench_report(const char *name, int identical, void (*vg)(void), void (*c)(void)) {
    bench_samples sv = {0}, sc = {0};
    for (int r = 0; r < 10; ++r) {
        bench_side(vg, &sv);
        bench_side(c, &sc);
    }
    uint64_t *tv = (uint64_t *)malloc(sv.n * sizeof(uint64_t));
    uint64_t *tc = (uint64_t *)malloc(sc.n * sizeof(uint64_t));
    double ratios[1000];
    uint64_t seed = 0x2026112400000016ull;
    for (int b = 0; b < 1000; ++b) {
        for (size_t i = 0; i < sv.n; ++i) {
            tv[i] = sv.t[bench_mix(&seed) % sv.n];
        }
        for (size_t i = 0; i < sc.n; ++i) {
            tc[i] = sc.t[bench_mix(&seed) % sc.n];
        }
        ratios[b] = bench_median(tv, sv.n) / bench_median(tc, sc.n);
    }
    qsort(ratios, 1000, sizeof(double), bench_cmp_double);
    double mv = bench_median(sv.t, sv.n), mc = bench_median(sc.t, sc.n);
    printf("{\"name\":\"%s\",\"target\":\"%s\",\"identical_output\":%s,\"median_vg_ns\":%.0f,"
           "\"median_c_ns\":%.0f,\"ratio\":%.4f,\"ci_low\":%.4f,\"ci_high\":%.4f,"
           "\"calls_vg\":%zu,\"calls_c\":%zu}\n",
           name, VG_TARGET, identical ? "true" : "false", mv, mc, mv / mc, ratios[25], ratios[974],
           sv.n, sc.n);
    free(tv);
    free(tc);
    free(sv.t);
    free(sc.t);
}

#endif
