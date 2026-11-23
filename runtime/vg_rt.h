#ifndef VG_RT_H
#define VG_RT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    VG_HANDLE_NONE = 0,
    VG_HANDLE_FILE_IN = 1,
    VG_HANDLE_FILE_OUT = 2,
    VG_HANDLE_CLOCK = 3
} vg_handle_kind;

typedef struct {
    int fd;
    uint8_t kind;
    uint8_t owned;
    uint8_t configured;
} vg_handle;

typedef struct {
    uint8_t *arena_base;
    uint64_t arena_cap;
    uint64_t arena_used;
    vg_handle *handles;
    uint32_t handle_count;
    uint64_t last_clock_ns;
    const char *target_triple;
    void **input_allocs;
    size_t input_alloc_count;
    size_t input_alloc_cap;
} vg_ctx;

typedef struct {
    uint8_t *ptr;
    uint64_t len;
} vg_slice_u8;

typedef struct {
    uint8_t is_ok;
    union {
        vg_slice_u8 ok;
        uint32_t err;
    } v;
} vg_res_slice_u8_u32;

typedef struct {
    uint8_t is_ok;
    union {
        uint64_t ok;
        uint32_t err;
    } v;
} vg_res_u64_u32;

/* Lifecycle and bump arena */
void vg_rt_init(vg_ctx *ctx, uint64_t arena_bytes, const char *target_triple);
void vg_rt_destroy(vg_ctx *ctx);
uint32_t vg_rt_resolve_handle(const vg_ctx *ctx, uint32_t param_ord, uint8_t kind);
int32_t vg_arena_has_space(const vg_ctx *ctx, uint64_t needed);
void *vg_arena_alloc(vg_ctx *ctx, uint64_t bytes);
void *vg_input_alloc(vg_ctx *ctx, uint64_t bytes);

/* Floating-point environment (default RN-even, no FTZ/DAZ) */
uint64_t vg_fp_enter(void);
void vg_fp_leave(uint64_t saved);

/* Effect shims */
void vg_rt_file_read(vg_ctx *ctx, uint32_t handle_id, uint64_t max_bytes, vg_res_slice_u8_u32 *out);
void vg_rt_file_write(vg_ctx *ctx, uint32_t handle_id, const vg_slice_u8 *data, vg_res_u64_u32 *out);
uint64_t vg_rt_clock_read(vg_ctx *ctx, uint32_t handle_id);

/* Telemetry and traps */
void vg_emit_guard(const vg_ctx *ctx, const char *guard_id, const char *obligation, const char *node_hash12);
_Noreturn void vg_trap_guard(const vg_ctx *ctx, const char *guard_id, const char *obligation, const char *node_hash12);

/* Binary stdin/stdout protocol helpers */
void vg_io_read_exact(void *buf, uint64_t n);
int32_t vg_io_read_opt_first(void *buf, uint64_t n);
void vg_io_write_exact(const void *buf, uint64_t n);
void vg_io_flush_stdout(void);

/* Exact IEEE-754 RNE storage-only f16 / bf16 conversion helpers */
float vg_f16_to_f32(uint16_t h);
double vg_f16_to_f64(uint16_t h);
uint16_t vg_f32_to_f16(float x);
uint16_t vg_f64_to_f16(double x);
uint16_t vg_u64_to_f16(uint64_t x);
uint16_t vg_i64_to_f16(int64_t x);

float vg_bf16_to_f32(uint16_t b);
double vg_bf16_to_f64(uint16_t b);
uint16_t vg_f32_to_bf16(float x);
uint16_t vg_f64_to_bf16(double x);
uint16_t vg_u64_to_bf16(uint64_t x);
uint16_t vg_i64_to_bf16(int64_t x);

#ifdef __cplusplus
}
#endif

#endif /* VG_RT_H */
