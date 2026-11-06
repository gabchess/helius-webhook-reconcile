# helius-webhook-reconcile

A Rust CLI that finds the transactions missing from your webhook log. It diffs the signatures you logged against what Helius has on chain (`getTransactionsForAddress`), refetches only the missing ones, and writes them as webhook payloads you can replay.

The Helius webhooks FAQ says: "If all retry attempts fail, the event is permanently lost" ([source](https://www.helius.dev/docs/faqs/webhooks), read 2026-11-06). This tool covers the step after that.

## Quick start (offline, no key)

```
cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json
echo $?
```

Data tags: `run live` is a real Helius call, `mocked` is hand-built, `litesvm` comes from LiteSVM. Here the received set is `mocked`, so the 2 missing rows are a test, not a real loss.

```
reference 100 (run live), received 97 (mocked), status succeeded
missing: 2
  64fNCAWt3Ezx7bdK8wsBM61Q18Gz6xK5SZhEuGLkZ8FMXGKf9PxkZAhdPVvCVacG3gnF5A9S4W7AuXW2NcruyLiw slot=466122531 failed=false
  2SpJCRwDEt8fjXHGfmiL8oYyFF8hRDAV9XF3KUAzcWvopj9v3m2QHbJhZWpkqph5j5t5dV9ps954JJrmaWCTfadJ slot=466122528 failed=false
not counted by --status: 3 (never received; rerun with --status any to list them)
duplicates: 2
  64GSSkdqb81kjFRxqwrcKGvu3Yooq2XUF9G9y6xTSohpbekxTV2ttmxhZe9FTNTg39B2TC9iuGpBwdybtPrwWCNn x2
  3mYQiLCc6UMna1en8v6vb3sEyhup2UmosZKK9aegT7BnvSrdGNLWKmYBEi1VDt822yyXCMz6ijJ8A1sGuNNHt3VA x2
extra (received, not in reference): 0
1
```

Exit code 0 means nothing is missing, 1 means events are missing, 2 means the run failed.

Enhanced webhooks do not send failed transactions, so the default `--status succeeded` leaves the 3 failed rows out. Use `--status any` for a raw webhook.

No local Rust? Prefix the command with `docker run --rm -v "$PWD:/w" -w /w rust:1`.

## What you need

| Need | When |
| --- | --- |
| Rust, or Docker with `rust:1` | every command |
| `HELIUS_API_KEY` from https://dashboard.helius.dev | only `--live-reference` and `--live` |
| Docker with `solanafoundation/anchor:v1.0.2` | only to rebuild `programs/event-lab` and re-record its fixtures |

## Key setup

The tool reads `HELIUS_API_KEY` from the environment. Keep a filled copy of `.env.example` outside the repo and load it for one command:

```
( set -a; . ~/.secrets/helius.env; set +a; cargo run -q -p webhook-reconcile -- diff ... --live )
```

The tool never prints the key.

## Live run

Page an outage window, refetch the missing rows and write a backfill:

```
cargo run -q -p webhook-reconcile -- diff --received received.json \
  --live-reference --address <address> --slot-gte <first slot> --slot-lte <last slot> \
  --live --backfill-out backfill.json
```

The live calls go to Helius mainnet (`mainnet.helius-rpc.com`).

Rows that your webhook's filters exclude are not missing. Check the filters before you replay.

`--received` takes one flat JSON array of transactions or signature strings, or an object that holds one under `payloads`, `data`, `result`, `response` or `events`. One array per POST body does not parse. Flatten it first with `jq -s 'flatten(1)' posts/*.json > received.json`.

`backfill.json` holds raw webhook payloads (`slot`, `blockTime`, `transaction`, `meta`), oldest slot first.

Other flags: `--idl` decodes Anchor events. `--json` prints JSON. `--reference-pages` and `--refetch-from` replay recorded responses offline.

## How to know it works

```
cargo test -p webhook-reconcile --locked
```

Offline, no key. It prints `test result: ok`, 12 passed for the CLI and 88 passed for `tests/offline.rs`.

## Layout

```
crates/webhook-reconcile/   # the CLI: reference, diff, refetch, helius, decode, backfill, report
programs/event-lab/         # Anchor test program
programs/event-lab/record/  # LiteSVM recorder for fixtures/
fixtures/                   # recorded Helius responses and test inputs
```

## What it does not do

- It never sends a transaction or loads a keypair.
- It does not post the backfill. It writes a file and you replay it.
- It does not build the enhanced webhook fields (`type`, `source`, `description`, `events`).

## License

MIT, see [LICENSE](LICENSE).
