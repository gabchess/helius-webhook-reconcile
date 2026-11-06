# AGENTS.md

Rules for any agent that changes this repo.

## Layout
- `crates/webhook-reconcile` holds the Rust CLI and its tests.
- `programs/event-lab` holds the Anchor program. `record_transfer` emits one `TransferRecorded` event.
- `lab/` runs the built program in LiteSVM and records `fixtures/event-lab-*.json`. The payer key is fixed, so a fresh recording must equal the committed files. CI fails on any drift.

## Run, test and lint
Run from the repo root. In Git Bash on Windows, set `MSYS_NO_PATHCONV=1` and pass the Windows form of the repo path to `-v`.

```
docker run --rm -v "$PWD:/w" -w /w rust:1 cargo test -p webhook-reconcile --locked
docker run --rm -v "$PWD:/w" -w /w rust:1 sh -c "rustup component add rustfmt clippy && cargo fmt --check && cargo clippy -p webhook-reconcile --all-targets -- -D warnings"
docker run --rm -v "$PWD:/w" -w /w solanafoundation/anchor:v1.0.2 bash -c "export HOME=/root && anchor build --ignore-keys && cd lab && npm ci --no-audit --no-fund && npm test"
```

CI runs the same steps. The required checks on `main` are `rust` and `anchor`. Do not rename those jobs.

## Key
Tests run offline with no key. `HELIUS_API_KEY` is read from the environment and is needed only for `--live` and `--live-reference`. Never print, log or commit it, or save a URL that contains it.

## Data tags
Tag every output of a live call `run live`. Tag recorded data `mocked` or `litesvm`.

## Branches
Never push to `main` and never force push.
