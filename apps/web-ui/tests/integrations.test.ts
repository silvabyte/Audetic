import { test } from "node:test";
import assert from "node:assert/strict";
import createClient from "openapi-fetch";
import type { paths } from "../src/api/schema";
import { IntegrationsStore } from "../src/stores/integrations-store";

function clientWith(handler: (request: Request) => Response | Promise<Response>) {
  return createClient<paths>({
    baseUrl: "http://audetic.test/api",
    fetch: async (request) => handler(request),
  });
}

test("one-time ingress secrets are not retained in the key list", async () => {
  let requests = 0;
  const store = new IntegrationsStore(clientWith(() => {
    requests += 1;
    return Response.json({
      id: "3e327427-d07e-4824-9b40-ded8cb893f34",
      name: "Index ring",
      scope: "index",
      created_at: "2026-09-30 12:00:00",
      last_used_at: null,
      revoked_at: null,
      secret: "audetic_ingress_one-time-secret",
    }, { status: 201 });
  }));

  assert.equal(await store.createKey("Index ring", "index"), true);
  assert.equal(store.issuedKey?.secret, "audetic_ingress_one-time-secret");
  assert.equal("secret" in store.keys[0]!, false);
  assert.equal(await store.createKey("Second", "index"), false);
  assert.equal(requests, 1);

  store.clearIssuedKey();
  assert.equal(store.issuedKey, null);
  assert.equal(await store.createKey("Second", "index"), true);
  assert.equal(requests, 2);
});

test("failed integration loads do not claim the store is loaded", async () => {
  const store = new IntegrationsStore(clientWith(() =>
    Response.json({ error: true, message: "Integration database unavailable" }, { status: 500 }),
  ));

  await store.load();
  assert.equal(store.state, "error");
  assert.match(store.error ?? "", /Integration database unavailable/);
});
