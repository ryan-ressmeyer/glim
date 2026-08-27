.PHONY: build check check-frontend check-rust frontend-build generic-skill-check rust-checks

build: frontend-build
	cargo build --locked

frontend-build:
	cd web && npm run build

check-frontend:
	cd web && npm run check

rust-checks:
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets --all-features -- -D warnings
	cargo test --locked --all-targets --all-features

check-rust: frontend-build rust-checks

generic-skill-check:
	node tests/generic-skill.mjs

check: check-frontend generic-skill-check rust-checks
