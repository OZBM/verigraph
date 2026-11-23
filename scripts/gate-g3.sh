#!/bin/sh
# make gate-g3: the G3 criteria of docs/16 (verification); writes reports/gate-g3.md. Needs z3,
# cvc5 and Bitwuzla: release mode proves each obligation with two of them.
GATE=g3 TITLE="Verification"
. "$(dirname "$0")/gate-common.sh"
gate_start

run corpus env VG_REQUIRE_SOLVERS=1 cargo test -p vg-cli --locked --test verifier_corpus
c=met; test_ran corpus known_good_are_proven_by_two_solvers_in_release_mode || c="not met"
row "Every known-good file is proven, with two solvers in release mode" "$c" \
  "$(ls tests/corpus/good/*.vgs | wc -l | tr -d ' ') files; verifier_corpus known_good_are_proven_by_two_solvers_in_release_mode"

c=met; test_ran corpus known_good_verify_and_known_bad_are_refuted || c="not met"
# Every obligation kind with a concrete counterexample has a known-bad file (docs/16). Left out, as
# no input of vg-interp can show them: CONCAT.length (slices of 2^64 elements), COST (not measured by
# vg-interp), PRE.wd (no input meets an undefined PRE), NONVACUOUS (a vacuity, not a refutation).
missing=""
for k in ADD.no_overflow NEG.no_overflow DIV.nonzero SHL.amount CAST.in_range GET_OK.is_ok GET_ERR.is_err \
  FCLAMP.bounds FCOPYSIGN.sign INDEX.in_bounds CALL.pre MAP.pre FOLD.base FOLD.pre FOLD.step GEN.pre \
  GUARD.then.pre GUARD.else.pre "POST.0" ".wd."; do
  awk 'FNR == 1' tests/corpus/bad/*.vgs | grep -q -- "$k" || missing="$missing $k"
done
[ -z "$missing" ] || c="not met"
row "Every known-bad file is rejected at its obligation with a concrete counterexample, covering every obligation kind" "$c" \
  "$(ls tests/corpus/bad/*.vgs | wc -l | tr -d ' ') files; kinds without a file:${missing:- none}"

c=met
run mutation cargo test -p vg-cli --locked --test mutation || c="not met"
test_ran mutation no_contract_breaking_mutant_verifies || c="not met"
row "No contract-breaking mutant is verified" "$c" "mutation no_contract_breaking_mutant_verifies"

c=met; test_ran corpus the_soft_knee_story_holds || c="not met"
row "examples/soft_knee_draft.vgs is rejected at @soft_knee/POST.0" "$c" "verifier_corpus the_soft_knee_story_holds"

c=met
run audio env VG_REQUIRE_SOLVERS=1 cargo test -p vg-cli --locked --test audio_limiter || c="not met"
test_ran audio audio_limiter_verifies_in_release_mode || c="not met"
row "examples/audio_limiter.vgs is verified" "$c" "audio_limiter: SHA-256 published to a fresh Mesh, the query resolved, every obligation proven in release mode"

c=met
if grep -lE 'VG-V000[89]' "$work"/*.log >/dev/null 2>&1; then c="not met"; fi
row "No VG-V0008 or VG-V0009 anywhere" "$c" "no such code in the output of the runs above"

gate_finish
