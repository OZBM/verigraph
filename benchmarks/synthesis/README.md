# Synthesis Benchmark (`P5.6`, `G5`)

30 plain-language synthesis tasks with reference solutions (`reference.vgs`),
grader reference contracts (`contract.vgs`), and `vg run` test vectors (`tests.json`).

| Task | Node | What it exercises | Obligations (`vg verify`) |
| --- | --- | --- | ---: |
| `01_sat_add_u8` | `@sat_add_u8` | Saturating u8 addition (ADD.sat) | 5 |
| `02_checked_sub_u32` | `@checked_sub_u32` | Checked u32 subtraction with precondition (SUB.checked) | 4 |
| `03_midpoint_u32` | `@midpoint_u32` | Overflow-free u32 midpoint (AND, XOR, SHR.checked, ADD.checked) | 7 |
| `04_wrap_add_u16` | `@wrap_add_u16` | Wrapping u16 modular addition (ADD.wrap) | 4 |
| `05_safe_div_i32` | `@safe_div_i32` | Total signed i32 division guarding 0 and MIN/-1 (SELECT, DIV.checked) | 10 |
| `06_clamp_i32` | `@clamp_i32` | Signed i32 clamping with ordered comparisons (LT.s, GT.s, SELECT) | 6 |
| `07_abs_sat_i32` | `@abs_sat_i32` | Saturating i32 absolute value (NEG.sat, LT.s, SELECT) | 5 |
| `08_widen_scale_u8_to_u16` | `@widen_scale_u8_to_u16` | Widening CAST.u16 with checked multiply-add | 5 |
| `09_sanitize_f32` | `@sanitize_f32` | Float sanitization for NaN and +/-inf with same() | 4 |
| `10_clamp_finite_f32` | `@clamp_finite_f32` | FCLAMP with NaN handling into finite bounds | 9 |
| `11_safe_fdiv_f32` | `@safe_fdiv_f32` | Finite-preserving float division handling 0, NaN, inf, and overflow | 3 |
| `12_lerp_f32` | `@lerp_f32` | Bounded FMA linear interpolation clamped into [min(a,b), max(a,b)] | 6 |
| `13_abs_diff_f64` | `@abs_diff_f64` | Double-precision finite absolute difference (FSUB, FABS, FMIN, FMAX) | 4 |
| `14_soft_clip_f32` | `@soft_clip_f32` | Soft-knee audio clipper (docs/03) avoiding inf/inf = NaN | 7 |
| `15_array_get_clamped` | `@array_get_clamped` | Clamped Array<u32, 8> indexing (INDEX, LT.u, SELECT) | 6 |
| `16_slice_get_or_default` | `@slice_get_or_default` | Bounds-checked Slice<u32> lookup via ASSERT_GUARD.branch | 11 |
| `17_array_swap_u32` | `@array_swap_u32` | Array element swap preserving all other indices (INDEX, UPDATE) | 14 |
| `18_slice_prefix_view` | `@slice_prefix_view` | Zero-allocation Slice prefix view (LEN, LT.u, SELECT, SLICE) | 6 |
| `19_array_concat_pair` | `@array_concat_pair` | Fixed-size Array CONCAT preserving elements | 7 |
| `20_fold_sum_bounded` | `@fold_sum_bounded` | FOLD_BOUNDED exact sum over Array<u32, 8> with INVARIANT and fold: | 12 |
| `21_fold_count_above` | `@fold_count_above` | FOLD_BOUNDED count over Slice<f32> with broadcast threshold | 11 |
| `22_fold_max_u32` | `@fold_max_u32` | FOLD_BOUNDED maximum element over Array<u32, 6> with INVARIANT | 11 |
| `23_fold_sat_sum_u8` | `@fold_sat_sum_u8` | FOLD_BOUNDED saturating byte sum over Slice<u8> | 13 |
| `24_map_inc_array` | `@map_inc_array` | SIMD_MAP over Array<u32, 8> with broadcast argument | 9 |
| `25_map_clamp_f32` | `@map_clamp_f32` | SIMD_MAP clamping finite Array<f32, 8> into [-1.0, 1.0] | 15 |
| `26_map_scale_slice` | `@map_scale_slice` | Arena-allocating SIMD_MAP doubling Slice<u16> | 10 |
| `27_checked_div_result` | `@checked_div_result` | ASSERT_GUARD.branch returning Result<u32, u32> (OK/ERR) | 23 |
| `28_unwrap_or_default` | `@unwrap_or_default` | IS_OK, ASSERT_GUARD.branch, and GET_OK on Result<u32, u32> | 11 |
| `29_compose_normalize_clip` | `@compose_normalize_clip` | CALL_HASH composition of gain scaling and soft clipping | 26 |
| `30_compose_affine_clamp` | `@compose_affine_clamp` | CALL_HASH composition of affine step and u32 range clamp | 18 |
