.PHONY: benchmark-compile benchmark-smoke build check check-frontend check-rust chromium-security-check frontend-build generic-skill-check hardening-harness-check pi-package-install pi-package-typecheck pi-package-test pi-package-check property-check release-acceptance release-acceptance-check release-contract-check resource-acceptance resource-acceptance-check rust-checks

build: frontend-build
	cargo build --locked

frontend-build:
	cd web && npm run build

check-frontend:
	cd web && npm run check

property-check:
	cargo test --locked --test adversarial_properties

chromium-security-check: build
	cd web && node tests/chromium-html-opt-in.mjs && node tests/chromium-live-feed.mjs

benchmark-compile:
	cargo bench --locked --bench hardening --no-run

benchmark-smoke:
	cargo bench --locked --bench hardening -- --sample-size 10 --measurement-time 0.1 --warm-up-time 0.1

hardening-harness-check:
	node --test tests/hardening-harness-contract.mjs
	node --check tests/resource-acceptance.mjs

resource-acceptance-check: hardening-harness-check

resource-acceptance: build resource-acceptance-check
	node tests/resource-acceptance.mjs

rust-checks:
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets --all-features -- -D warnings
	cargo test --locked --all-features

check-rust: frontend-build rust-checks

generic-skill-check:
	node tests/generic-skill.mjs

release-contract-check:
	node tests/release-contract.mjs

release-acceptance-check:
	node --test tests/release-acceptance-contract.mjs

release-acceptance: release-acceptance-check
	node tests/release-acceptance.mjs

pi-package-install:
	npm ci

pi-package-typecheck:
	npm run typecheck

pi-package-test:
	npm test

pi-package-check: pi-package-typecheck pi-package-test
	npm run check:package
	npm run test:pi-load

check: check-frontend generic-skill-check pi-package-check release-contract-check release-acceptance-check chromium-security-check benchmark-compile resource-acceptance-check rust-checks
