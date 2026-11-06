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

The mocked `fixtures/gtfa-live-received.json` includes failed transactions on purpose, to exercise the filter. Real enhanced webhooks never send failed transactions.

### Decode the events of the dropped transactions (offline)

The reference below is in signatures mode, which carries no logs. The tool diffs on signatures, refetches only the missing ones in full mode, and decodes their Anchor events through the IDL. Here the refetch replays recorded responses (`litesvm`), so no key and no network are needed.

```
cargo run -q -p webhook-reconcile -- diff   --received fixtures/event-lab-received.json   --reference fixtures/event-lab-signatures.json   --status any   --idl fixtures/event_lab.idl.json   --refetch-from fixtures/event-lab-refetch.json
```

Output (reference `litesvm`, received `mocked`), exit code 1 because events are missing:

```
missing: 3
  3ChjaDHLvRPXmj5Z9oRzNT5TMRyoEU1VYkgxNBPyW3FwvAwvDQ2mEX4o7UXNwTjWfxoYQBdbZx7Uo9n8n2Jf6f66 slot=102 failed=false
  ...
decoded events from missing txs: 3
  3ChjaDHLvRPXmj5Z9oRzNT5TMRyoEU1VYkgxNBPyW3FwvAwvDQ2mEX4o7UXNwTjWfxoYQBdbZx7Uo9n8n2Jf6f66 TransferRecorded {"amount":64,"id":1002,"memo":"order-2","who":"GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB"}
  ...
```

How the decode stays honest:

- Only `Program data:` lines that the event-lab program emitted count. The decoder tracks `Program X invoke [n]` and `Program X success`, so a data line from a program it called is skipped.
- A signatures-mode reference with `--idl` and no refetch source stops with an error that says the reference is in signatures mode. It never returns an empty list.
- Logs that end in `Log truncated` stop the decode with an error, because events may be missing from them. Anchor's docs warn about the same thing: "Ensure the RPC provider does not truncate program logs".
- An event payload that is cut short, or has extra bytes, is an error.
- `--live` refetches from Helius with `getTransaction` in batches of 100. It reads `HELIUS_API_KEY` from the environment and needs the key. `--refetch-from` and `--live` exclude each other.

No local cargo? Run it in Docker with `docker run --rm -v "$PWD:/w" -w /w rust:1 cargo run -q -p webhook-reconcile -- diff ...`.

## Setup

Live runs need a Helius API key. Create one at https://dashboard.helius.dev and export it as `HELIUS_API_KEY`. Never commit it. The offline tests need no key. Load the key only inside the subshell of one command, for example `( set -a; . ~/.secrets/helius.env; set +a; webhook-reconcile diff ... --live )`. The tool never prints the key and removes it from error text.

## Anchor program and LiteSVM recording

`programs/event-lab` is a real Anchor program. `record_transfer` emits a `TransferRecorded` event with `emit!`. `lab/record.test.mjs` runs the built program in LiteSVM, sends 10 transactions, asserts on the events in their logs with a second decoder written in JavaScript, and records the fixtures. The cargo tests then decode those fixtures. The payer key is fixed, so CI re-records and fails if any fixture changes.

```
anchor build --ignore-keys && cd lab && npm ci && npm test
```

## Status

| Part | Status |
| --- | --- |
| Diff core (missing, duplicates, extra) | passes, mocked (offline tests on recorded fixtures) |
| Failed-transaction `--status` filter | passes, mocked |
| Signatures-mode diff, same result as full mode | passes, litesvm |
| Full-mode refetch of only the missing signatures | passes, litesvm (recorded responses replayed, no network) |
| Anchor event decode through the IDL, filtered by the invoke stack | passes, litesvm |
| Loud errors on truncated logs, truncated payloads and signatures-mode input | passes, mocked |
| Anchor program `event-lab` built and recorded in LiteSVM | passes in CI (`anchor` check) |
| Paging across a long outage window | not run |
| Live Helius call, including the `--live` refetch | not run |
| CI | runs on every pull request (`rust` and `anchor` checks) |

## License

MIT
