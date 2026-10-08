import { createServer } from "node:http";
import { once } from "node:events";
import { describe, expect, it } from "vitest";
import { runCrewSmoke } from "../tools/crew-smoke.js";
import { createHarness } from "./helpers/harness.js";

describe("crew smoke command", () => {
  it("runs two signed clients over HTTP through the real router and SQLite migrations", async () => {
    const h = createHarness();
    h.clock.now = Date.now();
    const server = createServer(async (incoming, outgoing) => {
      try {
        const chunks: Buffer[] = [];
        for await (const chunk of incoming) chunks.push(Buffer.from(chunk));
        const text = Buffer.concat(chunks).toString("utf8");
        const headers = new Headers();
        for (const [key, value] of Object.entries(incoming.headers)) {
          if (value !== undefined) headers.set(key, Array.isArray(value) ? value.join(", ") : value);
        }
        const request = new Request(`http://127.0.0.1${incoming.url}`, {
          method: incoming.method, headers, ...(text ? { body: text } : {}),
        });
        const response = await h.send(request);
        outgoing.writeHead(response.status, Object.fromEntries(response.headers));
        outgoing.end(await response.text());
      } catch {
        outgoing.writeHead(500);
        outgoing.end();
      }
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    try {
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("expected TCP address");
      const result = await runCrewSmoke(`http://127.0.0.1:${address.port}`);
      expect(result).toMatchObject({ ok: true, cleanup: "idle" });
      expect(result.checks.map((check) => check.step)).toEqual([
        "health", "register_a", "register_b", "create_crew", "create_invite", "join_crew", "members",
        "heartbeat_a", "heartbeat_b", "snapshot_a", "snapshot_b", "idle_a", "idle_b",
      ]);
      expect(h.d1.query("SELECT display_name FROM devices ORDER BY display_name")).toEqual([
        { display_name: "DuoClip teste A" }, { display_name: "DuoClip teste B" },
      ]);
      expect(h.d1.query("SELECT game, active_crew, seq FROM device_presence")).toEqual([
        { game: null, active_crew: null, seq: 2 }, { game: null, active_crew: null, seq: 2 },
      ]);
      expect(h.d1.query("SELECT * FROM crew_members")).toHaveLength(2);
      expect(h.d1.query("SELECT uses_left FROM invites")).toEqual([{ uses_left: 4 }]);
      expect(h.d1.query("SELECT * FROM clips")).toHaveLength(0);
    } finally {
      await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
      h.d1.database.close();
    }
  });

  it("stops at a failed request without printing the response or pretending cleanup succeeded", async () => {
    const requests: string[] = [];
    const result = await runCrewSmoke("https://worker.test", async (url) => {
      requests.push(new URL(url).pathname);
      return new Response(JSON.stringify(requests.length === 1 ? { ok: true } : { message: "secret-response" }), {
        status: requests.length === 1 ? 200 : 429,
      });
    });
    expect(requests).toEqual(["/v1/health", "/v1/devices"]);
    expect(result).toMatchObject({ ok: false, cleanup: "not_completed", failure: { step: "register_a", code: "http_429" } });
    expect(JSON.stringify(result)).not.toContain("secret-response");
  });

  it("does not report success if a healthy endpoint returns a missing participant", async () => {
    const h = createHarness();
    try {
      const result = await runCrewSmoke("https://worker.test", async (url, init) => {
        const response = await h.send(new Request(url, init));
        if (new URL(url).pathname.endsWith("/members")) return new Response("[]");
        return response;
      }, () => h.clock.now);
      expect(result).toMatchObject({ ok: false, failure: { step: "members", code: "members_mismatch" } });
    } finally {
      h.d1.database.close();
    }
  });
});
