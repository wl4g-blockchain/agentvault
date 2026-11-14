import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:http";
import { HttpConfig } from "../../../src/agentvault/sdk/config/config.js";
import { HttpVaultTransport } from "../../../src/agentvault/sdk/transport/http.js";
test("native HTTP preserves auth and trace and bounds responses", async (t) => {
  const server = createServer((request, response) => {
    assert.equal(request.url, "/api/v1/test");
    assert.equal(request.headers.authorization, "Bearer edge-secret");
    assert.match(request.headers.traceparent as string, /^00-/);
    response.setHeader("content-type", "application/json");
    response.end('{"ok":true}');
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => {
    server.closeAllConnections();
    server.close();
  });
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  const url = `http://127.0.0.1:${address.port}`;
  const transport = new HttpVaultTransport(new HttpConfig(url, "edge-secret"));
  assert.deepEqual(await transport.request("GET", "/api/v1/test"), {
    ok: true,
  });
  const small = new HttpVaultTransport(
    new HttpConfig(url, "edge-secret", 30000, 4),
  );
  await assert.rejects(small.request("GET", "/api/v1/test"), /exceeds/);
  transport.close();
  await assert.rejects(transport.request("GET", "/api/v1/test"), /closed/);
  small.close();
});
