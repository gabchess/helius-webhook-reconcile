# helius-webhook-reconcile

A Rust CLI that finds Helius webhook events that were never delivered, by diffing what your consumer received against a Helius reference set.

The tags in this README say where each data set came from: `run live` means recorded from a real Helius call, `mocked` means built by hand, and `litesvm` means recorded from the event-lab program running in LiteSVM.

Proof from the recorded run in [docs/run-log/2026-11-06-quickstart-offline.txt](docs/run-log/2026-11-06-quickstart-offline.txt): `reference 100 (run live), received 97 (mocked), status succeeded`, then `missing: 2`. The reference is one real getTransactionsForAddress call. The received set is mocked, so the 2 missing rows test the tool and are not a real loss.

Demo script: [docs/demo.tape](docs/demo.tape), for charmbracelet/vhs. It is not rendered yet, because vhs is not installed on the build machine. [docs/demo.txt](docs/demo.txt) is the transcript of the same commands.

## Quick start (offline, no key)

One command, no key, no network. It diffs a recorded Helius reference (100 rows from getTransactionsForAddress) against a mocked set of received webhook payloads.

```
cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json  # diff the mocked received set against the recorded reference
echo $?  # prints 1, the expected result here, because events are missing
```

Exit code 1 is the expected result for this data, not an error. Exit 0 means nothing is missing. Exit 2 means the run failed, for example on bad input.

Output, with the reference `run live` and the received set `mocked`:

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

The reference holds 5 rows that were never received. 2 succeeded on chain and are listed as missing. 3 failed on chain and are not counted. Enhanced webhooks do not send failed transactions, so `--status succeeded` (the default) leaves them out and counts them on the "not counted" line. `--status any` lists all 5. The mocked received set includes failed transactions on purpose, to exercise the filter.

No local cargo? Run the same command in Docker with `docker run --rm -v "$PWD:/w" -w /w rust:1 cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json`.

## Problem

The Helius webhooks FAQ (https://www.helius.dev/docs/faqs/webhooks, read 2026-11-06) says: "If all retry attempts fail, the event is permanently lost". This tool works the step after that. It names the missing signatures from getTransactionsForAddress and writes them as webhook payloads to replay.

At least one team built its own reconcile step for this. A pull request in another project, https://github.com/wessteven/buyguy-free-crawler/pull/74 (opened 2026-11-04, not a Helius ticket), plans to "Reconcile `wallet+signature` with original webhook data" after what it calls "the 2026-11-01 05:20 UTC to 2026-11-03 08:55 UTC webhook outage". The PR does not say whether the outage was on the Helius side.

Helius already ships the pieces around the gap. The core-ai skill file [webhooks.md](https://github.com/helius-labs/core-ai/blob/main/.agents/skills/helius/references/webhooks.md) tells developers to "Deduplicate by signature" and has no replay or backfill text. [helius-cli](https://github.com/helius-labs/core-ai/tree/main/helius-cli) lists, creates, updates and deletes webhooks. The FAQ offers "customizable retry policies for enterprise plans". The same FAQ answer says: "Monitor your webhook logs in the [Helius Dashboard](https://dashboard.helius.dev/webhooks) to identify delivery failures and investigate endpoint issues." This tool does not replace any of them. It adds three steps after the logs: a signature-level diff against getTransactionsForAddress, a refetch of only the missing signatures, and a webhook-shaped backfill to replay.

## How I would handle this as your Solutions Engineer

The job post (https://jobs.ashbyhq.com/helius/e98b2c54-2baa-4bc5-8bd8-eb5b1b35b910) asks me to "Serve as the technical front door for developers using Helius APIs and streaming products" and to "Troubleshoot issues, reproduce bugs, and work with engineering to get fixes shipped". This is one ticket, worked end to end.

1. Report. A developer writes in that their webhook stopped. The PR above is a real case of it, from another project. The recorded ticket is `fixtures/triage-ticket-webhook-stopped.txt` (mocked).
2. Triage. The quick start command prints the signatures that the reference has and the received set lacks. Code: `crates/webhook-reconcile/src/main.rs` and `src/lib.rs`. The received set is mocked.
3. Reproduce. `fixtures/gtfa-live.json` is a recorded Helius reference (run live), so the same gap shows on any machine with no key. On Helius itself, the live run in [docs/run-log/2026-11-06-live-jupiter-v6.txt](docs/run-log/2026-11-06-live-jupiter-v6.txt) paged 102 reference rows for Jupiter v6 (run live) and found 7 missing against the mocked received set, so the 7 are an artifact of the mock and not a loss.
4. Backfill. `--backfill-out backfill.json`, with `--refetch-from` or `--live`, writes the missing transactions as raw webhook payloads, oldest slot first, ready to replay into the endpoint. Proof: `backfill_has_the_raw_webhook_shape_and_replays_the_gap` in `tests/offline.rs` (mocked, rows from litesvm). The live run above wrote 7 payloads (run live refetch, mocked received set).
5. Reply. `skills/helius-triage/SKILL.md` takes the ticket and the diff report to a drafted reply that a person reviews before it goes out. First write the quick start diff as a JSON report, then pass the report to the triage CLI:

   ```
   cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json --json > report.json
   cargo run -q -p webhook-reconcile --bin helius-triage -- --ticket fixtures/triage-ticket-webhook-stopped.txt --report report.json
   ```

   The recorded draft is in [docs/run-log/2026-11-06-triage-offline.txt](docs/run-log/2026-11-06-triage-offline.txt).

Two guarantees hold for the draft, and tests in `tests/triage.rs` pin both.

- With `--ai`, the triage CLI asks TypeSafe Jev, a hosted model API at `api.typesafe.ai`, whether the ticket is about webhook delivery. The only ticket text Jev receives is the ticket's words that are in a fixed 104-word list (`JEV_WORDS` in `src/triage.rs`).
- The draft never claims a loss unless both data tags are `run live`.

The job post also asks to "Automate support workflows using internal tooling, scripts and AI-assisted systems". This is that, with Rust, Anchor and the Helius APIs as the evidence.

## Setup

| Need | Where | When |
| --- | --- | --- |
| Rust, or Docker with `rust:1` | `crates/webhook-reconcile` | every command |
| `HELIUS_API_KEY` | create one at https://dashboard.helius.dev, template in `.env.example` | only `--live` and `--live-reference` |
| Docker with `solanafoundation/anchor:v1.0.2` | `programs/event-lab`, `lab/` | only to rebuild the Anchor program and re-record its fixtures |

The tool reads `HELIUS_API_KEY` from the process environment, not from `.env.example`. Never commit the key. Load it only inside the subshell of one command, for example `( set -a; . ~/.secrets/helius.env; set +a; webhook-reconcile diff ... --live )`. The tool never prints the key. Its error messages carry only the error type and code, never server text or the request URL. The offline commands and the tests need no key.

## How it works

1. Diff. `src/lib.rs` and `src/types.rs` compare the received signatures with the reference and report missing, duplicate and extra rows. `--status` matches `filters.status` of getTransactionsForAddress.
2. Page. `src/paging.rs` reads getTransactionsForAddress page by page across an outage window (`--live-reference`, or recorded pages with `--reference-pages`). It stops when a page has no `paginationToken`, errors on a repeated token, and errors at `--max-pages` (default 50) while the server still has more.
3. Refetch. `src/refetch.rs` fetches only the missing signatures in full with `getTransaction`, in batches of 100 (`--live`, or recorded responses with `--refetch-from`).
4. Decode. `src/decode.rs` decodes the Anchor events of the missing transactions through the IDL (`--idl`). It counts only `Program data:` lines that the target program emitted, by tracking the invoke stack. Logs that end in `Log truncated` stop the decode with an error.
5. Backfill. `src/backfill.rs` writes the raw webhook shape (`slot`, `blockTime`, `transaction`, `meta`).
6. Triage. `src/triage.rs` and `src/bin/helius-triage.rs` turn the `--json` report into a drafted reply. Its quotes are whole sentences from `fixtures/helius-faq-webhooks.md`, a snapshot of the Helius FAQ with its URL and fetch date in the first line.

The Anchor program `programs/event-lab` emits a `TransferRecorded` event with `emit!`. `lab/record.test.mjs` runs it in LiteSVM, checks the events with a second decoder in JavaScript, and records the `fixtures/event-lab-*.json` files that the cargo tests decode. CI re-records them and fails if any fixture changes.

AI judgment sits behind `--ai` and is off by default. With the flag off, keyword rules decide and no model is called, which `triage_flag_off_calls_no_model` proves with a recording fake. With `--ai`, Jev answers one question only, whether this is a webhook delivery ticket. `--ai --jev-from fixtures/jev-classify-mocked.json` replays the fixture offline. That fixture is mocked, hand-built from Jev's documented response shape with invented probabilities. Jev live call: not run.

## Tests and proof

```
cargo test -p webhook-reconcile --locked  # 142 offline tests, no key
docker run --rm -v "$PWD:/w" -w /w rust:1 cargo test -p webhook-reconcile --locked  # the same tests with no local cargo
docker run --rm -v "$PWD:/w" -w /w solanafoundation/anchor:v1.0.2 bash -c "export HOME=/root && anchor build --ignore-keys && cd lab && npm ci && npm test"  # build event-lab and record its events in LiteSVM
```

The cargo run of 2026-11-06 printed `test result: ok. 12 passed`, `86 passed` and `44 passed` for `src/main.rs`, `tests/offline.rs` and `tests/triage.rs`, which is 142 passed ([docs/run-log/2026-11-06-cargo-test.txt](docs/run-log/2026-11-06-cargo-test.txt)). CI runs the cargo tests and the Anchor recording on every pull request as the `rust` and `anchor` checks.

## Status

| Part | Status |
| --- | --- |
| Diff core (missing, duplicates, extra) and the `--status` filter | passes, mocked |
| Signatures-mode diff, refetch of only the missing signatures, Anchor event decode | passes, mocked (litesvm rows) |
| Loud errors on truncated logs, truncated payloads and signatures-mode input | passes, mocked |
| Anchor program `event-lab` built and recorded in LiteSVM | passes in CI (`anchor` check) |
| Paging across an outage window (token loop guard, page cap, window bounds, empty page) | passes, mocked |
| Backfill in raw webhook payload shape | passes, mocked (litesvm rows) |
| Live paging, `--live` refetch and backfill on Jupiter v6, slots 466122525 to 466122534, page limit 40 | run live on 2026-11-06, 102 reference rows, 7 missing and 7 payloads against the mocked received set |
| Support triage, diff report to drafted reply | passes, mocked (report from the run live reference and a mocked received set) |
| Jev ticket classifier behind `--ai` | passes, mocked (hand-built fixture). Jev live call: not run |

## Requirements

| Tool | Version |
| --- | --- |
| Rust image | `rust:1` (minor version not pinned) |
| Anchor image | `solanafoundation/anchor:v1.0.2` |
| Rust crates | pinned in `Cargo.lock` |
| Node packages for `lab/` | pinned in `lab/package-lock.json` |

## Layout

```
crates/webhook-reconcile/src/main.rs           # the diff CLI and its flags
crates/webhook-reconcile/src/lib.rs            # the diff core
crates/webhook-reconcile/src/types.rs          # data shapes (reference set, received set, diff report)
crates/webhook-reconcile/src/paging.rs         # getTransactionsForAddress paging loop
crates/webhook-reconcile/src/refetch.rs        # getTransaction refetch, live and recorded
crates/webhook-reconcile/src/decode.rs         # Anchor event decode through the IDL
crates/webhook-reconcile/src/backfill.rs       # raw webhook payloads for replay
crates/webhook-reconcile/src/triage.rs         # diff report to drafted support reply
crates/webhook-reconcile/src/bin/helius-triage.rs  # the triage CLI
crates/webhook-reconcile/tests/                # offline.rs and triage.rs, which with src/main.rs hold the 142 tests
programs/event-lab/                            # Anchor program that emits TransferRecorded
lab/                                           # LiteSVM test that records the event-lab fixtures
fixtures/                                      # 16 recorded inputs, tickets and the FAQ snapshot
skills/helius-triage/SKILL.md                  # the support skill
docs/run-log/                                  # 4 run logs from 2026-11-06
docs/demo.tape                                 # vhs script for the demo
docs/demo.txt                                  # transcript of the demo commands
.env.example                                   # HELIUS_API_KEY template
.github/workflows/ci.yml                       # rust and anchor checks
Cargo.toml                                     # workspace manifest
Anchor.toml                                    # Anchor workspace for event-lab
AGENTS.md                                      # rules for agents that change this repo
```

## What it does not do

- It never sends a transaction. The source has no `sendTransaction` call and loads no keypair.
- It does not post the backfill or the reply anywhere. It writes files and prints text. Its only network calls go to the Helius RPC and, with `--ai`, to Jev.
- It does not produce the enhanced webhook shape, whose `type`, `source`, `description` and `events` fields Helius computes.

## License

MIT, see [LICENSE](LICENSE).
