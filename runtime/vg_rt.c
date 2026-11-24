#define _POSIX_C_SOURCE 200809L
#if defined(__linux__)
#define _DEFAULT_SOURCE /* syscall(2), for Landlock */
#endif

#include "vg_rt.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#if defined(__APPLE__)
/* Deprecated by Apple, and still the one way to sandbox a process after it starts (ADR 0011). */
int sandbox_init(const char *profile, uint64_t flags, char **errorbuf);
#elif defined(__linux__)
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#endif

#define VG_MAX_HANDLES 35u
#define VG_ROOT_HANDLES 34u
#define VG_DEF_CLOCK 34u

static _Alignas(16) uint8_t vg_empty_sentinel[16] = {0};

static uint64_t vg_align16(uint64_t n) {
    if (n > UINT64_MAX - 15ULL) {
        return UINT64_MAX;
    }
    return (n + 15ULL) & ~15ULL;
}

uint64_t vg_fp_enter(void) {
#if defined(__aarch64__)
    uint64_t old_fpcr = 0;
    __asm__ __volatile__("mrs %0, fpcr" : "=r"(old_fpcr));
    uint64_t zero = 0;
    __asm__ __volatile__("msr fpcr, %0" :: "r"(zero));
    return old_fpcr;
#elif defined(__x86_64__)
    uint32_t old_mxcsr = 0;
    __asm__ __volatile__("stmxcsr %0" : "=m"(old_mxcsr));
    uint32_t def_mxcsr = 0x1F80u;
    __asm__ __volatile__("ldmxcsr %0" :: "m"(def_mxcsr));
    return (uint64_t)old_mxcsr;
#else
    return 0ULL;
#endif
}

void vg_fp_leave(uint64_t saved) {
#if defined(__aarch64__)
    __asm__ __volatile__("msr fpcr, %0" :: "r"(saved));
#elif defined(__x86_64__)
    uint32_t v = (uint32_t)saved;
    __asm__ __volatile__("ldmxcsr %0" :: "m"(v));
#else
    (void)saved;
#endif
}

static int vg_parse_u64(const char *s, uint64_t *out) {
    if (!s || !*s) {
        return 0;
    }
    char *end = NULL;
    errno = 0;
    unsigned long long val = strtoull(s, &end, 10);
    if (errno != 0 || end == s || *end != '\0') {
        return 0;
    }
    *out = (uint64_t)val;
    return 1;
}

/* The command line, for the manifest's argv bindings (docs/04); main hands it over first. */
static int vg_argc = 0;
static char **vg_argv = NULL;
static int vg_next_arg = 1;

void vg_rt_set_args(int argc, char **argv) {
    vg_argc = argc;
    vg_argv = argv;
    vg_next_arg = 1;
}

static void vg_init_handles(vg_ctx *ctx) {
    ctx->handles = (vg_handle *)calloc(VG_MAX_HANDLES, sizeof(vg_handle));
    if (!ctx->handles) {
        exit(2);
    }
    ctx->handle_count = VG_MAX_HANDLES;
    for (uint32_t i = 0; i < VG_MAX_HANDLES; ++i) {
        ctx->handles[i].fd = -1;
        ctx->handles[i].kind = VG_HANDLE_NONE;
        ctx->handles[i].owned = 0;
        ctx->handles[i].configured = 0;
    }
    ctx->handles[VG_DEF_CLOCK].kind = VG_HANDLE_CLOCK;
    ctx->handles[VG_DEF_CLOCK].configured = 1;
}

/* docs/04: the root's FileIn and FileOut handles bind, in parameter order, to the next command-line
 * argument, "-" meaning stdin for a FileIn and stdout for a FileOut; a Clock needs none. A FileOut
 * appends, so an existing file loses nothing. A handle that cannot be opened ends the process with
 * exit code 5 before the root runs. */
uint32_t vg_rt_resolve_handle(vg_ctx *ctx, uint32_t param_ord, uint8_t kind) {
    if (kind == VG_HANDLE_CLOCK) {
        return VG_DEF_CLOCK;
    }
    if (!ctx || !ctx->handles || param_ord >= VG_ROOT_HANDLES ||
        (kind != VG_HANDLE_FILE_IN && kind != VG_HANDLE_FILE_OUT)) {
        fprintf(stderr, "vg-rt: handle %u has no binding\n", param_ord);
        exit(5);
    }
    if (!vg_argv || vg_next_arg >= vg_argc) {
        fprintf(stderr, "vg-rt: handle %u needs a path argument (or - for %s)\n", param_ord,
                kind == VG_HANDLE_FILE_IN ? "stdin" : "stdout");
        exit(5);
    }
    const char *path = vg_argv[vg_next_arg++];
    int fd;
    uint8_t owned = 0;
    if (strcmp(path, "-") == 0) {
        fd = kind == VG_HANDLE_FILE_IN ? STDIN_FILENO : STDOUT_FILENO;
    } else {
        fd = kind == VG_HANDLE_FILE_IN ? open(path, O_RDONLY)
                                       : open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
        owned = 1;
    }
    if (fd < 0) {
        fprintf(stderr, "vg-rt: cannot open %s: %s\n", path, strerror(errno));
        exit(5);
    }
    ctx->handles[param_ord].fd = fd;
    ctx->handles[param_ord].kind = kind;
    ctx->handles[param_ord].owned = owned;
    ctx->handles[param_ord].configured = 1;
    return param_ord;
}

#if defined(__linux__)
/* Landlock's interface, defined here so older kernel headers still compile (ABI 1 to 4). */
#define VG_LANDLOCK_CREATE_RULESET_VERSION 1u
#define VG_LANDLOCK_FS_ABI1 ((1ULL << 13) - 1) /* EXECUTE ... MAKE_SYM */
#define VG_LANDLOCK_FS_REFER (1ULL << 13)
#define VG_LANDLOCK_FS_TRUNCATE (1ULL << 14)
#define VG_LANDLOCK_NET_TCP 3ULL /* BIND_TCP | CONNECT_TCP */
#ifndef SYS_landlock_create_ruleset
#define SYS_landlock_create_ruleset 444
#define SYS_landlock_restrict_self 446
#endif
#if defined(__x86_64__)
#define VG_AUDIT_ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define VG_AUDIT_ARCH AUDIT_ARCH_AARCH64
#endif

static void vg_sandbox_fail(const char *what) {
    fprintf(stderr, "vg-rt: %s; a VeriGraph program does not run unsandboxed\n", what);
    exit(5);
}
#endif

/* docs/04: once its handles are open, the process drops every other authority, so it keeps exactly
 * the descriptors its manifest names. macOS: a deny-by-default profile. Linux: a Landlock ruleset
 * that handles every filesystem right and grants none (and from ABI 4 every TCP right), plus a
 * seccomp filter that makes socket fail with EPERM. WASM: WASI already holds only its preopens. */
void vg_rt_sandbox(void) {
#if defined(__APPLE__)
    char *err = NULL;
    if (sandbox_init("(version 1)(deny default)", 0, &err) != 0) {
        fprintf(stderr, "vg-rt: sandbox_init failed: %s\n", err ? err : "unknown error");
        exit(5);
    }
#elif defined(__linux__)
    long abi = syscall(SYS_landlock_create_ruleset, NULL, 0, VG_LANDLOCK_CREATE_RULESET_VERSION);
    if (abi < 1) {
        vg_sandbox_fail("this kernel has no Landlock");
    }
    struct {
        uint64_t handled_access_fs;
        uint64_t handled_access_net;
    } attr = {VG_LANDLOCK_FS_ABI1, 0};
    if (abi >= 2) {
        attr.handled_access_fs |= VG_LANDLOCK_FS_REFER;
    }
    if (abi >= 3) {
        attr.handled_access_fs |= VG_LANDLOCK_FS_TRUNCATE;
    }
    size_t attr_size = sizeof(uint64_t);
    if (abi >= 4) {
        attr.handled_access_net = VG_LANDLOCK_NET_TCP;
        attr_size = sizeof(attr);
    }
    long rfd = syscall(SYS_landlock_create_ruleset, &attr, attr_size, 0);
    if (rfd < 0 || prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 ||
        syscall(SYS_landlock_restrict_self, rfd, 0) != 0) {
        vg_sandbox_fail("Landlock could not be applied");
    }
    close((int)rfd);
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, VG_AUDIT_ARCH, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        /* x32 numbers on x86_64 would otherwise reach socket past the next check. */
        BPF_JUMP(BPF_JMP | BPF_JGE | BPF_K, 0x40000000u, 1, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_socket, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (EPERM & SECCOMP_RET_DATA)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = {(unsigned short)(sizeof(filter) / sizeof(filter[0])), filter};
    if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog) != 0) {
        vg_sandbox_fail("the seccomp filter could not be applied");
    }
#endif
}

void vg_rt_init(vg_ctx *ctx, uint64_t arena_bytes, const char *target_triple) {
    memset(ctx, 0, sizeof(*ctx));
    (void)vg_fp_enter();
    ctx->target_triple = target_triple ? target_triple : "unknown";

    uint64_t cap = arena_bytes;
    const char *env_cap = getenv("VG_ARENA_CAP");
    uint64_t parsed_cap = 0;
    if (env_cap && vg_parse_u64(env_cap, &parsed_cap)) {
        cap = parsed_cap;
    }

    if (cap > 0) {
        uint64_t alloc_size = vg_align16(cap);
        if (alloc_size == UINT64_MAX || alloc_size > (uint64_t)SIZE_MAX) {
            exit(3);
        }
#if defined(_POSIX_VERSION) && _POSIX_VERSION >= 200112L
        void *mem = NULL;
        if (posix_memalign(&mem, 16, (size_t)alloc_size) != 0 || !mem) {
            exit(3);
        }
        ctx->arena_base = (uint8_t *)mem;
#else
        ctx->arena_base = (uint8_t *)malloc((size_t)alloc_size);
        if (!ctx->arena_base) {
            exit(3);
        }
#endif
        memset(ctx->arena_base, 0, (size_t)alloc_size);
        ctx->arena_cap = cap;
    } else {
        ctx->arena_base = NULL;
        ctx->arena_cap = 0;
    }
    ctx->arena_used = 0;
    ctx->last_clock_ns = 0;
    vg_init_handles(ctx);
}

void vg_rt_destroy(vg_ctx *ctx) {
    if (!ctx) {
        return;
    }
    if (ctx->handles) {
        for (uint32_t i = 0; i < ctx->handle_count; ++i) {
            if (ctx->handles[i].owned && ctx->handles[i].fd >= 0) {
                close(ctx->handles[i].fd);
            }
        }
        free(ctx->handles);
        ctx->handles = NULL;
    }
    if (ctx->arena_base) {
        free(ctx->arena_base);
        ctx->arena_base = NULL;
    }
    if (ctx->input_allocs) {
        for (size_t i = 0; i < ctx->input_alloc_count; ++i) {
            free(ctx->input_allocs[i]);
        }
        free(ctx->input_allocs);
        ctx->input_allocs = NULL;
    }
}

int32_t vg_arena_has_space(const vg_ctx *ctx, uint64_t needed) {
    if (needed == 0) {
        return 1;
    }
    if (!ctx || ctx->arena_used > ctx->arena_cap) {
        return 0;
    }
    return needed <= (ctx->arena_cap - ctx->arena_used) ? 1 : 0;
}

void *vg_arena_alloc(vg_ctx *ctx, uint64_t bytes) {
    if (bytes == 0) {
        return vg_empty_sentinel;
    }
    uint64_t aligned = vg_align16(bytes);
    if (aligned == UINT64_MAX || !ctx || ctx->arena_used > ctx->arena_cap ||
        aligned > (ctx->arena_cap - ctx->arena_used)) {
        exit(3);
    }
    void *ptr = ctx->arena_base + ctx->arena_used;
    ctx->arena_used += aligned;
    return ptr;
}

void *vg_input_alloc(vg_ctx *ctx, uint64_t bytes) {
    if (bytes == 0) {
        return vg_empty_sentinel;
    }
    if (bytes > (uint64_t)SIZE_MAX) {
        exit(2);
    }
    void *mem = malloc((size_t)bytes);
    if (!mem) {
        exit(2);
    }
    if (ctx) {
        if (ctx->input_alloc_count == ctx->input_alloc_cap) {
            size_t new_cap = ctx->input_alloc_cap ? ctx->input_alloc_cap * 2 : 8;
            void **grown = (void **)realloc(ctx->input_allocs, new_cap * sizeof(void *));
            if (!grown) {
                free(mem);
                exit(2);
            }
            ctx->input_allocs = grown;
            ctx->input_alloc_cap = new_cap;
        }
        ctx->input_allocs[ctx->input_alloc_count++] = mem;
    }
    return mem;
}

void vg_rt_file_read(vg_ctx *ctx, uint32_t handle_id, uint64_t max_bytes, vg_res_slice_u8_u32 *out) {
    if (!out) {
        return;
    }
    if (!ctx || handle_id >= ctx->handle_count ||
        ctx->handles[handle_id].kind != VG_HANDLE_FILE_IN ||
        ctx->handles[handle_id].fd < 0) {
        out->is_ok = 0;
        out->v.err = 1u;
        return;
    }
    int fd = ctx->handles[handle_id].fd;
    if (max_bytes > (uint64_t)SIZE_MAX - 1ULL) {
        out->is_ok = 0;
        out->v.err = 1u;
        return;
    }
    size_t cap = (size_t)max_bytes + 1;
    uint8_t *tmp = (uint8_t *)malloc(cap);
    if (!tmp) {
        out->is_ok = 0;
        out->v.err = 1u;
        return;
    }

    size_t total = 0;
    while (total < cap) {
        ssize_t n = read(fd, tmp + total, cap - total);
        if (n > 0) {
            total += (size_t)n;
        } else if (n == 0) {
            break;
        } else if (errno == EINTR) {
            continue;
        } else {
            free(tmp);
            out->is_ok = 0;
            out->v.err = 1u;
            return;
        }
    }

    if (total > (size_t)max_bytes) {
        free(tmp);
        out->is_ok = 0;
        out->v.err = 2u;
        return;
    }

    uint8_t *dst = (uint8_t *)vg_arena_alloc(ctx, (uint64_t)total);
    if (total > 0) {
        memcpy(dst, tmp, total);
    }
    free(tmp);
    out->is_ok = 1;
    out->v.ok.ptr = dst;
    out->v.ok.len = (uint64_t)total;
}

void vg_rt_file_write(vg_ctx *ctx, uint32_t handle_id, const vg_slice_u8 *data, vg_res_u64_u32 *out) {
    if (!out) {
        return;
    }
    if (!ctx || !data || handle_id >= ctx->handle_count ||
        ctx->handles[handle_id].kind != VG_HANDLE_FILE_OUT ||
        ctx->handles[handle_id].fd < 0) {
        out->is_ok = 0;
        out->v.err = 1u;
        return;
    }
    int fd = ctx->handles[handle_id].fd;
    uint64_t written = 0;
    while (written < data->len) {
        uint64_t rem = data->len - written;
        size_t chunk = rem > (uint64_t)SSIZE_MAX ? (size_t)SSIZE_MAX : (size_t)rem;
        ssize_t n = write(fd, data->ptr + written, chunk);
        if (n > 0) {
            written += (uint64_t)n;
        } else if (n < 0 && errno == EINTR) {
            continue;
        } else {
            out->is_ok = 0;
            out->v.err = 1u;
            return;
        }
    }
    out->is_ok = 1;
    out->v.ok = data->len;
}

uint64_t vg_rt_clock_read(vg_ctx *ctx, uint32_t handle_id) {
    if (!ctx || handle_id >= ctx->handle_count ||
        ctx->handles[handle_id].kind != VG_HANDLE_CLOCK) {
        return ctx ? ctx->last_clock_ns : 0ULL;
    }
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return ctx->last_clock_ns;
    }
    uint64_t ns = (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
    if (ns < ctx->last_clock_ns) {
        ns = ctx->last_clock_ns;
    }
    ctx->last_clock_ns = ns;
    return ns;
}

static void vg_fputs_json_escaped(FILE *fp, const char *s) {
    if (!s) {
        return;
    }
    for (const unsigned char *p = (const unsigned char *)s; *p; ++p) {
        unsigned char c = *p;
        if (c == '"' || c == '\\') {
            fputc('\\', fp);
            fputc((int)c, fp);
        } else if (c == '\n') {
            fputs("\\n", fp);
        } else if (c == '\r') {
            fputs("\\r", fp);
        } else if (c == '\t') {
            fputs("\\t", fp);
        } else if (c < 0x20u) {
            fprintf(fp, "\\u%04x", (unsigned)c);
        } else {
            fputc((int)c, fp);
        }
    }
}

void vg_emit_guard(const vg_ctx *ctx, const char *guard_id, const char *obligation, const char *node_hash12) {
    const char *triple = (ctx && ctx->target_triple) ? ctx->target_triple : "unknown";
    fputs("{\"event\":\"guard\",\"guard\":\"", stderr);
    vg_fputs_json_escaped(stderr, guard_id ? guard_id : "");
    fputs("\",\"obligation\":\"", stderr);
    vg_fputs_json_escaped(stderr, obligation ? obligation : "");
    fputs("\",\"node\":\"", stderr);
    vg_fputs_json_escaped(stderr, node_hash12 ? node_hash12 : "");
    fputs("\",\"target\":\"", stderr);
    vg_fputs_json_escaped(stderr, triple);
    fputs("\"}\n", stderr);
    fflush(stderr);
}

_Noreturn void vg_trap_guard(const vg_ctx *ctx, const char *guard_id, const char *obligation, const char *node_hash12) {
    vg_emit_guard(ctx, guard_id, obligation, node_hash12);
    exit(4);
}

void vg_io_read_exact(void *buf, uint64_t n) {
    uint8_t *p = (uint8_t *)buf;
    uint64_t done = 0;
    while (done < n) {
        uint64_t rem = n - done;
        size_t chunk = rem > (uint64_t)SIZE_MAX ? SIZE_MAX : (size_t)rem;
        size_t got = fread(p + done, 1, chunk, stdin);
        if (got == 0) {
            exit(2);
        }
        done += (uint64_t)got;
    }
}

int32_t vg_io_read_opt_first(void *buf, uint64_t n) {
    if (n == 0) {
        return 1;
    }
    int ch = fgetc(stdin);
    if (ch == EOF) {
        return 0;
    }
    uint8_t *p = (uint8_t *)buf;
    p[0] = (uint8_t)ch;
    if (n > 1) {
        vg_io_read_exact(p + 1, n - 1);
    }
    return 1;
}

void vg_io_write_exact(const void *buf, uint64_t n) {
    const uint8_t *p = (const uint8_t *)buf;
    uint64_t done = 0;
    while (done < n) {
        uint64_t rem = n - done;
        size_t chunk = rem > (uint64_t)SIZE_MAX ? SIZE_MAX : (size_t)rem;
        size_t put = fwrite(p + done, 1, chunk, stdout);
        if (put == 0) {
            exit(4);
        }
        done += (uint64_t)put;
    }
}

void vg_io_flush_stdout(void) {
    if (fflush(stdout) != 0) {
        exit(4);
    }
}

static inline uint32_t vg_f32_bits(float x) {
    uint32_t b = 0;
    memcpy(&b, &x, sizeof(b));
    return b;
}

static inline float vg_bits_f32(uint32_t b) {
    float x = 0.0f;
    memcpy(&x, &b, sizeof(x));
    return x;
}

static inline uint64_t vg_f64_bits(double x) {
    uint64_t b = 0;
    memcpy(&b, &x, sizeof(b));
    return b;
}

static inline double vg_bits_f64(uint64_t b) {
    double x = 0.0;
    memcpy(&x, &b, sizeof(x));
    return x;
}

float vg_f16_to_f32(uint16_t h) {
    uint32_t sign = (uint32_t)(h & 0x8000u) << 16;
    uint32_t exp = (uint32_t)((h >> 10) & 0x1Fu);
    uint32_t frac = (uint32_t)(h & 0x03FFu);
    if (exp == 0x1Fu) {
        if (frac == 0) {
            return vg_bits_f32(sign | 0x7F800000u);
        }
        return vg_bits_f32(0x7FC00000u);
    }
    if (exp == 0) {
        if (frac == 0) {
            return vg_bits_f32(sign);
        }
        int32_t e = -14;
        while ((frac & 0x0400u) == 0) {
            frac <<= 1;
            e -= 1;
        }
        frac &= 0x03FFu;
        uint32_t f32_exp = (uint32_t)(e + 127);
        return vg_bits_f32(sign | (f32_exp << 23) | (frac << 13));
    }
    uint32_t f32_exp = exp + (127u - 15u);
    return vg_bits_f32(sign | (f32_exp << 23) | (frac << 13));
}

double vg_f16_to_f64(uint16_t h) {
    uint32_t exp = (uint32_t)((h >> 10) & 0x1Fu);
    uint32_t frac = (uint32_t)(h & 0x03FFu);
    if (exp == 0x1Fu && frac != 0) {
        return vg_bits_f64(0x7FF8000000000000ULL);
    }
    return (double)vg_f16_to_f32(h);
}

uint16_t vg_f32_to_f16(float x) {
    uint32_t bits = vg_f32_bits(x);
    uint16_t sign16 = (uint16_t)((bits >> 16) & 0x8000u);
    uint32_t exp = (bits >> 23) & 0xFFu;
    uint32_t mant = bits & 0x007FFFFFu;
    if (exp == 0xFFu) {
        if (mant != 0) {
            return 0x7E00u;
        }
        return (uint16_t)(sign16 | 0x7C00u);
    }
    if (exp == 0) {
        return sign16;
    }
    int32_t e = (int32_t)exp - 127;
    if (e > 15) {
        return (uint16_t)(sign16 | 0x7C00u);
    }
    uint32_t sig = 0x00800000u | mant;
    int32_t exp16 = e + 15;
    if (exp16 >= 1) {
        uint32_t q = sig >> 13;
        uint32_t rem = sig & 0x1FFFu;
        if (rem > 0x1000u || (rem == 0x1000u && (q & 1u))) {
            q += 1u;
        }
        if (q == 0x0800u) {
            q = 0x0400u;
            exp16 += 1;
        }
        if (exp16 >= 31) {
            return (uint16_t)(sign16 | 0x7C00u);
        }
        return (uint16_t)(sign16 | ((uint16_t)exp16 << 10) | (uint16_t)(q & 0x03FFu));
    }
    int32_t shift = -1 - e;
    if (shift >= 25) {
        return sign16;
    }
    uint32_t q = sig >> shift;
    uint32_t rem = sig & ((1u << shift) - 1u);
    uint32_t half = 1u << (shift - 1);
    if (rem > half || (rem == half && (q & 1u))) {
        q += 1u;
    }
    return (uint16_t)(sign16 | (uint16_t)q);
}

uint16_t vg_f64_to_f16(double x) {
    uint64_t bits = vg_f64_bits(x);
    uint16_t sign16 = (uint16_t)((bits >> 48) & 0x8000u);
    uint32_t exp = (uint32_t)((bits >> 52) & 0x7FFu);
    uint64_t mant = bits & 0x000FFFFFFFFFFFFFULL;
    if (exp == 0x7FFu) {
        if (mant != 0) {
            return 0x7E00u;
        }
        return (uint16_t)(sign16 | 0x7C00u);
    }
    if (exp == 0) {
        return sign16;
    }
    int32_t e = (int32_t)exp - 1023;
    if (e > 15) {
        return (uint16_t)(sign16 | 0x7C00u);
    }
    uint64_t sig = 0x0010000000000000ULL | mant;
    int32_t exp16 = e + 15;
    if (exp16 >= 1) {
        uint64_t q = sig >> 42;
        uint64_t rem = sig & ((1ULL << 42) - 1ULL);
        uint64_t half = 1ULL << 41;
        if (rem > half || (rem == half && (q & 1ULL))) {
            q += 1ULL;
        }
        if (q == 0x0800ULL) {
            q = 0x0400ULL;
            exp16 += 1;
        }
        if (exp16 >= 31) {
            return (uint16_t)(sign16 | 0x7C00u);
        }
        return (uint16_t)(sign16 | ((uint16_t)exp16 << 10) | (uint16_t)(q & 0x03FFu));
    }
    int32_t shift = 28 - e;
    if (shift >= 54) {
        return sign16;
    }
    uint64_t q = sig >> shift;
    uint64_t rem = sig & ((1ULL << shift) - 1ULL);
    uint64_t half = 1ULL << (shift - 1);
    if (rem > half || (rem == half && (q & 1ULL))) {
        q += 1ULL;
    }
    return (uint16_t)(sign16 | (uint16_t)q);
}

uint16_t vg_u64_to_f16(uint64_t x) {
    if (x >= 65520ULL) {
        return 0x7C00u;
    }
    return vg_f64_to_f16((double)x);
}

uint16_t vg_i64_to_f16(int64_t x) {
    if (x >= 65520LL) {
        return 0x7C00u;
    }
    if (x <= -65520LL) {
        return 0xFC00u;
    }
    return vg_f64_to_f16((double)x);
}

float vg_bf16_to_f32(uint16_t b) {
    uint32_t exp = (uint32_t)((b >> 7) & 0xFFu);
    uint32_t frac = (uint32_t)(b & 0x7Fu);
    if (exp == 0xFFu && frac != 0) {
        return vg_bits_f32(0x7FC00000u);
    }
    return vg_bits_f32(((uint32_t)b) << 16);
}

double vg_bf16_to_f64(uint16_t b) {
    uint32_t exp = (uint32_t)((b >> 7) & 0xFFu);
    uint32_t frac = (uint32_t)(b & 0x7Fu);
    if (exp == 0xFFu && frac != 0) {
        return vg_bits_f64(0x7FF8000000000000ULL);
    }
    return (double)vg_bf16_to_f32(b);
}

uint16_t vg_f32_to_bf16(float x) {
    uint32_t bits = vg_f32_bits(x);
    if ((bits & 0x7FFFFFFFu) > 0x7F800000u) {
        return 0x7FC0u;
    }
    if ((bits & 0x7FFFFFFFu) == 0x7F800000u) {
        return (uint16_t)(bits >> 16);
    }
    uint32_t rounding_bias = 0x7FFFu + ((bits >> 16) & 1u);
    uint32_t rounded = bits + rounding_bias;
    return (uint16_t)(rounded >> 16);
}

uint16_t vg_f64_to_bf16(double x) {
    uint64_t bits = vg_f64_bits(x);
    uint16_t sign16 = (uint16_t)((bits >> 48) & 0x8000u);
    uint32_t exp = (uint32_t)((bits >> 52) & 0x7FFu);
    uint64_t mant = bits & 0x000FFFFFFFFFFFFFULL;
    if (exp == 0x7FFu) {
        if (mant != 0) {
            return 0x7FC0u;
        }
        return (uint16_t)(sign16 | 0x7F80u);
    }
    if (exp == 0) {
        return sign16;
    }
    int32_t e = (int32_t)exp - 1023;
    if (e > 127) {
        return (uint16_t)(sign16 | 0x7F80u);
    }
    uint64_t sig = 0x0010000000000000ULL | mant;
    int32_t exp8 = e + 127;
    if (exp8 >= 1) {
        uint64_t q = sig >> 45;
        uint64_t rem = sig & ((1ULL << 45) - 1ULL);
        uint64_t half = 1ULL << 44;
        if (rem > half || (rem == half && (q & 1ULL))) {
            q += 1ULL;
        }
        if (q == 0x0100ULL) {
            q = 0x0080ULL;
            exp8 += 1;
        }
        if (exp8 >= 255) {
            return (uint16_t)(sign16 | 0x7F80u);
        }
        return (uint16_t)(sign16 | ((uint16_t)exp8 << 7) | (uint16_t)(q & 0x7Fu));
    }
    int32_t shift = -81 - e;
    if (shift >= 54) {
        return sign16;
    }
    uint64_t q = sig >> shift;
    uint64_t rem = sig & ((1ULL << shift) - 1ULL);
    uint64_t half = 1ULL << (shift - 1);
    if (rem > half || (rem == half && (q & 1ULL))) {
        q += 1ULL;
    }
    return (uint16_t)(sign16 | (uint16_t)q);
}

uint16_t vg_u64_to_bf16(uint64_t x) {
    if (x == 0) {
        return 0;
    }
    int msb = 63 - __builtin_clzll(x);
    uint32_t exp = (uint32_t)(127 + msb);
    if (msb <= 7) {
        uint16_t frac = (uint16_t)((x << (7 - msb)) & 0x7Fu);
        return (uint16_t)((exp << 7) | frac);
    }
    int shift = msb - 7;
    uint64_t q = x >> shift;
    uint64_t rem = x & ((1ULL << shift) - 1ULL);
    uint64_t half = 1ULL << (shift - 1);
    if (rem > half || (rem == half && (q & 1ULL))) {
        q += 1ULL;
    }
    if (q == 0x100ULL) {
        q = 0x80ULL;
        exp += 1u;
    }
    return (uint16_t)((exp << 7) | (uint16_t)(q & 0x7Fu));
}

uint16_t vg_i64_to_bf16(int64_t x) {
    if (x == 0) {
        return 0;
    }
    uint16_t sign16 = (x < 0) ? 0x8000u : 0u;
    uint64_t mag = (x < 0) ? (0ULL - (uint64_t)x) : (uint64_t)x;
    return (uint16_t)(sign16 | vg_u64_to_bf16(mag));
}
