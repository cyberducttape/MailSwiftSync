.DEFAULT_GOAL := check
.SHELLFLAGS := -eu -o pipefail -c

.PHONY: check format capability-check documentation-check shell-check script-check evidence-check compatibility clippy test build integration audit release-check

# Fast local formatting, documentation, script, and static-analysis checks.
# CI additionally runs platform builds, container integrations, and audits.
check: format capability-check documentation-check shell-check script-check evidence-check compatibility clippy

format:
	cargo fmt --check

capability-check:
	python3 scripts/verify-capability-claims.py

documentation-check:
	python3 tests/verify_capability_claims_test.py
	python3 scripts/verify-markdown-links.py .
	python3 scripts/verify-ui-localization.py

shell-check:
	bash -n scripts/*.sh

script-check:
	bash tests/release-channel.sh
	python3 tests/release_bundle_layout_test.py

evidence-check:
	python3 tests/provider_evidence_generator_test.py
	python3 tests/provider_evidence_schema_test.py
	python3 tests/verify_evidence_gate_test.py

compatibility:
	scripts/verify-compatibility-matrix.sh --release-preview docs/compatibility-matrix.md

clippy:
	cargo clippy --locked --all-targets --all-features -- -D warnings

test:
	cargo test --locked --all-targets --all-features

build:
	cargo build --locked --release --all-features

integration:
	scripts/imap-integration-smoke.sh

audit:
	cargo audit

# Local release-quality gate. CI additionally performs platform builds,
# signing, SBOM generation, provenance, and packaged-container checks.
release-check: check test build audit
