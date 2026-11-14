import assert from "node:assert/strict";
import { inspect } from "node:util";
import test from "node:test";
import { HttpConfig } from "../../../src/agentvault/sdk/config/config.js";
test("HTTP configuration validates URLs and redacts bearer secrets", () => {
  assert.throws(() => new HttpConfig("file:///tmp/vault"));
  assert.throws(() => new HttpConfig("https://user:secret@vault.example"));
  assert.doesNotMatch(
    inspect(new HttpConfig("https://vault.example", "edge-secret")),
    /edge-secret/,
  );
});
