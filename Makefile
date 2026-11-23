# Each gate target exits 0 only when every criterion of its gate holds (ROADMAP.md, "Gates")
# and writes reports/gate-gN.md.
export RUSTFLAGS := -D warnings
export RUSTDOCFLAGS := -D warnings

GATES := gate-g2 gate-g3 gate-g4 gate-g5 gate-g6 gate-g7 gate-g8

.PHONY: check toolchain gate-g0 gate-g1 $(GATES)

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

$(GATES):
	./scripts/$@.sh
