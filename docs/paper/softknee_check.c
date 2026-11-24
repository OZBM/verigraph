/*
 * softknee_check.c - machine cross-check of the soft-knee theorems in verigraph.tex.
 *
 * It evaluates the repaired kernel (examples/audio_limiter.vgs) and the first draft
 * (examples/soft_knee_draft.vgs) in IEEE 754 binary32 with round-to-nearest-even,
 * following docs/14: FMAX/FMIN are IEEE 754-2019 maximum/minimum (NaN-propagating,
 * -0 < +0), FCLAMP(x, lo, hi) = FMIN(FMAX(x, lo), hi), FMA has one rounding.
 *
 * Checks:
 *   E1  every finite binary32 x (4,278,190,080 values) for each knee in a fixed list;
 *   E2  every binary32 knee in [0.5, fl(0.99)] against a list of special x values;
 *   E3  2^30 seeded random (x, knee) pairs over the whole PRE domain;
 *   E4  the draft's trace at x = fl(3.0e38), knee = 0.5, and its violation count per knee.
 *
 * Build: clang -std=c11 -O2 -ffp-contract=off -fno-fast-math -pthread softknee_check.c -lm
 * The program reports counts only; it proves nothing beyond the inputs it enumerates.
 */
#include <math.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static float f_of(uint32_t u) { float f; memcpy(&f, &u, 4); return f; }
static uint32_t bits_of(float f) { uint32_t u; memcpy(&u, &f, 4); return u; }

/* IEEE 754-2019 maximum and minimum: NaN if either operand is NaN; -0 < +0. */
static inline float ieee_max(float a, float b) {
  if (isnan(a) || isnan(b)) return NAN;
  if (a > b) return a;
  if (b > a) return b;
  if (a == 0.0f) return (signbit(a) && signbit(b)) ? -0.0f : 0.0f;
  return a;
}
static inline float ieee_min(float a, float b) {
  if (isnan(a) || isnan(b)) return NAN;
  if (a < b) return a;
  if (b < a) return b;
  if (a == 0.0f) return (signbit(a) || signbit(b)) ? -0.0f : 0.0f;
  return a;
}
static inline float fclamp(float x, float lo, float hi) { return ieee_min(ieee_max(x, lo), hi); }

typedef struct { float p[13]; } trace_t;

/* Repaired kernel, port by port (%0 ... %12). */
static inline float repaired(float x, float knee, trace_t *t) {
  float p0 = fabsf(x);
  float p1 = p0 - knee;
  float p2 = ieee_max(p1, 0.0f);
  float p3 = 1.0f - knee;
  float p4 = p2 / p3;
  float p5 = 1.0f + p4;
  float p6 = 1.0f / p5;
  float p7 = 1.0f - p6;
  float p8 = fmaf(p3, p7, knee);
  float p9 = copysignf(p8, x);
  float p10 = (p0 <= knee) ? 1.0f : 0.0f;
  float p11 = (p0 <= knee) ? x : p9;
  float p12 = fclamp(p11, -1.0f, 1.0f);
  if (t) {
    float v[13] = {p0, p1, p2, p3, p4, p5, p6, p7, p8, p9, p10, p11, p12};
    memcpy(t->p, v, sizeof v);
  }
  return p12;
}

/* First draft: %6 = %4 / %5 and %7 = FMIN(%6, 1). */
static inline float draft(float x, float knee, trace_t *t) {
  float p0 = fabsf(x);
  float p1 = p0 - knee;
  float p2 = ieee_max(p1, 0.0f);
  float p3 = 1.0f - knee;
  float p4 = p2 / p3;
  float p5 = 1.0f + p4;
  float p6 = p4 / p5;
  float p7 = ieee_min(p6, 1.0f);
  float p8 = fmaf(p3, p7, knee);
  float p9 = copysignf(p8, x);
  float p10 = (p0 <= knee) ? 1.0f : 0.0f;
  float p11 = (p0 <= knee) ? x : p9;
  float p12 = fclamp(p11, -1.0f, 1.0f);
  if (t) {
    float v[13] = {p0, p1, p2, p3, p4, p5, p6, p7, p8, p9, p10, p11, p12};
    memcpy(t->p, v, sizeof v);
  }
  return p12;
}

/* POST of @soft_knee, with IEEE comparisons. */
static inline int post_ok(float x, float r) {
  return (r >= -1.0f) && (r <= 1.0f) && ((x >= 0.0f) == (r >= 0.0f));
}

/* The intermediate facts used in the proof of Theorem (soft knee). */
static inline int facts_ok(float x, float knee, const trace_t *t) {
  for (int i = 0; i < 13; i++) if (isnan(t->p[i])) return 0;
  if (!(t->p[2] >= 0.0f) || signbit(t->p[2])) return 0;          /* %2 = +0 or positive */
  if ((double)t->p[3] != 1.0 - (double)knee) return 0;          /* %3 exact */
  if (!(t->p[4] >= 0.0f)) return 0;                             /* %4 >= 0, maybe +inf */
  if (!(t->p[5] >= 1.0f)) return 0;                             /* %5 in [1, +inf] */
  if (!(t->p[6] >= 0.0f && t->p[6] <= 1.0f)) return 0;          /* %6 in [0, 1] */
  if (!(t->p[7] >= 0.0f && t->p[7] <= 1.0f)) return 0;          /* %7 in [0, 1] */
  if (!(t->p[8] >= knee && t->p[8] <= 1.0f)) return 0;          /* %8 in [knee, 1] */
  if (fabsf(t->p[9]) != t->p[8] || signbit(t->p[9]) != signbit(x)) return 0;
  (void)x;
  return 1;
}

typedef struct {
  float knee;
  uint64_t lo, hi;            /* bit-pattern range [lo, hi) */
  uint64_t finite, rep_bad, rep_facts_bad, draft_bad;
} job_t;

static void *run_range(void *arg) {
  job_t *j = (job_t *)arg;
  for (uint64_t u = j->lo; u < j->hi; u++) {
    float x = f_of((uint32_t)u);
    if (!isfinite(x)) continue;
    j->finite++;
    trace_t t;
    float r = repaired(x, j->knee, &t);
    if (!post_ok(x, r)) j->rep_bad++;
    if (!facts_ok(x, j->knee, &t)) j->rep_facts_bad++;
    if (!post_ok(x, draft(x, j->knee, NULL))) j->draft_bad++;
  }
  return NULL;
}

static uint64_t splitmix64(uint64_t *s) {
  uint64_t z = (*s += 0x9E3779B97F4A7C15ull);
  z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
  z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
  return z ^ (z >> 31);
}

typedef struct { uint64_t seed, n, bad, facts_bad; uint32_t klo, khi; } rjob_t;
static void *run_random(void *arg) {
  rjob_t *j = (rjob_t *)arg;
  uint64_t s = j->seed;
  for (uint64_t i = 0; i < j->n; i++) {
    uint64_t z = splitmix64(&s);
    float x = f_of((uint32_t)z);
    if (!isfinite(x)) { i--; continue; }
    float knee = f_of(j->klo + (uint32_t)((z >> 32) % (uint64_t)(j->khi - j->klo + 1)));
    trace_t t;
    float r = repaired(x, knee, &t);
    if (!post_ok(x, r)) j->bad++;
    if (!facts_ok(x, knee, &t)) j->facts_bad++;
  }
  return NULL;
}

int main(int argc, char **argv) {
  int nthreads = argc > 1 ? atoi(argv[1]) : 8;
  if (nthreads < 1 || nthreads > 64) nthreads = 8;
  const float K_LO = 0.5f, K_HI = 0.99f;          /* 0.99f = 0x3F7D70A4 */
  const uint32_t klo = bits_of(K_LO), khi = bits_of(K_HI);

  /* Environment checks: default rounding and no flush-to-zero. */
  volatile float tiny = f_of(0x00000001u), one = 1.0f;
  printf("env: fl(0.99) = 0x%08X = %.25g; subnormal*1 = %g (must be nonzero)\n",
         khi, (double)K_HI, (double)(tiny * one));

  /* E4: the draft's trace at x = fl(3e38), knee = 0.5. */
  float x0 = 3.0e38f;
  trace_t t;
  float rd = draft(x0, 0.5f, &t);
  printf("E4 x = fl(3e38) = 0x%08X = %.17g\n", bits_of(x0), (double)x0);
  for (int i = 0; i < 13; i++)
    printf("E4 draft %%%d = %.9g (0x%08X)\n", i, (double)t.p[i], bits_of(t.p[i]));
  printf("E4 draft POST holds: %d\n", post_ok(x0, rd));
  float rr = repaired(x0, 0.5f, &t);
  for (int i = 0; i < 13; i++)
    printf("E4 repaired %%%d = %.9g (0x%08X)\n", i, (double)t.p[i], bits_of(t.p[i]));
  printf("E4 repaired POST holds: %d\n", post_ok(x0, rr));

  /* E1: exhaustive over finite x for a fixed list of knees. */
  uint32_t knees[] = {klo, klo + 1, bits_of(0.75f), bits_of(0.9f), khi - 1, khi,
                      0, 0, 0, 0};
  uint64_t ks = 0x5eed20261106ull;
  for (int i = 6; i < 10; i++) knees[i] = klo + (uint32_t)(splitmix64(&ks) % (uint64_t)(khi - klo + 1));
  uint64_t tot_finite = 0, tot_rep_bad = 0, tot_facts_bad = 0;
  for (int k = 0; k < 10; k++) {
    float knee = f_of(knees[k]);
    job_t jobs[64];
    pthread_t th[64];
    uint64_t span = (1ull << 32) / (uint64_t)nthreads;
    for (int i = 0; i < nthreads; i++) {
      jobs[i] = (job_t){knee, span * (uint64_t)i,
                        i == nthreads - 1 ? (1ull << 32) : span * (uint64_t)(i + 1), 0, 0, 0, 0};
      pthread_create(&th[i], NULL, run_range, &jobs[i]);
    }
    uint64_t fin = 0, rb = 0, fb = 0, db = 0;
    for (int i = 0; i < nthreads; i++) {
      pthread_join(th[i], NULL);
      fin += jobs[i].finite; rb += jobs[i].rep_bad; fb += jobs[i].rep_facts_bad; db += jobs[i].draft_bad;
    }
    tot_finite += fin; tot_rep_bad += rb; tot_facts_bad += fb;
    printf("E1 knee = 0x%08X = %.10g: finite x = %llu, repaired POST violations = %llu, "
           "repaired fact violations = %llu, draft POST violations = %llu\n",
           knees[k], (double)knee, (unsigned long long)fin, (unsigned long long)rb,
           (unsigned long long)fb, (unsigned long long)db);
  }
  printf("E1 total: %llu evaluations, %llu POST violations, %llu fact violations\n",
         (unsigned long long)tot_finite, (unsigned long long)tot_rep_bad,
         (unsigned long long)tot_facts_bad);

  /* E2: every knee in [0.5, fl(0.99)] against special x values. */
  uint64_t e2_n = 0, e2_bad = 0, e2_facts_bad = 0, e2_sterbenz_bad = 0;
  for (uint32_t kb = klo; kb <= khi; kb++) {
    float knee = f_of(kb);
    if ((double)(1.0f - knee) != 1.0 - (double)knee) e2_sterbenz_bad++;
    float base[] = {0.0f, f_of(0x00000001u), f_of(0x007FFFFFu), f_of(0x00800000u), 0.5f, 1.0f,
                    2.0f, 1.0e30f, 3.0e38f, f_of(0x7F7FFFFFu), knee,
                    nextafterf(knee, 0.0f), nextafterf(knee, INFINITY),
                    nextafterf(nextafterf(knee, INFINITY), INFINITY), 2.0f * knee,
                    knee + 1.0f, f_of(0x7F000000u), f_of(0x7EFFFFFFu)};
    for (unsigned i = 0; i < sizeof base / sizeof base[0]; i++) {
      for (int sgn = 0; sgn < 2; sgn++) {
        float x = sgn ? -base[i] : base[i];
        trace_t tt;
        float r = repaired(x, knee, &tt);
        e2_n++;
        if (!post_ok(x, r)) e2_bad++;
        if (!facts_ok(x, knee, &tt)) e2_facts_bad++;
      }
    }
  }
  printf("E2 knees = %u, evaluations = %llu, POST violations = %llu, fact violations = %llu, "
         "inexact 1 - knee = %llu\n", khi - klo + 1, (unsigned long long)e2_n,
         (unsigned long long)e2_bad, (unsigned long long)e2_facts_bad,
         (unsigned long long)e2_sterbenz_bad);

  /* E3: random pairs. */
  {
    rjob_t rj[64];
    pthread_t th[64];
    uint64_t total = 1ull << 30, per = total / (uint64_t)nthreads;
    for (int i = 0; i < nthreads; i++) {
      rj[i] = (rjob_t){0xC0FFEE00ull + (uint64_t)i, i == nthreads - 1 ? total - per * (uint64_t)(nthreads - 1) : per,
                       0, 0, klo, khi};
      pthread_create(&th[i], NULL, run_random, &rj[i]);
    }
    uint64_t bad = 0, fbad = 0;
    for (int i = 0; i < nthreads; i++) { pthread_join(th[i], NULL); bad += rj[i].bad; fbad += rj[i].facts_bad; }
    printf("E3 random pairs = %llu, POST violations = %llu, fact violations = %llu\n",
           (unsigned long long)total, (unsigned long long)bad, (unsigned long long)fbad);
  }
  return 0;
}
