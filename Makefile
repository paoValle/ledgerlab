# `cargo` is the native tool and that is enough. The Makefile repeats nothing: the demo below is
# the pipeline the README claims, so it cannot drift from the documentation.
.DEFAULT_GOAL := help
DEMO_LOG := target/ledgerlab-demo.jsonl

.PHONY: help setup demo verify reconcile report test lint fmt fmt-check ci clean

help: ## show this help
	@grep -E '^[a-z-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}'

setup: ## fetch dependencies
	cargo fetch

demo: ## build the demo ledger from examples/ (idempotent: run it twice)
	@rm -f $(DEMO_LOG)
	@cargo run --quiet --release -- open --log $(DEMO_LOG) --accounts examples/chart.jsonl
	@cargo run --quiet --release -- apply --log $(DEMO_LOG) --pending examples/activity.jsonl
	@echo "--- second run of the same file: nothing may be applied twice ---"
	@cargo run --quiet --release -- apply --log $(DEMO_LOG) --pending examples/activity.jsonl

verify: demo ## audit the demo ledger
	@cargo run --quiet --release -- verify --log $(DEMO_LOG)

reconcile: demo ## reconcile against the statement that diverges (expects exit 1)
	@cargo run --quiet --release -- reconcile --log $(DEMO_LOG) --account assets:bank --statement examples/statement-divergent.jsonl || true

report: demo ## write reports/latest.md from the three statements
# `report` exits 1 when a divergence is real, because that is what the command is for. The artifact
# is written either way, so the target ignores the code: the gate belongs to CI, not to a make
# target that exists to produce a file.
	@cargo run --quiet --release -- report --log $(DEMO_LOG) --account assets:bank \
		--statement clean=examples/statement-clean.jsonl \
		--statement divergent=examples/statement-divergent.jsonl \
		--statement pending=examples/statement-pending.jsonl \
		--out reports/latest.md > /dev/null || true
	@printf "wrote reports/latest.md (%s lines)\n" "$$(wc -l < reports/latest.md)"

test: ## the invariants, by generation
	cargo test --all-targets

lint: ## clippy, warnings are errors
	cargo clippy --all-targets --all-features -- -D warnings

fmt: ## writes the files
	cargo fmt

fmt-check: ## checks formatting without writing
	cargo fmt --check

ci: fmt-check lint test ## exactly what runs in CI

clean: ## removes the demo ledger
	rm -f $(DEMO_LOG)
