#!/bin/sh
# make gate-g8: the G8 criteria of docs/16 (OmniLift lite); writes reports/gate-g8.md. Needs
# alive-tv and opt: VG_REQUIRE_ALIVE2=1 makes their absence a failure instead of a skip.
GATE=g8 TITLE="OmniLift lite"
. "$(dirname "$0")/gate-common.sh"
gate_start
c=met
run g8 env VG_REQUIRE_ALIVE2=1 cargo test -p vg-omnilift --locked --test g8 -- --nocapture || c="not met"
test_ran g8 g8_lifts_four_targets_and_unifies_sha256_spec_class || c="not met"
row "crc32, adler32 and SHA-256 compression lifted from C and Rust; Alive2 passes at unroll 8; 1e6 differential fuzz cases per function; both SHA-256 lifts in one spec class, evidence bounded(8)" "$c" \
  "g8_lifts_four_targets_and_unifies_sha256_spec_class (zlib crc32_z and adler32_z, mbedtls_sha256_compress, rust-sha2 compress256)"
c=met
test_ran g8 g8_rejects_mutated_lift_with_vg_l0002_and_vg_l0003 || c="not met"
row "A wrong lift is caught: Alive2 (VG-L0002) and fuzzing (VG-L0003) both reject a mutated SHA-256" "$c" "g8_rejects_mutated_lift_with_vg_l0002_and_vg_l0003"
gate_finish
