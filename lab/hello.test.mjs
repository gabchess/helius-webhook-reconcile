// Chunk 0 hello test. Loads the built stub program into LiteSVM and checks it is executable.
import { test } from "node:test";
import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { LiteSVM } from "litesvm";
import { address } from "@solana/kit";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const PROGRAM_SO = join(ROOT, "target/deploy/event_lab.so");
const PROGRAM_ID = address("9LQwY9ThwtXFhpgMZ1MmRBfA5wc9YUSTUGDcmoKcKTZ6");

test("the built event_lab stub loads into LiteSVM as an executable program", () => {
  const svm = new LiteSVM();
  svm.addProgramFromFile(PROGRAM_ID, PROGRAM_SO);
  const account = svm.getAccount(PROGRAM_ID);
  assert.ok(account.exists);
  assert.equal(account.executable, true);
});
