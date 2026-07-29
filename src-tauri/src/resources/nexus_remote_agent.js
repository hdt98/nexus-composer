#!/usr/bin/env node
"use strict";

const http = require("http");
const https = require("https");
const crypto = require("crypto");
const childProcess = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

const VERSION = "0.1.9";
const HOME = os.homedir();
const ROOT = path.join(HOME, ".nexus-composer", "remote-agent");
const CONFIG_PATH = path.join(ROOT, "agent-config.json");
const PID_PATH = path.join(ROOT, "agent.pid");
const HISTORY_MAINTAINER_PATH = path.join(
  ROOT,
  "nexus-remote-history-maintainer.py",
);
const ACTIVE_CODEX_BACKUP_PATH = path.join(ROOT, "active-codex-backup.json");
const CODEX_DIR = path.join(HOME, ".codex");
const CODEX_CONFIG_PATH = path.join(CODEX_DIR, "config.toml");
const CODEX_AUTH_PATH = path.join(CODEX_DIR, "auth.json");
const CODEX_MODEL_CATALOG_PATH = path.join(
  CODEX_DIR,
  "nexus-remote-agent-model-catalog.json",
);

let startedAt = new Date();
let requestsTotal = 0;
let requestsFailed = 0;
let lastRequestAt = null;
let currentConfig = readConfig();
let proxyServer = null;
let controlServer = null;
let stopping = false;
const responseChatHistory = new Map();
const MAX_RESPONSE_HISTORY = 64;

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
}

function readJson(file, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (_) {
    return fallback;
  }
}

function writeJson(file, value) {
  writeFileAtomic(file, JSON.stringify(value, null, 2), 0o600);
}

function writeFileAtomic(file, content, mode) {
  const directory = path.dirname(file);
  ensureDir(directory);
  const temporary = path.join(
    directory,
    `.${path.basename(file)}.${process.pid}.${crypto.randomBytes(6).toString("hex")}.tmp`,
  );
  try {
    fs.writeFileSync(temporary, content, { mode, flag: "wx" });
    fs.renameSync(temporary, file);
    fs.chmodSync(file, mode);
  } finally {
    removeFileIfPresent(temporary);
  }
}

function readConfig() {
  const config = readJson(CONFIG_PATH, {});
  return {
    proxyPort: Number(config.proxyPort || 18080),
    controlPort: Number(config.controlPort || 18090),
    upstreamUrl: String(config.upstreamUrl || "").replace(/\/+$/, ""),
    apiKey: String(config.apiKey || ""),
    model: String(config.model || "GLM-5.2-SGLang"),
    displayName: String(config.displayName || "Nexus Agent Proxy"),
    modelCatalog:
      config.modelCatalog &&
      typeof config.modelCatalog === "object" &&
      !Array.isArray(config.modelCatalog)
        ? config.modelCatalog
        : null,
    reasoningEffort: String(config.reasoningEffort || "high"),
    maxTokens: Number(config.maxTokens || 16384),
  };
}

function chatCompletionsUrl(configuredUrl) {
  const normalized = String(configuredUrl || "").replace(/\/+$/, "");
  if (/\/chat\/completions$/i.test(normalized)) return new URL(normalized);
  return new URL(`${normalized}/chat/completions`);
}

function isAgentManagedCodexConfig() {
  try {
    return /^\s*model_provider\s*=\s*"nexus-agent"\s*$/m.test(
      fs.readFileSync(CODEX_CONFIG_PATH, "utf8"),
    );
  } catch (_) {
    return false;
  }
}

function readActiveCodexBackup() {
  const backup = readJson(ACTIVE_CODEX_BACKUP_PATH, null);
  if (!backup || typeof backup.backupDir !== "string" || !backup.files)
    return null;
  return backup;
}

function removeFileIfPresent(file) {
  try {
    fs.unlinkSync(file);
  } catch (_) {}
}

function preserveCodexProfile() {
  const activeBackup = readActiveCodexBackup();
  if (isAgentManagedCodexConfig()) return activeBackup;

  // A stale marker means a previous agent was already stopped or replaced.
  if (activeBackup) removeFileIfPresent(ACTIVE_CODEX_BACKUP_PATH);

  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const backupDir = path.join(ROOT, "backups", stamp);
  ensureDir(backupDir);
  const files = {};
  for (const [key, source, name] of [
    ["config", CODEX_CONFIG_PATH, "config.toml"],
    ["auth", CODEX_AUTH_PATH, "auth.json"],
  ]) {
    const existed = fs.existsSync(source);
    files[key] = { existed, path: path.join(backupDir, name) };
    if (existed) fs.copyFileSync(source, files[key].path);
  }
  const backup = {
    version: 1,
    createdAt: new Date().toISOString(),
    backupDir,
    files,
  };
  writeJson(ACTIVE_CODEX_BACKUP_PATH, backup);
  return backup;
}

function restoreCodexProfile() {
  const backup = readActiveCodexBackup();
  if (!backup) return false;
  ensureDir(CODEX_DIR);
  const restoreFile = (key, target) => {
    const file = backup.files[key];
    if (file && file.existed && fs.existsSync(file.path)) {
      writeFileAtomic(target, fs.readFileSync(file.path), 0o600);
    } else {
      removeFileIfPresent(target);
    }
  };

  // Restore auth and remove the managed catalog before config.toml. The
  // routing config is the commit point: if the process exits earlier, it still
  // looks agent-managed and the active backup marker remains retryable.
  restoreFile("auth", CODEX_AUTH_PATH);
  removeFileIfPresent(CODEX_MODEL_CATALOG_PATH);
  restoreFile("config", CODEX_CONFIG_PATH);
  removeFileIfPresent(ACTIVE_CODEX_BACKUP_PATH);
  return true;
}

function originalCodexConfigText() {
  const backup = readActiveCodexBackup();
  const config = backup && backup.files && backup.files.config;
  if (!config || !config.existed || !config.path) return "";
  try {
    return fs.readFileSync(config.path, "utf8");
  } catch (_) {
    return "";
  }
}

function stripAgentManagedToml(text) {
  const managedTopLevelKeys = new Set([
    "model_provider",
    "model",
    "model_catalog_json",
    "model_reasoning_effort",
    "disable_response_storage",
  ]);
  const output = [];
  let section = "";
  let skipAgentProvider = false;
  for (const line of String(text || "").split(/\r?\n/)) {
    const sectionMatch = line.match(/^\s*\[([^\]]+)\]\s*(?:#.*)?$/);
    if (sectionMatch) {
      section = sectionMatch[1].trim();
      skipAgentProvider =
        /^model_providers\.(?:nexus-agent|"nexus-agent"|'nexus-agent')(?:\.|$)/.test(
          section,
        );
      if (skipAgentProvider) continue;
    } else if (skipAgentProvider) {
      continue;
    }

    if (!section) {
      const keyMatch = line.match(/^\s*([A-Za-z0-9_-]+)\s*=/);
      if (keyMatch && managedTopLevelKeys.has(keyMatch[1])) continue;
    }
    output.push(line);
  }
  return output.join("\n").trim();
}

function ensureLegacyLandlockFeature(text) {
  const lines = String(text || "").split(/\r?\n/);
  const output = [];
  let inFeatures = false;
  let foundFeatures = false;
  let inserted = false;
  for (const line of lines) {
    const sectionMatch = line.match(/^\s*\[([^\]]+)\]\s*(?:#.*)?$/);
    if (sectionMatch) {
      if (inFeatures && !inserted) {
        output.push("use_legacy_landlock = true");
        inserted = true;
      }
      inFeatures = sectionMatch[1].trim() === "features";
      foundFeatures = foundFeatures || inFeatures;
      output.push(line);
      continue;
    }
    if (inFeatures && /^\s*use_legacy_landlock\s*=/.test(line)) {
      if (!inserted) {
        output.push("use_legacy_landlock = true");
        inserted = true;
      }
      continue;
    }
    output.push(line);
  }
  if (inFeatures && !inserted) {
    output.push("use_legacy_landlock = true");
    inserted = true;
  }
  if (!foundFeatures) {
    if (output.length && output[output.length - 1] !== "") output.push("");
    output.push("[features]", "use_legacy_landlock = true");
  }
  return output.join("\n").trim();
}

function writeCodexConfig(config) {
  ensureDir(CODEX_DIR);
  const hasModelCatalog =
    config.modelCatalog &&
    Array.isArray(config.modelCatalog.models) &&
    config.modelCatalog.models.length > 0;
  if (hasModelCatalog) {
    writeJson(CODEX_MODEL_CATALOG_PATH, config.modelCatalog);
  } else {
    removeFileIfPresent(CODEX_MODEL_CATALOG_PATH);
  }
  const managedTopLevel = [
    'model_provider = "nexus-agent"',
    `model = ${JSON.stringify(config.model)}`,
    ...(hasModelCatalog
      ? [`model_catalog_json = ${JSON.stringify(CODEX_MODEL_CATALOG_PATH)}`]
      : []),
    `model_reasoning_effort = ${JSON.stringify(config.reasoningEffort)}`,
    "disable_response_storage = true",
  ].join("\n");
  const agentProviderTable = [
    "[model_providers.nexus-agent]",
    `name = ${JSON.stringify(config.displayName)}`,
    `base_url = ${JSON.stringify(`http://127.0.0.1:${config.proxyPort}/v1`)}`,
    'wire_api = "responses"',
    "requires_openai_auth = false",
  ].join("\n");
  const preserved = ensureLegacyLandlockFeature(
    stripAgentManagedToml(originalCodexConfigText()),
  );
  const preservedLines = preserved.split(/\r?\n/);
  const firstTable = preservedLines.findIndex((line) => /^\s*\[/.test(line));
  const preservedTopLevel =
    firstTable === -1
      ? preserved
      : preservedLines.slice(0, firstTable).join("\n");
  const preservedTables =
    firstTable === -1 ? "" : preservedLines.slice(firstTable).join("\n");
  const text = [
    managedTopLevel,
    preservedTopLevel.trim(),
    agentProviderTable,
    preservedTables.trim(),
  ]
    .filter(Boolean)
    .join("\n\n")
    .concat("\n");
  writeFileAtomic(CODEX_CONFIG_PATH, text, 0o600);
  // The loopback provider declares requires_openai_auth=false, so an existing
  // OAuth/API login can remain untouched. Only create a dummy shape when the
  // remote did not have auth.json at all; restore removes it later.
  if (!fs.existsSync(CODEX_AUTH_PATH)) {
    writeJson(CODEX_AUTH_PATH, { OPENAI_API_KEY: "nexus-agent-local" });
  }
}

function sendJson(res, status, value) {
  const body = JSON.stringify(value);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
  });
  res.end(body);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

function responseItemContent(item) {
  return Array.isArray(item.content)
    ? item.content
        .map((part) => {
          if (typeof part === "string") return part;
          if (part && typeof part === "object")
            return part.text || part.content || "";
          return "";
        })
        .filter(Boolean)
        .join("\n")
    : item.content || item.text || "";
}

function chatNameForResponseTool(item, toolContext) {
  const name = String(item.name || "").trim();
  if (item.type === "tool_search_call") return "tool_search";
  if (item.namespace)
    return flattenToolName(String(item.namespace).trim(), name);
  const match = Object.entries(toolContext || {}).find(
    ([, spec]) =>
      spec.name === name &&
      (spec.namespace || "") === String(item.namespace || ""),
  );
  return match ? match[0] : name;
}

function canonicalToolArguments(value) {
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed || "{}";
  }
  if (value === undefined || value === null) return "{}";
  return JSON.stringify(value);
}

function customToolInputFromArguments(argumentsText) {
  if (!argumentsText) return "";
  try {
    const parsed = JSON.parse(argumentsText);
    if (
      parsed &&
      typeof parsed === "object" &&
      !Array.isArray(parsed) &&
      Object.prototype.hasOwnProperty.call(parsed, "input")
    ) {
      return typeof parsed.input === "string"
        ? parsed.input
        : JSON.stringify(parsed.input);
    }
  } catch (_) {}
  return argumentsText;
}

function rememberChatResponse(responseId, requestMessages, responseMessage) {
  if (!responseId) return;
  const messages = [...requestMessages, responseMessage].map((message) =>
    JSON.parse(JSON.stringify(message)),
  );
  responseChatHistory.delete(responseId);
  responseChatHistory.set(responseId, messages);
  while (responseChatHistory.size > MAX_RESPONSE_HISTORY) {
    responseChatHistory.delete(responseChatHistory.keys().next().value);
  }
}

function applyPreviousResponseHistory(body, messages) {
  const previousId = String(body.previous_response_id || "");
  const previous = responseChatHistory.get(previousId);
  if (!previous) return messages;
  return [
    ...previous.map((message) => JSON.parse(JSON.stringify(message))),
    // Codex repeats the instructions on continuation requests. They already
    // exist at the start of the cached chat history.
    ...messages.filter((message) => message.role !== "system"),
  ];
}

function responseToolCallToChatToolCall(item, toolContext) {
  const callId = String(item.call_id || item.id || "");
  const name = chatNameForResponseTool(item, toolContext);
  let argumentsText;
  if (item.type === "custom_tool_call") {
    argumentsText = JSON.stringify({ input: item.input || "" });
  } else {
    argumentsText = canonicalToolArguments(item.arguments);
  }
  return {
    id: callId,
    type: "function",
    function: { name, arguments: argumentsText },
  };
}

function responseInputToMessages(input, instructions, toolContext) {
  const messages = [];
  if (instructions)
    messages.push({ role: "system", content: String(instructions) });
  if (typeof input === "string") {
    messages.push({ role: "user", content: input });
  } else if (Array.isArray(input)) {
    let pendingToolCalls = [];
    const flushToolCalls = () => {
      if (pendingToolCalls.length === 0) return;
      messages.push({
        role: "assistant",
        content: null,
        tool_calls: pendingToolCalls,
      });
      pendingToolCalls = [];
    };
    for (const item of input) {
      if (!item || typeof item !== "object") continue;
      if (
        item.type === "function_call" ||
        item.type === "custom_tool_call" ||
        item.type === "tool_search_call"
      ) {
        pendingToolCalls.push(
          responseToolCallToChatToolCall(item, toolContext),
        );
        continue;
      }
      if (
        item.type === "function_call_output" ||
        item.type === "custom_tool_call_output" ||
        item.type === "tool_search_output"
      ) {
        flushToolCalls();
        const output =
          typeof item.output === "string"
            ? item.output
            : item.output === undefined
              ? JSON.stringify(item)
              : JSON.stringify(item.output);
        messages.push({
          role: "tool",
          tool_call_id: String(item.call_id || item.id || ""),
          content: output,
        });
        continue;
      }
      flushToolCalls();
      const role = item.role || "user";
      const content = responseItemContent(item);
      messages.push({ role, content: String(content) });
    }
    flushToolCalls();
  }
  if (messages.length === 0) messages.push({ role: "user", content: "" });
  return messages;
}

function shortHash(value) {
  return crypto.createHash("sha256").update(value).digest("hex").slice(0, 12);
}

function flattenToolName(namespace, name) {
  const fullName = `${namespace}__${name}`;
  if (fullName.length <= 64) return fullName;
  const suffix = `__${shortHash(fullName)}`;
  return `${fullName.slice(0, 64 - suffix.length)}${suffix}`;
}

function responseToolName(tool) {
  if (!tool || typeof tool !== "object") return "";
  const nested =
    tool.function && typeof tool.function === "object"
      ? tool.function.name
      : "";
  return String(nested || tool.name || "").trim();
}

function responseFunctionToolToChatTool(tool, namespace) {
  const name = responseToolName(tool);
  if (!name) return null;
  const chatName = namespace ? flattenToolName(namespace, name) : name;
  const sourceFunction =
    tool.function && typeof tool.function === "object" ? tool.function : tool;
  const fn = {
    name: chatName,
    description: sourceFunction.description || "",
    parameters: sourceFunction.parameters || { type: "object", properties: {} },
  };
  if (sourceFunction.strict !== undefined) fn.strict = sourceFunction.strict;
  if (tool.strict !== undefined && fn.strict === undefined)
    fn.strict = tool.strict;
  return { type: "function", function: fn };
}

function responseCustomToolToChatTool(tool) {
  const name = responseToolName(tool);
  if (!name) return null;
  return {
    type: "function",
    function: {
      name,
      description: tool.description || `Custom Codex tool ${name}`,
      parameters: {
        type: "object",
        properties: {
          input: { type: "string" },
        },
        required: ["input"],
      },
    },
  };
}

function responseToolSearchToChatTool() {
  return {
    type: "function",
    function: {
      name: "tool_search",
      description:
        "Search and load Codex tools, plugins, connectors, and MCP namespaces.",
      parameters: {
        type: "object",
        properties: {
          query: { type: "string" },
          limit: { type: "integer" },
        },
        required: ["query"],
      },
    },
  };
}

function responsesToolsToChatTools(tools) {
  if (!Array.isArray(tools)) return { tools: [], toolContext: {} };
  const seen = new Set();
  const chatTools = [];
  const toolContext = {};
  const add = (tool, spec) => {
    if (!tool || !tool.function || !tool.function.name) return;
    if (seen.has(tool.function.name)) return;
    seen.add(tool.function.name);
    chatTools.push(tool);
    toolContext[tool.function.name] = spec;
  };

  for (const tool of tools) {
    if (typeof tool === "string") {
      add(responseCustomToolToChatTool({ type: "custom", name: tool }), {
        kind: "custom",
        name: tool,
      });
      continue;
    }
    if (!tool || typeof tool !== "object") continue;
    const type = tool.type;
    if (type === "function") {
      add(responseFunctionToolToChatTool(tool), {
        kind: "function",
        name: responseToolName(tool),
      });
    } else if (type === "custom") {
      add(responseCustomToolToChatTool(tool), {
        kind: "custom",
        name: responseToolName(tool),
      });
    } else if (type === "tool_search") {
      add(responseToolSearchToChatTool(), {
        kind: "tool_search",
        name: "tool_search",
      });
    } else if (type === "namespace") {
      const namespace = String(tool.name || "").trim();
      const children = Array.isArray(tool.tools)
        ? tool.tools
        : Array.isArray(tool.children)
          ? tool.children
          : [];
      for (const child of children) {
        if (child && child.type === "function") {
          add(responseFunctionToolToChatTool(child, namespace), {
            kind: "function",
            name: responseToolName(child),
            namespace,
          });
        }
      }
    }
  }

  return { tools: chatTools, toolContext };
}

function responseToolChoiceToChat(toolChoice, hasTools) {
  if (!hasTools) return undefined;
  if (typeof toolChoice === "string") return toolChoice;
  if (!toolChoice || typeof toolChoice !== "object") return toolChoice;
  const type = toolChoice.type;
  if (type === "auto" || type === "none") return type;
  if (type === "required" || type === "any") return "required";
  if (type === "function" || type === "custom") {
    const name = responseToolName(toolChoice);
    if (!name) return undefined;
    const chatName = toolChoice.namespace
      ? flattenToolName(String(toolChoice.namespace), name)
      : name;
    return { type: "function", function: { name: chatName } };
  }
  if (type === "tool_search") {
    return { type: "function", function: { name: "tool_search" } };
  }
  return undefined;
}

function responsesReasoningEffortToChat(effort) {
  switch (
    String(effort || "")
      .trim()
      .toLowerCase()
  ) {
    // Codex calls its highest effort level `xhigh`; SGLang's OpenAI-compatible
    // Chat Completions endpoint calls the equivalent level `max`.
    case "xhigh":
      return "max";
    // Some Codex clients use `minimal`; SGLang has no equivalent lower level.
    case "minimal":
      return "low";
    case "none":
    case "low":
    case "medium":
    case "high":
    case "max":
      return String(effort).trim().toLowerCase();
    default:
      return undefined;
  }
}

function responsesToChat(body, config) {
  const convertedTools = responsesToolsToChatTools(body.tools);
  const chat = {
    model: config.model || body.model,
    messages: applyPreviousResponseHistory(
      body,
      responseInputToMessages(
        body.input,
        body.instructions,
        convertedTools.toolContext,
      ),
    ),
    stream: Boolean(body.stream),
  };
  if (body.temperature !== undefined) chat.temperature = body.temperature;
  if (body.top_p !== undefined) chat.top_p = body.top_p;
  if (body.max_output_tokens !== undefined)
    chat.max_tokens = body.max_output_tokens;
  const tools = convertedTools.tools;
  if (tools.length > 0) {
    chat.tools = tools;
  }
  if (body.tool_choice !== undefined) {
    const toolChoice = responseToolChoiceToChat(
      body.tool_choice,
      tools.length > 0,
    );
    if (toolChoice !== undefined) chat.tool_choice = toolChoice;
  }
  const reasoningEffort = responsesReasoningEffortToChat(
    body.reasoning && body.reasoning.effort,
  );
  if (reasoningEffort !== undefined) chat.reasoning_effort = reasoningEffort;
  return { chat, toolContext: convertedTools.toolContext };
}

function chatUsageToResponses(usage) {
  if (!usage) return undefined;
  return {
    input_tokens: usage.prompt_tokens || 0,
    output_tokens: usage.completion_tokens || 0,
    total_tokens: usage.total_tokens || 0,
  };
}

function responseItemFromChatToolCall(toolCall, index, toolContext, status) {
  const callId = String(toolCall.id || `call_${index}`);
  const fn = toolCall.function || {};
  const chatName = String(fn.name || "");
  const spec = toolContext[chatName];
  const argumentsText = canonicalToolArguments(fn.arguments);
  if (spec && spec.kind === "custom") {
    return {
      id: `ctc_${callId}`,
      type: "custom_tool_call",
      status,
      call_id: callId,
      name: spec.name,
      input: customToolInputFromArguments(argumentsText),
    };
  }
  if (spec && spec.kind === "tool_search") {
    let argumentsObject = {};
    try {
      argumentsObject = JSON.parse(argumentsText);
    } catch (_) {
      argumentsObject = { query: argumentsText };
    }
    return {
      id: `ts_${callId}`,
      type: "tool_search_call",
      status,
      call_id: callId,
      execution: "client",
      arguments: argumentsObject,
    };
  }
  const item = {
    id: `fc_${callId}`,
    type: "function_call",
    status,
    call_id: callId,
    name: spec ? spec.name : chatName,
    arguments: argumentsText,
  };
  if (spec && spec.namespace) item.namespace = spec.namespace;
  return item;
}

function responseOutputFromChatMessage(message, responseId, toolContext) {
  const output = [];
  const text = typeof message.content === "string" ? message.content : "";
  if (text) {
    output.push({
      id: `msg_${responseId}`,
      type: "message",
      role: "assistant",
      status: "completed",
      content: [{ type: "output_text", text, annotations: [] }],
    });
  }
  const toolCalls = Array.isArray(message.tool_calls)
    ? message.tool_calls
    : message.function_call
      ? [{ id: message.function_call.id, function: message.function_call }]
      : [];
  toolCalls.forEach((toolCall, index) => {
    if (toolCall && toolCall.function && toolCall.function.name) {
      output.push(
        responseItemFromChatToolCall(toolCall, index, toolContext, "completed"),
      );
    }
  });
  return output;
}

function chatToResponse(chat, model, toolContext) {
  const choice = Array.isArray(chat.choices) ? chat.choices[0] : undefined;
  const message = choice && choice.message ? choice.message : {};
  const responseId = String(chat.id || `resp_${Date.now()}`);
  return {
    id: responseId,
    object: "response",
    created_at: Math.floor(Date.now() / 1000),
    status: "completed",
    model,
    output: responseOutputFromChatMessage(message, responseId, toolContext),
    usage: chatUsageToResponses(chat.usage),
  };
}

function sseEvent(name, payload) {
  return `event: ${name}\ndata: ${JSON.stringify(payload)}\n\n`;
}

function baseStreamResponse(id, model, status, output) {
  return {
    id,
    object: "response",
    created_at: Math.floor(Date.now() / 1000),
    status,
    model,
    output: Array.isArray(output) ? output : [],
  };
}

function responseStreamStart(res, id, model) {
  res.write(
    sseEvent("response.created", {
      type: "response.created",
      response: baseStreamResponse(id, model, "in_progress", []),
    }),
  );
  res.write(
    sseEvent("response.in_progress", {
      type: "response.in_progress",
      response: baseStreamResponse(id, model, "in_progress", []),
    }),
  );
}

function responseStreamText(res, id, text, state) {
  if (!text) return;
  if (state && !state.textStarted) {
    state.textStarted = true;
    res.write(
      sseEvent("response.output_item.added", {
        type: "response.output_item.added",
        output_index: 0,
        item: {
          id: `msg_${id}`,
          type: "message",
          status: "in_progress",
          role: "assistant",
          content: [],
        },
      }),
    );
    res.write(
      sseEvent("response.content_part.added", {
        type: "response.content_part.added",
        item_id: `msg_${id}`,
        output_index: 0,
        content_index: 0,
        part: {
          type: "output_text",
          text: "",
          annotations: [],
        },
      }),
    );
  }
  res.write(
    sseEvent("response.output_text.delta", {
      type: "response.output_text.delta",
      item_id: `msg_${id}`,
      output_index: 0,
      content_index: 0,
      delta: text,
    }),
  );
}

function responseStreamReasoning(res, text) {
  if (text)
    res.write(
      sseEvent("response.reasoning_summary_text.delta", {
        type: "response.reasoning_summary_text.delta",
        delta: text,
      }),
    );
}

function responseStreamDone(
  res,
  id,
  model,
  text,
  chatToolCalls,
  toolContext,
  requestMessages,
) {
  const finalText = text || "";
  const output = [];
  if (finalText) {
    const item = {
      id: `msg_${id}`,
      type: "message",
      status: "completed",
      role: "assistant",
      content: [{ type: "output_text", text: finalText, annotations: [] }],
    };
    output.push(item);
    res.write(
      sseEvent("response.output_text.done", {
        type: "response.output_text.done",
        item_id: `msg_${id}`,
        output_index: 0,
        content_index: 0,
        text: finalText,
      }),
    );
    res.write(
      sseEvent("response.content_part.done", {
        type: "response.content_part.done",
        item_id: `msg_${id}`,
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: finalText, annotations: [] },
      }),
    );
    res.write(
      sseEvent("response.output_item.done", {
        type: "response.output_item.done",
        output_index: 0,
        item,
      }),
    );
  }
  for (const [index, toolCall] of (chatToolCalls || []).entries()) {
    if (!toolCall || !toolCall.function || !toolCall.function.name) continue;
    const outputIndex = output.length;
    const completedItem = responseItemFromChatToolCall(
      toolCall,
      index,
      toolContext,
      "completed",
    );
    const addedItem = { ...completedItem, status: "in_progress" };
    if (addedItem.type === "function_call") addedItem.arguments = "";
    if (addedItem.type === "custom_tool_call") addedItem.input = "";
    res.write(
      sseEvent("response.output_item.added", {
        type: "response.output_item.added",
        output_index: outputIndex,
        item: addedItem,
      }),
    );
    if (completedItem.type === "custom_tool_call") {
      if (completedItem.input) {
        res.write(
          sseEvent("response.custom_tool_call_input.delta", {
            type: "response.custom_tool_call_input.delta",
            item_id: completedItem.id,
            output_index: outputIndex,
            delta: completedItem.input,
          }),
        );
      }
      res.write(
        sseEvent("response.custom_tool_call_input.done", {
          type: "response.custom_tool_call_input.done",
          item_id: completedItem.id,
          output_index: outputIndex,
          input: completedItem.input,
        }),
      );
    } else {
      if (completedItem.arguments) {
        res.write(
          sseEvent("response.function_call_arguments.delta", {
            type: "response.function_call_arguments.delta",
            item_id: completedItem.id,
            output_index: outputIndex,
            delta: completedItem.arguments,
          }),
        );
      }
      res.write(
        sseEvent("response.function_call_arguments.done", {
          type: "response.function_call_arguments.done",
          item_id: completedItem.id,
          output_index: outputIndex,
          name: completedItem.name,
          arguments: completedItem.arguments || "{}",
        }),
      );
    }
    res.write(
      sseEvent("response.output_item.done", {
        type: "response.output_item.done",
        output_index: outputIndex,
        item: completedItem,
      }),
    );
    output.push(completedItem);
  }
  res.write(
    sseEvent("response.completed", {
      type: "response.completed",
      response: baseStreamResponse(id, model, "completed", output),
    }),
  );
  rememberChatResponse(id, requestMessages, {
    role: "assistant",
    content: finalText || null,
    ...((chatToolCalls || []).length > 0 ? { tool_calls: chatToolCalls } : {}),
  });
  res.write("data: [DONE]\n\n");
  res.end();
}

function chatJsonToResponseStream(
  raw,
  res,
  id,
  model,
  toolContext,
  requestMessages,
) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    requestsFailed += 1;
    sendJson(res, 502, {
      error: { message: `Invalid upstream response: ${error.message}` },
    });
    return;
  }
  const choice = Array.isArray(parsed.choices) ? parsed.choices[0] : undefined;
  const message = choice && choice.message ? choice.message : {};
  const text = message.content || "";
  const toolCalls = Array.isArray(message.tool_calls)
    ? message.tool_calls
    : message.function_call
      ? [{ id: message.function_call.id, function: message.function_call }]
      : [];
  const state = { textStarted: false };
  responseStreamStart(res, id, model);
  responseStreamText(res, id, text, state);
  responseStreamDone(
    res,
    id,
    model,
    text,
    toolCalls,
    toolContext,
    requestMessages,
  );
}

function proxyStream(upstream, res, model, toolContext, requestMessages) {
  const id = `resp_${Date.now()}`;
  let started = false;
  let completed = false;
  let collectedText = "";
  let buffer = "";
  const toolCalls = new Map();
  const streamState = { textStarted: false };
  const upstreamStatus = upstream.statusCode || 200;
  const contentType = String(upstream.headers["content-type"] || "");

  if (upstreamStatus >= 400) {
    const chunks = [];
    upstream.on("data", (chunk) => chunks.push(chunk));
    upstream.on("end", () => {
      requestsFailed += 1;
      res.writeHead(upstreamStatus, {
        "content-type": contentType || "application/json",
      });
      res.end(Buffer.concat(chunks).toString("utf8"));
    });
    return;
  }

  res.writeHead(upstreamStatus, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });

  if (!contentType.includes("text/event-stream")) {
    const chunks = [];
    upstream.on("data", (chunk) => chunks.push(chunk));
    upstream.on("end", () =>
      chatJsonToResponseStream(
        Buffer.concat(chunks).toString("utf8"),
        res,
        id,
        model,
        toolContext,
        requestMessages,
      ),
    );
    return;
  }

  const handleBlock = (block) => {
    const data = block
      .split(/\r?\n/)
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).trim())
      .join("\n")
      .trim();
    if (!data) return;
    if (data === "[DONE]") {
      if (!completed) {
        if (!started) {
          started = true;
          responseStreamStart(res, id, model);
        }
        completed = true;
        responseStreamDone(
          res,
          id,
          model,
          collectedText,
          Array.from(toolCalls.values()),
          toolContext,
          requestMessages,
        );
      }
      return;
    }
    let parsed;
    try {
      parsed = JSON.parse(data);
    } catch (_) {
      return;
    }
    const delta =
      parsed.choices && parsed.choices[0] && parsed.choices[0].delta;
    if (!started) {
      started = true;
      responseStreamStart(res, id, model);
    }
    responseStreamReasoning(
      res,
      delta && (delta.reasoning_content || delta.reasoning),
    );
    const text = delta && (delta.content || delta.text);
    if (text) {
      collectedText += text;
      responseStreamText(res, id, text, streamState);
    }
    const deltaToolCalls =
      delta && Array.isArray(delta.tool_calls) ? delta.tool_calls : [];
    if (delta && delta.function_call) {
      deltaToolCalls.push({
        index: 0,
        id: delta.function_call.id,
        function: delta.function_call,
      });
    }
    for (const [position, toolDelta] of deltaToolCalls.entries()) {
      const index = Number.isInteger(toolDelta.index)
        ? toolDelta.index
        : position;
      const state = toolCalls.get(index) || {
        id: "",
        type: "function",
        function: { name: "", arguments: "" },
      };
      if (toolDelta.id) state.id = toolDelta.id;
      const fn = toolDelta.function || {};
      if (fn.name) state.function.name += fn.name;
      if (fn.arguments) state.function.arguments += fn.arguments;
      toolCalls.set(index, state);
    }
    const finish =
      parsed.choices && parsed.choices[0] && parsed.choices[0].finish_reason;
    if (finish && !completed) {
      completed = true;
      responseStreamDone(
        res,
        id,
        model,
        collectedText,
        Array.from(toolCalls.values()),
        toolContext,
        requestMessages,
      );
    }
  };

  upstream.on("data", (chunk) => {
    buffer += chunk.toString("utf8");
    const parts = buffer.split(/\r?\n\r?\n/);
    buffer = parts.pop() || "";
    for (const block of parts) {
      handleBlock(block);
    }
  });
  upstream.on("end", () => {
    if (buffer.trim()) handleBlock(buffer);
    if (!completed) {
      if (!started) {
        started = true;
        responseStreamStart(res, id, model);
      }
      responseStreamDone(
        res,
        id,
        model,
        collectedText,
        Array.from(toolCalls.values()),
        toolContext,
        requestMessages,
      );
    }
  });
}

async function handleResponses(req, res) {
  requestsTotal += 1;
  lastRequestAt = new Date().toISOString();
  try {
    const body = JSON.parse(await readBody(req));
    const converted = responsesToChat(body, currentConfig);
    const chatBody = converted.chat;
    const toolContext = converted.toolContext;
    const upstreamUrl = chatCompletionsUrl(currentConfig.upstreamUrl);
    const transport = upstreamUrl.protocol === "https:" ? https : http;
    const upstreamReq = transport.request(
      upstreamUrl,
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${currentConfig.apiKey}`,
        },
      },
      (upstreamRes) => {
        if (chatBody.stream) {
          proxyStream(
            upstreamRes,
            res,
            chatBody.model,
            toolContext,
            chatBody.messages,
          );
          return;
        }
        const chunks = [];
        upstreamRes.on("data", (chunk) => chunks.push(chunk));
        upstreamRes.on("end", () => {
          const raw = Buffer.concat(chunks).toString("utf8");
          if ((upstreamRes.statusCode || 500) >= 400) {
            requestsFailed += 1;
            res.writeHead(upstreamRes.statusCode || 500, {
              "content-type": "application/json",
            });
            res.end(raw);
            return;
          }
          try {
            const parsed = JSON.parse(raw);
            const response = chatToResponse(
              parsed,
              chatBody.model,
              toolContext,
            );
            const choice = Array.isArray(parsed.choices)
              ? parsed.choices[0]
              : undefined;
            rememberChatResponse(
              response.id,
              chatBody.messages,
              (choice && choice.message) || {
                role: "assistant",
                content: null,
              },
            );
            sendJson(res, 200, response);
          } catch (error) {
            requestsFailed += 1;
            sendJson(res, 502, {
              error: { message: `Invalid upstream response: ${error.message}` },
            });
          }
        });
      },
    );
    upstreamReq.on("error", (error) => {
      requestsFailed += 1;
      sendJson(res, 502, { error: { message: error.message } });
    });
    upstreamReq.end(JSON.stringify(chatBody));
  } catch (error) {
    requestsFailed += 1;
    sendJson(res, 400, { error: { message: error.message } });
  }
}

function statusPayload() {
  const profileBackup = readActiveCodexBackup();
  return {
    proxy: {
      running: Boolean(proxyServer),
      port: currentConfig.proxyPort,
      upstream_url: currentConfig.upstreamUrl,
      upstream_model: currentConfig.model,
      requests_total: requestsTotal,
      requests_failed: requestsFailed,
      last_request_at: lastRequestAt,
    },
    codex: {
      config_path: CODEX_CONFIG_PATH,
      current_model: currentConfig.model,
      current_provider: "nexus-agent",
      model_catalog_path: fs.existsSync(CODEX_MODEL_CATALOG_PATH)
        ? CODEX_MODEL_CATALOG_PATH
        : null,
      original_profile_preserved: Boolean(profileBackup),
      original_auth_preserved: Boolean(profileBackup?.files?.auth?.existed),
    },
    agent: {
      version: VERSION,
      pid: process.pid,
      started_at: startedAt.toISOString(),
    },
  };
}

function fileContainsNeedle(file, needle) {
  let descriptor;
  try {
    descriptor = fs.openSync(file, "r");
    const buffer = Buffer.allocUnsafe(256 * 1024);
    let overlap = Buffer.alloc(0);
    for (;;) {
      const bytes = fs.readSync(descriptor, buffer, 0, buffer.length, null);
      if (!bytes) return false;
      const chunk = Buffer.concat([overlap, buffer.subarray(0, bytes)]);
      if (chunk.includes(needle)) return true;
      overlap = chunk.subarray(Math.max(0, chunk.length - needle.length + 1));
    }
  } catch (_) {
    return false;
  } finally {
    if (descriptor !== undefined) {
      try {
        fs.closeSync(descriptor);
      } catch (_) {}
    }
  }
}

function collectFiles(root, predicate, files, depth, maxDepth) {
  if (depth > maxDepth) return;
  let entries;
  try {
    entries = fs.readdirSync(root, { withFileTypes: true });
  } catch (_) {
    return;
  }
  for (const entry of entries) {
    const current = path.join(root, entry.name);
    if (entry.isDirectory()) {
      collectFiles(current, predicate, files, depth + 1, maxDepth);
    } else if (entry.isFile() && predicate(current)) {
      files.push(current);
    }
  }
}

function hasAgentThreadHistory() {
  const sessionFiles = [];
  collectFiles(
    path.join(CODEX_DIR, "sessions"),
    (file) => file.endsWith(".jsonl"),
    sessionFiles,
    0,
    8,
  );
  collectFiles(
    path.join(CODEX_DIR, "archived_sessions"),
    (file) => file.endsWith(".jsonl"),
    sessionFiles,
    0,
    4,
  );
  // A false positive is safer than silently restoring a profile that would
  // orphan a thread. The helper performs the exact structured match when a
  // SQLite-capable Python is available.
  const needle = Buffer.from("nexus-agent");
  if (sessionFiles.some((file) => fileContainsNeedle(file, needle)))
    return true;

  const stateFiles = [];
  collectFiles(
    CODEX_DIR,
    (file) => /state_\d+\.sqlite$/.test(file),
    stateFiles,
    0,
    1,
  );
  return stateFiles.some((file) =>
    fileContainsNeedle(file, Buffer.from("nexus-agent")),
  );
}

function runHistoryMaintenance() {
  const args = [HISTORY_MAINTAINER_PATH, "--source-provider", "nexus-agent"];
  for (const binary of ["python3", "python"]) {
    const result = childProcess.spawnSync(binary, args, {
      env: process.env,
      encoding: "utf8",
      timeout: 60_000,
      maxBuffer: 1024 * 1024,
    });
    if (result.error && result.error.code === "ENOENT") continue;
    if (result.error) {
      throw new Error(
        `Codex history maintenance failed: ${result.error.message}`,
      );
    }
    const output = String(result.stdout || "").trim();
    let payload;
    try {
      payload = JSON.parse(output);
    } catch (_) {
      throw new Error(
        `Codex history maintenance returned invalid output: ${output || result.stderr}`,
      );
    }
    if (result.status !== 0 || !payload.ok) {
      throw new Error(
        payload.error ||
          String(result.stderr || "Codex history maintenance failed").trim(),
      );
    }
    return payload;
  }

  if (hasAgentThreadHistory()) {
    throw new Error(
      "Remote Agent found existing Codex threads but Python with sqlite3 is unavailable. Install Python 3, then stop again; the agent was left running and no profile was changed.",
    );
  }
  return {
    ok: true,
    skipped: "no_matching_threads_without_python",
    jsonlFiles: 0,
    stateRows: 0,
  };
}

async function controlHandler(req, res) {
  const url = new URL(req.url, `http://${req.headers.host || "127.0.0.1"}`);
  if (req.method === "GET" && url.pathname === "/health") {
    return sendJson(res, 200, {
      ok: true,
      version: VERSION,
      uptime_seconds: Math.floor(process.uptime()),
    });
  }
  if (req.method === "GET" && url.pathname === "/status") {
    return sendJson(res, 200, statusPayload());
  }
  if (req.method === "POST" && url.pathname === "/switch") {
    const patch = JSON.parse(await readBody(req));
    currentConfig = { ...currentConfig, ...patch };
    writeJson(CONFIG_PATH, currentConfig);
    writeCodexConfig(currentConfig);
    return sendJson(res, 200, {
      ok: true,
      written_files: [CODEX_CONFIG_PATH, CODEX_AUTH_PATH],
      proxy_restarted: false,
    });
  }
  if (req.method === "POST" && url.pathname === "/configure") {
    const patch = JSON.parse(await readBody(req));
    currentConfig = { ...currentConfig, ...patch };
    writeJson(CONFIG_PATH, currentConfig);
    return sendJson(res, 200, { ok: true });
  }
  if (req.method === "POST" && url.pathname === "/stop") {
    const options = JSON.parse((await readBody(req)) || "{}");
    const maintenance =
      options.maintainHistory === false
        ? { ok: true, skipped: "agent_redeploy" }
        : runHistoryMaintenance();
    const restored = restoreCodexProfile();
    sendJson(res, 200, {
      ok: true,
      restored_backup: restored,
      history_maintenance: maintenance,
    });
    setTimeout(() => shutdown(), 50);
    return;
  }
  sendJson(res, 404, { error: "not_found" });
}

function start() {
  ensureDir(ROOT);
  writeFileAtomic(PID_PATH, String(process.pid), 0o600);
  preserveCodexProfile();
  writeCodexConfig(currentConfig);
  proxyServer = http.createServer((req, res) => {
    const url = new URL(req.url, `http://${req.headers.host || "127.0.0.1"}`);
    if (req.method === "GET" && url.pathname === "/health")
      return sendJson(res, 200, { ok: true });
    if (req.method === "GET" && url.pathname === "/v1/models") {
      return sendJson(res, 200, {
        object: "list",
        data: [
          { id: currentConfig.model, object: "model", owned_by: "nexus-agent" },
        ],
      });
    }
    if (req.method === "POST" && url.pathname === "/v1/responses")
      return handleResponses(req, res);
    sendJson(res, 404, { error: "not_found" });
  });
  controlServer = http.createServer((req, res) => {
    controlHandler(req, res).catch((error) =>
      sendJson(res, 500, { error: error.message }),
    );
  });
  proxyServer.listen(currentConfig.proxyPort, "127.0.0.1");
  controlServer.listen(currentConfig.controlPort, "127.0.0.1");
}

function shutdown() {
  if (stopping) return;
  stopping = true;
  try {
    fs.unlinkSync(PID_PATH);
  } catch (_) {}
  if (proxyServer) proxyServer.close();
  if (controlServer) controlServer.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 5000).unref();
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
if (process.argv.includes("--restore")) {
  try {
    const maintenance = runHistoryMaintenance();
    const restored = restoreCodexProfile();
    console.log(
      JSON.stringify({
        ok: true,
        restored_backup: restored,
        history_maintenance: maintenance,
      }),
    );
    process.exit(0);
  } catch (error) {
    console.error(String(error && error.message ? error.message : error));
    process.exit(1);
  }
}
start();
