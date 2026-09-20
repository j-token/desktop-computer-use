import test from "node:test";
import assert from "node:assert/strict";
import net from "node:net";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sendNativeRequest, MAX_FRAME_BYTES } from "../dist/transport.js";
import { DcuError } from "../dist/errors.js";

async function endpointFor(directory, name) {
  if (process.platform === "win32") return `\\\\.\\pipe\\dcu-test-${process.pid}-${name}`;
  return join(directory, `${name}.sock`);
}

async function serveOne(endpoint, handler) {
  const server = net.createServer(socket => {
    let body = "";
    socket.on("data", chunk => {
      body += chunk.toString("utf8");
      const newline = body.indexOf("\n");
      if (newline < 0) return;
      const request = JSON.parse(body.slice(0, newline));
      handler(request, socket);
    });
  });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(endpoint, resolve); });
  return server;
}

test("NDJSON transport sends one authenticated request and validates response id", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-transport-"));
  const endpoint = await endpointFor(directory, "ok");
  const server = await serveOne(endpoint, (request, socket) => {
    assert.equal(request.method, "capabilities");
    assert.equal(request.token, "t".repeat(32));
    socket.end(JSON.stringify({ id: request.id, ok: true, result: { screenshot: false } }) + "\n");
  });
  try {
    const response = await sendNativeRequest(endpoint, "capabilities", "t".repeat(32), {});
    assert.deepEqual(response.result, { screenshot: false });
  } finally {
    server.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test("doctor uses the same clean native JSON envelope without a startup banner", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-transport-"));
  const endpoint = await endpointFor(directory, "doctor");
  const server = await serveOne(endpoint, (request, socket) => {
    assert.equal(request.method, "doctor");
    socket.end(JSON.stringify({ id: request.id, ok: true, result: { platform: "test", graphicalSession: false, ready: false } }) + "\n");
  });
  try {
    const response = await sendNativeRequest(endpoint, "doctor", "t".repeat(32), {});
    assert.deepEqual(response, { id: response.id, ok: true, result: { platform: "test", graphicalSession: false, ready: false } });
  } finally {
    server.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test("oversized frames are rejected before opening a connection", async () => {
  await assert.rejects(
    () => sendNativeRequest("unused", "type-text", "t".repeat(32), { text: "x".repeat(MAX_FRAME_BYTES) }),
    error => error instanceof DcuError && error.code === "invalid_argument"
  );
});

test("UTF-8 response split across socket chunks remains valid JSON", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-transport-"));
  const endpoint = await endpointFor(directory, "utf8");
  const server = await serveOne(endpoint, (request, socket) => {
    const payload = Buffer.from(JSON.stringify({ id: request.id, ok: true, result: { title: "창 제목" } }) + "\n", "utf8");
    const split = payload.indexOf(Buffer.from("창", "utf8")) + 1;
    socket.write(payload.subarray(0, split));
    setTimeout(() => socket.end(payload.subarray(split)), 5);
  });
  try {
    const response = await sendNativeRequest(endpoint, "capabilities", "t".repeat(32), {});
    assert.equal(response.result.title, "창 제목");
  } finally {
    server.close();
    await rm(directory, { recursive: true, force: true });
  }
});

test("a connection failure after request bytes does not create a retry", async () => {
  const directory = await mkdtemp(join(tmpdir(), "dcu-transport-"));
  const endpoint = await endpointFor(directory, "once");
  let requests = 0;
  const server = await serveOne(endpoint, (_request, socket) => {
    requests += 1;
    socket.destroy();
  });
  try {
    await assert.rejects(() => sendNativeRequest(endpoint, "click", "t".repeat(32), { app: "test" }));
    assert.equal(requests, 1);
  } finally {
    server.close();
    await rm(directory, { recursive: true, force: true });
  }
});
