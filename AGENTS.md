# AGENTS.md

Rules for any agent that changes this repo.

## Layout
- `crates/webhook-reconcile` holds the Rust CLI and its tests.
- `programs/event-lab` holds the Anchor program. `record_transfer` emits one `TransferRecorded` event.
- `programs/event-lab/record/` runs the built program in LiteSVM and records `fixtures/event-lab-*.json`. The payer key is fixed, so a fresh recording must equal the committed files. CI fails on any drift.

## Run, test and lint
Run from the repo root. In Git Bash on Windows, set `MSYS_NO_PATHCONV=1` and pass the Windows form of the repo path to `-v`.

```
docker run --rm -v "$PWD:/w" -w /w rust:1 cargo test -p webhook-reconcile --locked
docker run --rm -v "$PWD:/w" -w /w rust:1 sh -c "rustup component add rustfmt clippy && cargo fmt --check && cargo clippy -p webhook-reconcile --all-targets -- -D warnings"
docker run --rm -v "$PWD:/w" -w /w solanafoundation/anchor:v1.0.2 bash -c "export HOME=/root && anchor build --ignore-keys && cd programs/event-lab/record && npm ci --no-audit --no-fund && npm test"
```

Run one test:

```
docker run --rm -v "$PWD:/w" -w /w rust:1 cargo test -p webhook-reconcile --locked --test offline <name>
```

The `anchor` CI job also runs `cargo clippy -p event-lab --all-targets -- -D warnings` and `git diff --exit-code -- fixtures`.

CI runs the same steps. The required checks on `main` are `rust` and `anchor`. Do not rename those jobs.

## When something breaks

| Symptom | Cause | Fix |
| --- | --- | --- |
| The `anchor` job fails at `git diff --exit-code -- fixtures` (`.github/workflows/ci.yml`) | The Anchor program (`programs/event-lab`) and the recorded `fixtures/event-lab-*.json` are two separate things. Editing one does not update the other, so CI's fresh recording differs from the committed files. | Re-record with the CI command: `export HOME=/root && anchor build --ignore-keys && cd programs/event-lab/record && npm ci --no-audit --no-fund && npm test`. Run it in `solanafoundation/anchor:v1.0.2` as in the third command above, then commit the changed fixtures. |
| `docker run` fails with `the working directory 'W:/' is invalid` in Git Bash on Windows | Git Bash rewrites `/w` in `-w /w` into a Windows path. | Set `MSYS_NO_PATHCONV=1` and pass the Windows form of the repo path to `-v`, for example `-v "$(cygpath -w "$PWD"):/w"`. |
| `cargo fmt` fails with `'cargo-fmt' is not installed` | The `rust:1` image ships without rustfmt and clippy. | Run `rustup component add rustfmt clippy` first, as the `rust` job does in `ci.yml`. |

## Key
Tests run offline with no key. `HELIUS_API_KEY` is read from the environment and is needed only for `--live` and `--live-reference`. Never print, log or commit it, or save a URL that contains it.

## Data tags
Tag every output of a live call `run live`. Tag recorded data `mocked` or `litesvm`.

## Branches
Never push to `main` and never force push.
