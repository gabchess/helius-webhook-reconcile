# helius-webhook-reconcile

Finds the transactions missing from your Helius webhook log. It diffs the signatures you logged against `getTransactionsForAddress`, refetches only the missing ones, and writes them as webhook payloads you can replay.

## Quick start

```
cargo run -q -p webhook-reconcile -- diff --received fixtures/gtfa-live-received.json --reference fixtures/gtfa-live.json
echo $?
```

Offline, no key. The reference is a real Helius call (`run live`) and the received set is `mocked`, so the 2 missing rows are a test. Exit code 0 means nothing is missing under `--status` (default `succeeded` skips failed transactions; `--status any` includes them), 1 means events are missing, 2 means the run failed.

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

## Live run

Set `HELIUS_API_KEY` (from https://dashboard.helius.dev) in the environment. The tool never prints it. The live calls go to Helius mainnet. Write a backfill for an outage window:

```
cargo run -q -p webhook-reconcile -- diff --received received.json \
  --live-reference --address <address> --slot-gte <first slot> --slot-lte <last slot> \
  --live --backfill-out backfill.json
```

The backfill is raw webhook shape (`slot`, `blockTime`, `transaction`, `meta`, oldest slot first) with no enhanced fields, so check that your endpoint accepts raw payloads before you replay. Rows your webhook's filters exclude are not missing, so check the filters before you replay.

`--received` takes a JSON array of transactions or signature strings, or an object that holds one under `payloads`, `data`, `result`, `response` or `events`. Flatten saved POST bodies with `jq -s 'flatten(1)' posts/*.json > received.json`. Add `--idl <idl.json>` to decode Anchor events.

## Requirements

Rust, or Docker (prefix commands with `docker run --rm -v "$PWD:/w" -w /w rust:1`, and add `-e HELIUS_API_KEY` for live runs). Tests run offline with no key. They print `test result: ok`, 12 passed and 88 passed:

```
cargo test -p webhook-reconcile --locked
```

To run one test, add its name after the command.

## License

MIT, see [LICENSE](LICENSE).
