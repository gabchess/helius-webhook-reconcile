# helius-webhook-reconcile

A Rust CLI that finds the Helius webhook events your endpoint never received. It diffs the signatures you logged against `getTransactionsForAddress`, refetches only the missing ones, and writes them as webhook payloads you can replay.

The Helius webhooks FAQ says: "If all retry attempts fail, the event is permanently lost" ([source](https://www.helius.dev/docs/faqs/webhooks), read 2026-11-06). This tool covers the step after that.

## Data tags

Each output block below names where its data came from.

- `run live`: recorded from a real Helius call.
- `mocked`: built by hand.
- `litesvm`: recorded from the `event-lab` test program in LiteSVM.

## Quick start (offline, no key)

```
cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json
echo $?
```

Output. The reference is `run live` (one `getTransactionsForAddress` call). The received set is `mocked`, so the 2 missing rows test the tool and are not a real loss.

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

Exit code 0 means nothing is missing. 1 means events are missing. 2 means the run failed, for example on bad input.

Enhanced webhooks do not send failed transactions, so the default `--status succeeded` leaves the 3 failed rows out. Use `--status any` for a raw webhook.

No local Rust? Prefix the command with `docker run --rm -v "$PWD:/w" -w /w rust:1`.

## What you need

| Need | When |
| --- | --- |
| Rust, or Docker with `rust:1` | every command |
| `HELIUS_API_KEY` from https://dashboard.helius.dev | only `--live-reference` and `--live` |
| Docker with `solanafoundation/anchor:v1.0.2` | only to rebuild `programs/event-lab` and re-record its fixtures |

## Key setup

The tool reads `HELIUS_API_KEY` from the environment. `.env.example` is a template only. Keep the filled file outside the repo and load it for one command:

```
( set -a; . ~/.secrets/helius.env; set +a; cargo run -q -p webhook-reconcile -- diff ... --live )
```

The tool never prints the key. Its errors carry only the error type and code.

## Live run

Page an outage window, refetch the missing signatures, and write a backfill file:

```
cargo run -q -p webhook-reconcile -- diff --received received.json \
  --live-reference --address <address> --slot-gte <first slot> --slot-lte <last slot> \
  --live --backfill-out backfill.json
```

`backfill.json` holds raw webhook payloads (`slot`, `blockTime`, `transaction`, `meta`), oldest slot first. A recorded run on Jupiter v6 is in [docs/run-log/2026-11-06-live-jupiter-v6.txt](docs/run-log/2026-11-06-live-jupiter-v6.txt): 102 reference rows (`run live`) and 7 missing against the `mocked` received set.

Other flags: `--idl` decodes the Anchor events of the missing transactions. `--json` prints the report as JSON. `--reference-pages` and `--refetch-from` replay recorded responses offline.

`helius-triage` turns a `--json` report and a ticket into a draft reply that a person reads before sending. See [skills/helius-triage/SKILL.md](skills/helius-triage/SKILL.md).

## How to know it works

```
cargo test -p webhook-reconcile --locked
```

142 offline tests, no key. The recorded run printed 12, 86 and 44 passed ([docs/run-log/2026-11-06-cargo-test.txt](docs/run-log/2026-11-06-cargo-test.txt)). CI runs them and the LiteSVM recording of `event-lab` on every pull request.

## Layout

```
crates/webhook-reconcile/   # the CLI: diff, paging, refetch, decode, backfill, triage
programs/event-lab/         # Anchor program that emits test events
lab/                        # records event-lab in LiteSVM into fixtures/
fixtures/                   # recorded Helius responses, tickets, FAQ snapshot
skills/helius-triage/       # agent skill for the triage step
docs/                       # run logs and the demo script
```

## What it does not do

- It never sends a transaction or loads a keypair.
- It does not post the backfill. It writes a file and you replay it.
- It does not build the enhanced webhook shape. Helius computes its `type`, `source`, `description` and `events` fields.

## License

MIT, see [LICENSE](LICENSE).
