import assert from "node:assert/strict";
import test from "node:test";
import { HttpConfig } from "../../../src/agentvault/sdk/config/config.js";
import { VaultTransportFactory } from "../../../src/agentvault/sdk/transport/factory.js";
test("transport factory creates isolated HTTP clients", () => {
  const factory = new VaultTransportFactory();
  const config = {
    provider: "http" as const,
    http: new HttpConfig("http://127.0.0.1:65535"),
  };
  const first = factory.create(config),
    second = factory.create(config);
  assert.notStrictEqual(first, second);
  first.close();
  first.close();
  second.close();
});
