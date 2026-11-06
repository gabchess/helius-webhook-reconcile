<!-- source: https://www.helius.dev/docs/faqs/webhooks.md ; fetched: 2026-11-06T17:04Z with curl ; vendored unchanged below this line -->
> ## Documentation Index
> Fetch the complete documentation index at: https://www.helius.dev/docs/llms.txt
> Use this file to discover all available pages before exploring further.

# Webhooks FAQs

> Get answers to the most common questions about webhooks including management, network support, retries, and troubleshooting

## Credits and Rate Limits

> [accordion] How many addresses can I input for 1 webhook?
>
> Up to 100,000 addresses using the [API](https://www.helius.dev/docs/api-reference/webhooks/create-webhook.md).

> [accordion] Will I be charged credits if I change the webhook URL in the Helius dashboard after creating the webhook?
>
> Yes, editing a webhook via the API or dashboard costs 100 credits per request. This applies to any modification including URL changes, adding/removing addresses, or updating transaction types.

> [accordion] How many credits does it cost to receive a webhook notification?
>
> Each webhook event costs 1 credit when Helius processes and sends the event to your endpoint. You are charged regardless of whether your endpoint successfully processes the webhook or returns an error.

> [accordion] How much does it cost to create, edit, and delete webhooks?
>
> All webhook management operations (create, edit, delete) cost 100 credits per request when performed via the API. This covers the processing required to update your webhook configurations.

## Supported Networks

> [accordion] Are webhooks available on Solana Devnet?
>
> Yes. If you create a webhook through the UI, choose “devnet” as “Network”. If you create it through the Webhook API, set the “webhookType” parameter to "rawDevnet", "enhancedDevnet" or “discordDevnet” depending on your desired webhook type.

> [accordion] Can I input 'localhost' for my webhook URL?
>
> No—the URL must be publicly reachable over HTTPS. Helius cannot access local development servers via localhost URLs.

## Using Webhooks

> [accordion] How can I verify that the webhook came from Helius?
>
> Set an authorization header when creating or updating the webhook. Helius echoes this value in the `Authorization` header when sending data to your webhook endpoint, allowing you to verify the request's authenticity.

> [accordion] What is the difference between 'raw' and 'enhanced' webhooks?
>
> **Raw webhooks** return regular Solana transaction data directly from the blockchain. When a transaction occurs for the monitored addresses, the raw transaction data is sent to your webhook URL.
>
> **Enhanced webhooks** provide Helius' interpreted transaction types. We parse over 100 types of Solana transactions (NFT listings, DeFi swaps, transfers, etc.) and abstract them into our own schema.
>
> Use raw webhooks if you want all transaction data without abstractions, or enhanced webhooks if you want built-in transaction type detection and parsing.

> [accordion] When does the Webhook send a notification?
>
> Webhooks send notifications immediately after a matching transaction is confirmed on the blockchain.

## Retries

> [accordion] How should my endpoint acknowledge a webhook event?
>
> Your endpoint must return a `200` status code within **1 second** to acknowledge successful receipt. As a best practice, respond with `200` immediately and handle any processing asynchronously — this prevents timeouts caused by slow business logic.

> [accordion] What is the retry policy for webhooks?
>
> Helius retries delivery when your endpoint returns a `5xx` error, a `4xx` error (except `403`), times out, or fails due to a connection error or DNS failure. We retry up to **3 times** with a **1-second gap** between each attempt.
>
> If all retry attempts fail, the event is **permanently lost** — there is no re-queue or second delivery cycle.
>
> Monitor your webhook logs in the [Helius Dashboard](https://dashboard.helius.dev/webhooks) to identify delivery failures and investigate endpoint issues.
>
> We also offer customizable retry policies for enterprise plans.

> [accordion] Is a webhook event delivered exactly once?
>
> Helius may deliver the same webhook event multiple times. This may happen due to retry logic or network issues.
>
> Your webhook handler must be prepared to handle duplicates gracefully — processing the same event multiple times should produce the same result as processing it once.
>
> To handle duplicates, use the transaction signature or a unique event identifier to track which events you've already processed.

> [accordion] Do webhooks post failed transactions?
>
> It depends on the webhook type. Enhanced webhooks do not send failed transactions, but raw webhooks do include both successful and failed transactions.

## Auto-Disabling

> [accordion] Why was my webhook automatically disabled?
>
> Helius monitors endpoint health and automatically disables webhooks with a **≥ 95% delivery failure rate**. Paid plans (Dev and above) are evaluated over a 7-day window, while Free plan webhooks are evaluated over a 24-hour window. This prevents wasted delivery attempts against unreachable endpoints.
>
> Common causes include an unreachable endpoint URL, your server returning consistent error responses, or network/firewall issues blocking Helius deliveries.

> [accordion] How do I re-enable a disabled webhook?
>
> 1. Fix the underlying issue with your endpoint.
> 2. Log in to the [Helius Dashboard](https://dashboard.helius.dev/webhooks).
> 3. Find the disabled webhook and toggle it back on.
>
> You can also re-enable webhooks programmatically via the [Toggle Webhook API endpoint](https://www.helius.dev/docs/api-reference/webhooks/toggle-webhook.md) by sending a PATCH request with `{ "active": true }`.

> [accordion] Will my webhook get disabled again right after I re-enable it?
>
> No. After re-enabling, the webhook enters a **24-hour grace period** during which it will not be automatically disabled. This gives you time to verify your endpoint is healthy before the next evaluation.

> [accordion] Will I be notified when a webhook is auto-disabled?
>
> Yes — users on the **Dev plan and above** receive an email notification when a webhook is automatically disabled. Free plan users can monitor webhook status in the [Helius Dashboard](https://dashboard.helius.dev/webhooks).

> [accordion] Can I disable a webhook without deleting it?
>
> Yes. You can toggle webhooks on and off from the [Helius Dashboard](https://dashboard.helius.dev/webhooks) or via the [Toggle Webhook API endpoint](https://www.helius.dev/docs/api-reference/webhooks/toggle-webhook.md). This lets you temporarily pause deliveries without losing your webhook configuration.

## Troubleshooting Webhooks

> [accordion] Why is my webhook dropping data?
>
> Data dropping typically occurs when your endpoint encounters an error, causing webhook retries that eventually fail and lead to data loss. Check your webhook logs in the [dashboard](https://dashboard.helius.dev/webhooks), ensure sufficient client-side logging is in place, and [contact support](https://www.helius.dev/docs/support/contact-support) if the problem persists.

> [accordion] Why aren't my webhooks working?
>
> Ensure the selected transaction type matches the transactions you're monitoring. Start with the "ANY" transaction type to identify if it's a general issue or a type mismatch. Use [webhook.site](http://webhook.site/) to test if the problem lies with your endpoint configuration.

> [accordion] Why does my webhook not send me all transactions of a certain type (such as TOKEN\_MINT)?
>
> The `TOKEN_MINT` webhook only triggers for specific programs like Candy Machine V1, and Solana Program Library. It doesn't cover all token minting events across the entire blockchain. For broader monitoring, consider using a more general account or program subscription via webhooks with "ANY" transaction type filtering.

***

## Need More Help?

> [card] [Contact Support](https://www.helius.dev/docs/support/contact-support)
>
> Get help from our team through Discord, chat, or email support.

> [card] [Status Page](https://www.helius.dev/docs/support/status-page)
>
> Check real-time service availability and performance information.


This documentation is built and hosted on [Mintlify](https://mintlify.com), a developer documentation platform.