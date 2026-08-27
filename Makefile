.PHONY: build check check-frontend check-rust frontend-build generic-skill-check pi-package-install pi-package-typecheck pi-package-test pi-package-check release-contract-check rust-checks

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

release-contract-check:
	node tests/release-contract.mjs

pi-package-install:
	npm ci

pi-package-typecheck:
	npm run typecheck

pi-package-test:
	npm test

pi-package-check: pi-package-typecheck pi-package-test
	npm run check:package
	npm run test:pi-load

check: check-frontend generic-skill-check pi-package-check release-contract-check rust-checks
