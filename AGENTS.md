# AGENTS.md

Rules for any agent that changes this repo.

## What this is
`webhook-reconcile` is a Rust CLI that finds Helius webhook events that were never delivered. It diffs the signatures a webhook consumer received against a Helius reference set. It also holds a small Anchor program, `programs/event-lab`, that emits events for tests.

## Layout
- `crates/webhook-reconcile` holds the Rust CLI and its tests.
- `programs/event-lab` holds the Anchor program. It is a stub with no logic. Chunk 2 adds the real instruction and event.
- `lab/` holds the Node tests that run the built program in LiteSVM.

## Run, test and lint
There is no local cargo. Run everything in Docker. Pass the Windows form of the path to `docker -v`, because Bash remaps `$PWD`. Also set `MSYS_NO_PATHCONV=1` in the Bash tool, or Git Bash rewrites `-w /w` to `W:/`.

```
docker run --rm -v "C:/Users/gavaf/code/helius-webhook-reconcile:/w" -w /w rust:1 cargo test -p webhook-reconcile
docker run --rm -v "C:/Users/gavaf/code/helius-webhook-reconcile:/w" -w /w rust:1 sh -c "rustup component add rustfmt clippy && cargo fmt --check && cargo clippy -p webhook-reconcile --all-targets -- -D warnings"
docker run --rm -v "C:/Users/gavaf/code/helius-webhook-reconcile:/w" -w /w solanafoundation/anchor:v1.0.2 bash -c "export HOME=/root && anchor build --ignore-keys && cd lab && npm ci --no-audit --no-fund && npm test"
```

CI runs the same steps. The required check names on `main` are `rust` and `anchor`. Do not rename those jobs.

## Key rule
`HELIUS_API_KEY` comes only from `C:/Users/gavaf/.secrets/helius.env`. Load it in the subshell of one command, for example `( set -a; . ~/.secrets/helius.env; set +a; <cmd> )`. Never print it, log it, commit it, or save a URL that contains it. Tests run offline with no key.

## Branches and pull requests
- Branch names start with `claude/`. Never push to `main` and never force push.
- One pull request is one chunk. CodeRabbit reviews it automatically. Never pass `--use-credits`.
- A non-author agent reviews and merges. The author never approves its own pull request.
- Every output of a live call is tagged `run live`. Recorded data is tagged `mocked` or `litesvm`.
