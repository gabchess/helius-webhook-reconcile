# helius-webhook-reconcile

A Rust CLI that finds Helius webhook events that were never delivered, by diffing what your consumer received against a Helius reference set.

## Problem

Helius webhooks can drop events, and the consumer has no signal that one is missing. Support threads keep asking the same question: "my webhook stopped, what did I miss?" This tool answers it from data.

## Quick start (offline, no key)

Not built yet. Chunk 1 adds the diff core and its offline tests.

## Setup

Live runs need a Helius API key. Create one at https://dashboard.helius.dev and export it as `HELIUS_API_KEY`. Never commit it. The offline tests need no key.

## Status

| Part | Status |
| --- | --- |
| Diff core | not run |
| Signatures mode and full mode | not run |
| Anchor event decode | not run |
| Paging across a long outage window | not run |
| Live Helius call | not run |
| CI | not run |

## License

MIT
