import { readFile } from "node:fs/promises";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { DcuClient } from "./client.js";
import { SESSIONLESS_METHODS } from "./methods.js";
import { clearSession, readSession, writeSession } from "./runtime.js";
import type { JsonObject, JsonValue } from "./types.js";

const optionalString = z.string().optional();
const optionalNumber = z.number().finite().optional();
const optionalInteger = z.number().int().optional();
const common = {
  sessionId: optionalString,
  app: optionalString,
  windowId: optionalString,
  observationId: optionalString,
  elementIndex: optionalInteger,
  fromElementIndex: optionalInteger,
  toElementIndex: optionalInteger,
  x: optionalNumber,
  y: optionalNumber,
  fromX: optionalNumber,
  fromY: optionalNumber,
  toX: optionalNumber,
  toY: optionalNumber,
  coords: z.enum(["reduced", "full"]).optional(),
  button: z.enum(["left", "right", "middle"]).optional(),
  durationMs: optionalNumber,
  steps: optionalInteger,
  holdBeforeMs: optionalNumber,
  holdAfterMs: optionalNumber,
  direction: optionalString,
  amount: optionalNumber,
  text: optionalString,
  key: optionalString,
  value: optionalString,
  includeText: z.boolean().optional(),
  includeScreenshot: z.boolean().optional(),
  format: z.enum(["jpeg", "png"]).optional(),
  quality: optionalInteger,
  maxEdge: optionalNumber,
  observe: z.enum(["none", "screenshot", "text", "both"]).optional(),
  modifiers: optionalString,
  restoreWindow: z.boolean().optional(),
  activate: z.boolean().optional()
};

type ToolSchema = Record<string, z.ZodTypeAny>;
type ToolDefinition = {
  name: string;
  method: string;
  description: string;
  schema: ToolSchema;
};

type McpContent =
  | { type: "text"; text: string }
  | { type: "image"; data: string; mimeType: string };

const tools: ToolDefinition[] = [
  {
    name: "dcu_doctor",
    method: "doctor",
    description: "Report desktop computer-use dependencies and permissions without starting input.",
    schema: {}
  },
  {
    name: "dcu_capabilities",
    method: "capabilities",
    description: "Report supported native desktop computer-use capabilities.",
    schema: {}
  },
  {
    name: "dcu_session_start",
    method: "session.start",
    description:
      "Start a visible computer-use session with a top banner, high-contrast cursor ring, " +
      "and blue inward-fading screen-edge border; press Esc to stop it.",
    schema: {}
  },
  {
    name: "dcu_session_status",
    method: "session.status",
    description: "Read the current computer-use session status.",
    schema: { sessionId: optionalString }
  },
  {
    name: "dcu_session_stop",
    method: "session.stop",
    description: "Stop the current computer-use session and release held input.",
    schema: { sessionId: optionalString }
  },
  {
    name: "dcu_list_apps",
    method: "list-apps",
    description: "List visible desktop applications.",
    schema: { sessionId: optionalString }
  },
  {
    name: "dcu_list_windows",
    method: "list-windows",
    description: "List windows, optionally filtered by application.",
    schema: { sessionId: optionalString, app: optionalString }
  },
  {
    name: "dcu_get_app_state",
    method: "get-app-state",
    description:
      "Capture the latest target window state; a reduced screenshot (0.5x above 1280x720) is included by default.",
    schema: { ...common }
  },
  {
    name: "dcu_get_full_screenshot",
    method: "get-full-screenshot",
    description:
      "Return the full-resolution original of an observation's screenshot without recapturing. " +
      "Pass coords \"full\" when clicking a point read from this image.",
    schema: {
      sessionId: optionalString,
      app: optionalString,
      windowId: optionalString,
      observationId: z.string()
    }
  },
  {
    name: "dcu_click",
    method: "click",
    description:
      "Click a semantic element or a point read from the latest reduced screenshot " +
      "(coords \"full\" for a get-full-screenshot image).",
    schema: { ...common }
  },
  {
    name: "dcu_drag",
    method: "drag",
    description: "Perform a paced native drag with button-down, interpolated moves, and button-up.",
    schema: { ...common }
  },
  {
    name: "dcu_scroll",
    method: "scroll",
    description: "Scroll at a point read from the latest reduced screenshot (coords \"full\" for a full image).",
    schema: { ...common }
  },
  {
    name: "dcu_type_text",
    method: "type-text",
    description: "Type text into the currently focused target.",
    schema: { ...common }
  },
  {
    name: "dcu_press_key",
    method: "press-key",
    description: "Press one key in the target window.",
    schema: { ...common }
  },
  {
    name: "dcu_hotkey",
    method: "hotkey",
    description: "Press a modifier chord in the target window.",
    schema: { ...common }
  },
  {
    name: "dcu_set_value",
    method: "set-value",
    description: "Set an accessibility element value and optionally observe it.",
    schema: { ...common }
  },
  {
    name: "dcu_paste_text",
    method: "paste-text",
    description: "Paste text into the focused target.",
    schema: { ...common }
  }
];

function asJsonObject(value: Record<string, unknown>): JsonObject {
  const result: JsonObject = {};
  for (const [key, item] of Object.entries(value)) {
    if (item !== undefined) result[key] = item as JsonValue;
  }
  return result;
}

function jsonText(value: unknown): string {
  return JSON.stringify(value, null, 2);
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function findScreenshot(value: unknown): Record<string, unknown> | undefined {
  const record = asRecord(value);
  const directScreenshot = asRecord(record?.screenshot);
  if (directScreenshot) return directScreenshot;

  const observation = asRecord(record?.observation);
  return asRecord(observation?.screenshot);
}

async function readScreenshot(screenshot: Record<string, unknown>): Promise<{ data?: string; error?: string }> {
  if (typeof screenshot.data === "string") return { data: screenshot.data };
  if (typeof screenshot.path !== "string") return {};

  try {
    const bytes = await readFile(screenshot.path);
    return { data: bytes.toString("base64") };
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  }
}

export async function resultContent(value: unknown): Promise<McpContent[]> {
  const content: McpContent[] = [{ type: "text", text: jsonText(value) }];
  const screenshot = findScreenshot(value);
  if (!screenshot) return content;

  const loadedScreenshot = await readScreenshot(screenshot);
  if (loadedScreenshot.data) {
    content.push({
      type: "image",
      data: loadedScreenshot.data,
      mimeType: typeof screenshot.mimeType === "string" ? screenshot.mimeType : "image/jpeg"
    });
  }
  if (loadedScreenshot.error) {
    content.push({ type: "text", text: `screenshot_read_error: ${loadedScreenshot.error}` });
  }
  return content;
}

async function invoke(client: DcuClient, method: string, input: Record<string, unknown>): Promise<unknown> {
  const params = asJsonObject(input);
  if (!SESSIONLESS_METHODS.has(method) && params.sessionId === undefined) {
    const current = await readSession(client.paths);
    if (current) params.sessionId = current.sessionId;
  }
  const response = await client.request(method, params);
  if (response.ok) {
    const value = response.result;
    const result = asRecord(value);
    if (method === "session.start" && typeof result?.sessionId === "string") {
      await writeSession(result.sessionId, client.paths);
    }
    if (method === "session.stop") {
      await clearSession(client.paths);
    }
    return value;
  }
  throw new Error(`${response.error?.code ?? "native_error"}: ${response.error?.message ?? "Native request failed"}`);
}

export async function runMcpServer(client = new DcuClient()): Promise<void> {
  const server = new McpServer({ name: "desktop-computer-use", version: "0.1.0" });
  for (const definition of tools) {
    server.registerTool(
      definition.name,
      { description: definition.description, inputSchema: definition.schema },
      async (input: Record<string, unknown>) => {
        try {
          const value = await invoke(client, definition.method, input);
          return { content: await resultContent(value) };
        } catch (error) {
          return {
            isError: true,
            content: [{ type: "text" as const, text: error instanceof Error ? error.message : String(error) }]
          };
        }
      }
    );
  }
  const transport = new StdioServerTransport();
  await server.connect(transport);
  await new Promise<void>(resolve => {
    process.stdin.once("end", resolve);
    process.stdin.once("close", resolve);
  });
}
