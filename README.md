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

### Page across an outage window and replay the gap (offline)

A long outage has more rows than one page holds. `--reference-pages` replays recorded getTransactionsForAddress pages through the same paging loop that `--live-reference` uses on Helius. `--backfill-out` writes the missing transactions as an array of webhook payloads, oldest slot first, so a team can post them to the endpoint that never got them.

```
cargo run -q -p webhook-reconcile -- diff --received fixtures/event-lab-received.json --reference-pages fixtures/event-lab-pages.json --status any --refetch-from fixtures/event-lab-refetch.json --idl fixtures/event_lab.idl.json --backfill-out backfill.json
```

Output (rows `litesvm`, paging and received set `mocked`), exit code 1 because events are missing:

```
reference 10 (mocked), received 10 (mocked), status any
missing: 3
  3ChjaDHLvRPXmj5Z9oRzNT5TMRyoEU1VYkgxNBPyW3FwvAwvDQ2mEX4o7UXNwTjWfxoYQBdbZx7Uo9n8n2Jf6f66 slot=102 failed=false
  ...
decoded events from missing txs: 3
backfill: 3 webhook payloads written to backfill.json
```

The fixture has 4 pages. Page 2 is empty but carries a token, and page 3 repeats the last row of page 1, as overlapping pages do. The 11 rows read come out as 10, one per signature.

How the paging stays honest:

- The loop stops when a response has no `paginationToken`. An empty page that still has a token does not stop it.
- The window (`--slot-gte`, `--slot-lte`, `--time-gte`, `--time-lte`) and `--page-limit` go on every page request. Only the token changes.
- A token that a response returns a second time stops the run with an error. It never loops.
- `--max-pages` (default 50) caps the loop. Hitting the cap while the server still has more is an error, because a short reference would report false `extra` rows.
- Errors carry counts and fixed labels only, never a token value or server text.
- A signature that appears on two pages is kept once.

The backfill is the raw webhook shape: `slot`, `blockTime`, `transaction` and `meta` of each transaction. Helius says raw webhooks "return regular Solana transaction data directly from the blockchain", and a raw webhook includes failed transactions. The tool does not emit the enhanced shape, whose `type`, `source`, `description` and `events` only Helius can compute. Backfill output from the recorded run above is `mocked`. It needs `--refetch-from` or `--live` to get the full transactions. `--backfill-out -` prints the array to stdout and sends the report to stderr.

Failed transactions: with `--status any`, a failed transaction is in the backfill with its `meta.err`, as a raw webhook would send it. Its `Program data:` events are skipped when decoding, because the runtime rolled the transaction back and the event never took effect.

No local cargo? Run it in Docker with `docker run --rm -v "$PWD:/w" -w /w rust:1 cargo run -q -p webhook-reconcile -- diff ...`.

## Support triage

The `helius-triage` skill (`skills/helius-triage/SKILL.md`) takes a "my webhook stopped" ticket through the diff to a drafted support reply. A person reviews the draft before it goes out.

```
cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json --json > report.json
cargo run -q -p webhook-reconcile --bin helius-triage -- --ticket fixtures/triage-ticket-webhook-stopped.txt --report report.json
```

The reply repeats the data tags from the report (reference `run live`, received `mocked` here), so a mocked gap is never presented as a real loss. Its quotes are whole sentences copied from `fixtures/helius-faq-webhooks.md`, a snapshot of the Helius webhooks FAQ with its URL and fetch date in the first line. A test fails if a quote is not in the snapshot. This is a template over a diff and a pinned page, not retrieval over a corpus.

AI judgment sits behind `--ai` and is off by default. With the flag off, keyword rules decide and no model is called, which `triage_flag_off_calls_no_model` proves with a recording fake. With `--ai`, TypeSafe Jev answers one question: is this a webhook delivery ticket? Its answer only decides whether to draft. The model never sees the ticket text: the request carries only the ticket's lowercase letter words of 1 to 15 letters (`ticket_words`), so no digit, symbol or long string leaves the machine. A secret made only of letters (15 or fewer) or a short letter fragment of a longer one can still survive, and a test pins that limit. `--ai --jev-from fixtures/jev-classify-mocked.json` replays the fixture offline. The fixture is `mocked`: hand-built from Jev's documented response shape, with invented probabilities. The live Jev call is not run.

## Setup

Live runs need a Helius API key. Create one at https://dashboard.helius.dev and export it as `HELIUS_API_KEY`. Never commit it. The offline tests need no key. Load the key only inside the subshell of one command, for example `( set -a; . ~/.secrets/helius.env; set +a; webhook-reconcile diff ... --live )`. The tool never prints the key. Error messages carry only the error type and code, never server text or the request URL.

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
| Paging across a long outage window (token loop guard, page cap, window bounds, empty page) | passes, mocked (recorded pages replayed, no network) |
| Duplicate reference rows kept once | passes, mocked |
| Backfill in raw webhook payload shape, replay closes the gap | passes, mocked (litesvm rows) |
| Events of failed transactions skipped in decode | passes, mocked |
| Live paging, `--live` refetch and backfill on Jupiter v6 (102 reference rows, slots 466122525 to 466122534, page limit 40). 7 missing and 7 backfill payloads, against the mocked received set from chunk 1, so the 7 are an artifact of the mock | run live (2026-11-06, key never printed), received set mocked |
| Support triage: diff report to drafted reply, verbatim FAQ quotes, tags repeated, failed rows not called lost | passes, mocked (report from the run live reference and a mocked received set) |
| Jev ticket classifier behind `--ai`, off by default, flag off calls no model | passes, mocked (hand-built fixture). Live Jev call: not run |
| CI | runs on every pull request (`rust` and `anchor` checks) |

## License

MIT
