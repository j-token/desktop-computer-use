import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";

test("MCP stdio server advertises the desktop tools", async () => {
  const child = spawn(process.execPath, ["dist/cli.js", "mcp", "serve"], { cwd: process.cwd(), stdio: ["pipe", "pipe", "pipe"] });
  let output = "";
  const lines = [];
  let resolveTools;
  let rejectTools;
  const toolsPromise = new Promise((resolve, reject) => { resolveTools = resolve; rejectTools = reject; });
  const timer = setTimeout(() => rejectTools(new Error("MCP tools/list timed out")), 5000);
  child.stdout.on("data", chunk => {
    output += chunk.toString("utf8");
    while (output.includes("\n")) {
      const index = output.indexOf("\n");
      const line = output.slice(0, index);
      output = output.slice(index + 1);
      if (!line.trim()) continue;
      lines.push(JSON.parse(line));
      const latest = lines.at(-1);
      if (latest?.id === 2) resolveTools(latest);
    }
  });
  child.stderr.on("data", () => { /* SDK diagnostics are intentionally ignored in this protocol test */ });
  child.on("error", rejectTools);
  try {
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "test", version: "1" } } }) + "\n");
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized", params: {} }) + "\n");
    child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} }) + "\n");
    const response = await toolsPromise;
    clearTimeout(timer);
    const names = response.result.tools.map(tool => tool.name);
    assert.ok(names.includes("dcu_drag"));
    assert.ok(names.includes("dcu_get_app_state"));
    assert.ok(names.includes("dcu_get_full_screenshot"));
  } finally {
    clearTimeout(timer);
    child.kill();
  }
});
