# Each gate target exits 0 only when every criterion of its gate holds (ROADMAP.md, "Gates")
# and writes reports/gate-gN.md. Gates whose phase has not landed fail loudly.
export RUSTFLAGS := -D warnings
export RUSTDOCFLAGS := -D warnings

GATES_TODO := gate-g2 gate-g3 gate-g4 gate-g5 gate-g6 gate-g7 gate-g8

.PHONY: check toolchain gate-g0 gate-g1 $(GATES_TODO)

check:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cargo test --workspace --locked

toolchain:
	./scripts/check-toolchain.sh

gate-g0:
	./scripts/gate-g0.sh

gate-g1:
	./scripts/gate-g1.sh

$(GATES_TODO):
	@echo "$@: NOT IMPLEMENTED. This gate's checks land with its phase (ROADMAP.md); it cannot pass yet." >&2
	@exit 1
