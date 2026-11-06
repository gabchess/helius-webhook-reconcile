// Runs target/deploy/event_lab.so in LiteSVM, sends 10 record_transfer transactions and asserts on
// the events in their logs. It then records fixtures for the cargo tests:
//   event-lab-reference.json   getTransactionsForAddress transactionDetails=full shape (tag "litesvm")
//   event-lab-signatures.json  the same 10 rows in signatures mode, no logs (tag "litesvm")
//   event-lab-refetch.json     getTransaction results keyed by signature (tag "litesvm")
//   event-lab-received.json    what a webhook endpoint saw after drops and retries (tag "mocked")
//   event-lab-expected.json    what the decoder must find in the dropped transactions
// The payer key is fixed, so the files are the same on every run and CI can fail on any drift.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { FailedTransactionMetadata, LiteSVM } from "litesvm";
import {
  AccountRole,
  address,
  appendTransactionMessageInstruction,
  createKeyPairSignerFromPrivateKeyBytes,
  createTransactionMessage,
  getAddressDecoder,
  getSignatureFromTransaction,
  lamports,
  pipe,
  setTransactionMessageFeePayerSigner,
  signTransactionMessageWithSigners,
} from "@solana/kit";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "../../..");
const PROGRAM_SO = join(ROOT, "target/deploy/event_lab.so");
const BUILT_IDL = join(ROOT, "target/idl/event_lab.json");
const OUT = join(ROOT, "fixtures");
const PROGRAM_ID = address("9LQwY9ThwtXFhpgMZ1MmRBfA5wc9YUSTUGDcmoKcKTZ6");
const DROPPED = [2, 5, 7]; // never reach the webhook
const RETRIED = [0, 9]; // delivered twice
const EXTRA = "EXTRAmockedSignatureNotInTheReferenceSet1111111111111111111111111";

const sha8 = (text) => createHash("sha256").update(text).digest().subarray(0, 8);

function ixData(id, amount, memo) {
  const body = Buffer.alloc(16);
  body.writeBigUInt64LE(BigInt(id), 0);
  body.writeBigUInt64LE(BigInt(amount), 8);
  const m = Buffer.from(memo, "utf8");
  const len = Buffer.alloc(4);
  len.writeUInt32LE(m.length);
  return new Uint8Array(Buffer.concat([sha8("global:record_transfer"), body, len, m]));
}

// A second decoder, written separately from the Rust one, so the two check each other.
function decodeTransferRecorded(base64) {
  const raw = Buffer.from(base64, "base64");
  assert.deepEqual(raw.subarray(0, 8), sha8("event:TransferRecorded"), "event discriminator");
  const id = raw.readBigUInt64LE(8);
  const amount = raw.readBigUInt64LE(16);
  const who = getAddressDecoder().decode(raw.subarray(24, 56));
  const n = raw.readUInt32LE(56);
  const memo = raw.subarray(60, 60 + n).toString("utf8");
  assert.equal(raw.length, 60 + n, "no trailing bytes");
  return { id: Number(id), amount: Number(amount), who, memo };
}

test("the built IDL still matches the committed fixture", () => {
  const built = JSON.parse(readFileSync(BUILT_IDL, "utf8"));
  const committed = JSON.parse(readFileSync(join(OUT, "event_lab.idl.json"), "utf8"));
  assert.deepEqual(built, committed);
  assert.deepEqual(committed.events[0].discriminator, [...sha8("event:TransferRecorded")]);
});

test("10 record_transfer txs emit events, then become fixtures for the cargo decoder", async () => {
  const svm = new LiteSVM();
  svm.addProgramFromFile(PROGRAM_ID, PROGRAM_SO);
  const payer = await createKeyPairSignerFromPrivateKeyBytes(new Uint8Array(32).fill(7));
  svm.airdrop(payer.address, lamports(1_000_000_000n));

  const rows = [];
  const results = {};
  const expected = [];
  for (let i = 0; i < 10; i++) {
    svm.expireBlockhash();
    const id = 1000 + i;
    const amount = 50 + i * 7;
    const memo = `order-${i}`;
    const tx = await pipe(
      createTransactionMessage({ version: 0 }),
      (m) => setTransactionMessageFeePayerSigner(payer, m),
      (m) => svm.setTransactionMessageLifetimeUsingLatestBlockhash(m),
      (m) =>
        appendTransactionMessageInstruction(
          {
            programAddress: PROGRAM_ID,
            accounts: [{ address: payer.address, role: AccountRole.READONLY_SIGNER, signer: payer }],
            data: ixData(id, amount, memo),
          },
          m,
        ),
      (m) => signTransactionMessageWithSigners(m),
    );
    const signature = getSignatureFromTransaction(tx);
    const result = svm.sendTransaction(tx);
    assert.ok(!(result instanceof FailedTransactionMetadata), `tx ${i} failed: ${result}`);
    const logs = result.logs();

    const dataLines = logs.filter((l) => l.startsWith("Program data: "));
    assert.equal(dataLines.length, 1, `tx ${i} emitted ${dataLines.length} events`);
    assert.equal(logs[0], `Program ${PROGRAM_ID} invoke [1]`);
    assert.equal(logs.at(-1), `Program ${PROGRAM_ID} success`);
    const event = decodeTransferRecorded(dataLines[0].slice("Program data: ".length));
    assert.deepEqual(event, { id, amount, who: payer.address, memo });

    const slot = 100 + i;
    rows.push({
      slot,
      transactionIndex: 0,
      blockTime: null,
      transaction: { signatures: [signature], message: {} },
      meta: { err: null, fee: 5000, logMessages: logs },
    });
    results[signature] = {
      slot,
      blockTime: null,
      transaction: { signatures: [signature], message: {} },
      meta: { err: null, fee: 5000, logMessages: logs },
    };
    expected.push({ signature, id, amount, who: payer.address, memo });
  }

  mkdirSync(OUT, { recursive: true });
  const write = (name, obj) => writeFileSync(join(OUT, name), JSON.stringify(obj, null, 2) + "\n");

  write("event-lab-reference.json", {
    tag: "litesvm",
    note: "getTransactionsForAddress transactionDetails=full shape, recorded from LiteSVM",
    address: PROGRAM_ID,
    result: { data: rows },
  });
  write("event-lab-signatures.json", {
    tag: "litesvm",
    note: "getTransactionsForAddress transactionDetails=signatures shape: no logs, so no decode",
    address: PROGRAM_ID,
    result: {
      data: rows.map((r) => ({
        signature: r.transaction.signatures[0],
        slot: r.slot,
        transactionIndex: 0,
        err: null,
        memo: null,
        blockTime: null,
        confirmationStatus: "finalized",
      })),
      paginationToken: null,
    },
  });
  write("event-lab-refetch.json", {
    tag: "litesvm",
    note: "getTransaction results keyed by signature, for replaying a refetch offline",
    responses: results,
  });

  const payloads = [];
  rows.forEach((row, i) => {
    if (DROPPED.includes(i)) return;
    const p = { signature: row.transaction.signatures[0], slot: row.slot, type: "UNKNOWN" };
    payloads.push(p);
    if (RETRIED.includes(i)) payloads.push({ ...p });
  });
  payloads.push({ signature: EXTRA, slot: 1, type: "UNKNOWN" });
  write("event-lab-received.json", {
    tag: "mocked",
    note: `dropped indexes ${DROPPED}, retried ${RETRIED}, plus one extra signature`,
    payloads,
  });

  write("event-lab-expected.json", {
    missing: DROPPED.map((i) => expected[i]),
    duplicates: RETRIED.map((i) => expected[i].signature),
    extra: [EXTRA],
  });
});
