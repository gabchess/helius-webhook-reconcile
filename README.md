# helius-webhook-reconcile

A Rust CLI that finds Helius webhook events that were never delivered, by diffing what your consumer received against a Helius reference set.

## Problem

Helius webhooks can drop events, and the consumer has no signal that one is missing. Support threads keep asking the same question: "my webhook stopped, what did I miss?" This tool answers it from data.

## Quick start (offline, no key)

One command, no key, no network. It diffs a recorded Helius reference (100 rows from getTransactionsForAddress) against a mocked set of received webhook payloads.

```
cargo run -q -p webhook-reconcile -- diff \
  --received fixtures/gtfa-live-received.json \
  --reference fixtures/gtfa-live.json
```

Output (mocked), exit code 1 because events are missing:

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
```

The reference holds 5 never-received rows. 3 of them are failed transactions. Enhanced webhooks do not send failed transactions, so they are not a lost webhook, and `--status succeeded` (the default) leaves them out and counts them in the "not counted" line. `--status any` lists all 5. `--status failed` lists only the failed ones. The three values match `filters.status` of getTransactionsForAddress.

No local cargo? Run it in Docker with `docker run --rm -v "$PWD:/w" -w /w rust:1 cargo run -q -p webhook-reconcile -- diff ...`.

## Setup

Live runs need a Helius API key. Create one at https://dashboard.helius.dev and export it as `HELIUS_API_KEY`. Never commit it. The offline tests need no key.

## Status

| Part | Status |
| --- | --- |
| Diff core (missing, duplicates, extra) | passes, mocked (offline tests on recorded fixtures) |
| Failed-transaction `--status` filter | passes, mocked |
| Signatures mode and full mode | not run |
| Anchor event decode | not run |
| Paging across a long outage window | not run |
| Live Helius call | not run |
| CI | runs on every pull request (`rust` and `anchor` checks) |

## License

MIT
