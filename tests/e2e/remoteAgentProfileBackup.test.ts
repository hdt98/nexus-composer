import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import {
  existsSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createServer, request, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

const agentPath = resolve(
  process.cwd(),
  "src-tauri/src/resources/nexus_remote_agent.js",
);
const historyMaintainerPath = resolve(
  process.cwd(),
  "src-tauri/src/resources/nexus_remote_history_maintainer.py",
);
const pythonBinary = ["python3", "python"].find((binary) => {
  const result = spawnSync(binary, ["-c", "import sqlite3"], {
    stdio: "ignore",
  });
  return result.status === 0;
});

async function unusedPort(): Promise<number> {
  return await new Promise((resolvePort, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close((error) => {
        if (error) reject(error);
        else if (address && typeof address !== "string")
          resolvePort(address.port);
        else reject(new Error("Unable to reserve a test port"));
      });
    });
  });
}

async function postJson(
  port: number,
  path: string,
  body: unknown,
): Promise<void> {
  await postJsonResponse(port, path, body);
}

async function postJsonResponse(
  port: number,
  path: string,
  body: unknown,
): Promise<string> {
  const payload = JSON.stringify(body);
  return await new Promise<string>((resolveRequest, reject) => {
    const req = request(
      {
        host: "127.0.0.1",
        port,
        path,
        method: "POST",
        headers: {
          "content-type": "application/json",
          "content-length": Buffer.byteLength(payload),
        },
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
        res.on("end", () => {
          const responseBody = Buffer.concat(chunks).toString("utf8");
          if (res.statusCode && res.statusCode < 300) {
            resolveRequest(responseBody);
          } else {
            reject(
              new Error(
                `Agent returned HTTP ${res.statusCode}: ${responseBody}`,
              ),
            );
          }
        });
      },
    );
    req.once("error", reject);
    req.end(payload);
  });
}

async function waitForAgent(
  port: number,
  agent: ChildProcess,
  output: string[],
): Promise<void> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (agent.exitCode !== null) {
      throw new Error(`Remote agent exited: ${output.join("")}`);
    }
    try {
      await postJson(port, "/configure", {});
      return;
    } catch (_) {
      await new Promise((resolveWait) => setTimeout(resolveWait, 50));
    }
  }
  throw new Error(`Remote agent did not start: ${output.join("")}`);
}

async function waitForExit(process: ChildProcess): Promise<void> {
  if (process.exitCode !== null) return;
  await new Promise<void>((resolveExit) =>
    process.once("exit", () => resolveExit()),
  );
}

function completedResponseId(sse: string): string {
  for (const block of sse.split(/\r?\n\r?\n/)) {
    if (!block.startsWith("event: response.completed")) continue;
    const data = block.split(/\r?\n/).find((line) => line.startsWith("data: "));
    if (data) return JSON.parse(data.slice(6)).response.id;
  }
  throw new Error("Missing response.completed event");
}

describe("remote agent Codex profile preservation", () => {
  let home: string | undefined;
  let agent: ChildProcess | undefined;
  let upstream: Server | undefined;

  afterEach(async () => {
    if (agent && agent.exitCode === null) {
      agent.kill("SIGTERM");
      await waitForExit(agent);
    }
    if (upstream) {
      await new Promise<void>((resolveClose) =>
        upstream?.close(() => resolveClose()),
      );
      upstream = undefined;
    }
    if (home) rmSync(home, { recursive: true, force: true });
  });

  it("restores the original remote config and auth after provider switches", async () => {
    home = mkdtempSync(join(tmpdir(), "nexus-remote-agent-"));
    const codexDir = join(home, ".codex");
    const agentDir = join(home, ".nexus-composer", "remote-agent");
    const originalConfig = `model_provider = "openai"
model = "gpt-5.5"

[mcp_servers.keep-me]
command = "remote-mcp"

[features]
js_repl = false
`;
    const originalAuth = '{"tokens":{"access_token":"remote-session"}}\n';
    const modelCatalog = {
      models: [
        {
          slug: "GLM-5.2-FP8",
          display_name: "GLM-5.2",
          context_window: 128_000,
        },
      ],
    };
    const proxyPort = await unusedPort();
    const controlPort = await unusedPort();

    mkdirSync(codexDir, { recursive: true });
    mkdirSync(agentDir, { recursive: true });
    writeFileSync(join(codexDir, "config.toml"), originalConfig);
    writeFileSync(join(codexDir, "auth.json"), originalAuth);
    writeFileSync(
      join(agentDir, "agent-config.json"),
      JSON.stringify({
        proxyPort,
        controlPort,
        upstreamUrl: "http://127.0.0.1:30000/v1",
        apiKey: "test-key",
        model: "GLM-5.2-FP8",
        modelCatalog,
      }),
    );
    const executableAgentPath = join(agentDir, "nexus-remote-agent.cjs");
    // The bundled script is CommonJS. Deploying it on a remote host works
    // because its agent directory has no package.json; use .cjs locally so
    // this repository's package-level ESM mode does not alter that behavior.
    copyFileSync(agentPath, executableAgentPath);
    copyFileSync(
      historyMaintainerPath,
      join(agentDir, "nexus-remote-history-maintainer.py"),
    );

    const output: string[] = [];
    agent = spawn(process.execPath, [executableAgentPath], {
      env: { ...process.env, HOME: home },
      stdio: ["ignore", "ignore", "pipe"],
    });
    agent.stderr?.on("data", (chunk) => output.push(String(chunk)));
    await waitForAgent(controlPort, agent, output);

    const backupPath = join(agentDir, "active-codex-backup.json");
    const catalogPath = join(codexDir, "nexus-remote-agent-model-catalog.json");
    const managedConfig = readFileSync(join(codexDir, "config.toml"), "utf8");
    expect(managedConfig).toContain(
      `model_catalog_json = ${JSON.stringify(catalogPath)}`,
    );
    expect(managedConfig).toContain("use_legacy_landlock = true");
    expect(managedConfig).toContain("[mcp_servers.keep-me]");
    expect(managedConfig).toContain('command = "remote-mcp"');
    expect(managedConfig).toContain("js_repl = false");
    expect(readFileSync(join(codexDir, "auth.json"), "utf8")).toBe(
      originalAuth,
    );
    expect(JSON.parse(readFileSync(catalogPath, "utf8"))).toEqual(modelCatalog);
    const backupBeforeSwitch = readFileSync(backupPath, "utf8");
    await postJson(controlPort, "/switch", { model: "GLM-5.2-FP8" });
    expect(readFileSync(backupPath, "utf8")).toBe(backupBeforeSwitch);

    await postJson(controlPort, "/stop", {});
    await waitForExit(agent);
    agent = undefined;

    expect(readFileSync(join(codexDir, "config.toml"), "utf8")).toBe(
      originalConfig,
    );
    expect(readFileSync(join(codexDir, "auth.json"), "utf8")).toBe(
      originalAuth,
    );
    expect(existsSync(catalogPath)).toBe(false);
    expect(existsSync(backupPath)).toBe(false);
  }, 15_000);

  it("bridges a streamed custom tool call and its result", async () => {
    home = mkdtempSync(join(tmpdir(), "nexus-remote-agent-tools-"));
    const codexDir = join(home, ".codex");
    const agentDir = join(home, ".nexus-composer", "remote-agent");
    const proxyPort = await unusedPort();
    const controlPort = await unusedPort();
    const upstreamPort = await unusedPort();
    const upstreamBodies: Array<{
      messages?: Array<{
        role?: string;
        content?: string;
        tool_calls?: unknown[];
      }>;
    }> = [];
    const upstreamPaths: string[] = [];

    upstream = createServer((req, res) => {
      upstreamPaths.push(req.url ?? "");
      const chunks: Buffer[] = [];
      req.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
      req.on("end", () => {
        const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        upstreamBodies.push(body);
        const hasToolResult = body.messages.some(
          (message: { role?: string }) => message.role === "tool",
        );
        res.writeHead(200, { "content-type": "text/event-stream" });
        if (hasToolResult) {
          res.write(
            `data: ${JSON.stringify({
              id: "chat_2",
              choices: [
                {
                  index: 0,
                  delta: { content: "Found 2 repositories." },
                  finish_reason: "stop",
                },
              ],
            })}\n\n`,
          );
        } else {
          res.write(
            `data: ${JSON.stringify({
              id: "chat_1",
              choices: [
                {
                  index: 0,
                  delta: {
                    tool_calls: [
                      {
                        index: 0,
                        id: "call_1",
                        type: "function",
                        function: {
                          name: "exec",
                          arguments: '{"input":"find . ',
                        },
                      },
                    ],
                  },
                  finish_reason: null,
                },
              ],
            })}\n\n`,
          );
          res.write(
            `data: ${JSON.stringify({
              id: "chat_1",
              choices: [
                {
                  index: 0,
                  delta: {
                    tool_calls: [
                      {
                        index: 0,
                        function: { arguments: '-name .git"}' },
                      },
                    ],
                  },
                  finish_reason: "tool_calls",
                },
              ],
            })}\n\n`,
          );
        }
        res.end("data: [DONE]\n\n");
      });
    });
    await new Promise<void>((resolveListen, reject) => {
      upstream?.once("error", reject);
      upstream?.listen(upstreamPort, "127.0.0.1", resolveListen);
    });

    mkdirSync(codexDir, { recursive: true });
    mkdirSync(agentDir, { recursive: true });
    writeFileSync(
      join(agentDir, "agent-config.json"),
      JSON.stringify({
        proxyPort,
        controlPort,
        // A full Chat Completions URL is valid provider input. The Agent must
        // not append `/chat/completions` a second time.
        upstreamUrl: `http://127.0.0.1:${upstreamPort}/v1/chat/completions`,
        apiKey: "test-key",
        model: "GLM-5.2-FP8",
      }),
    );
    const executableAgentPath = join(agentDir, "nexus-remote-agent.cjs");
    copyFileSync(agentPath, executableAgentPath);
    copyFileSync(
      historyMaintainerPath,
      join(agentDir, "nexus-remote-history-maintainer.py"),
    );
    const output: string[] = [];
    agent = spawn(process.execPath, [executableAgentPath], {
      env: { ...process.env, HOME: home },
      stdio: ["ignore", "ignore", "pipe"],
    });
    agent.stderr?.on("data", (chunk) => output.push(String(chunk)));
    await waitForAgent(controlPort, agent, output);

    const tools = [
      {
        type: "custom",
        name: "exec",
        description: "Run a shell command",
      },
    ];
    const firstResponse = await postJsonResponse(proxyPort, "/v1/responses", {
      model: "GLM-5.2-FP8",
      input: [{ role: "user", content: "Inspect repositories" }],
      tools,
      stream: true,
    });
    expect(firstResponse).toContain(
      "event: response.custom_tool_call_input.done",
    );
    expect(firstResponse).toContain('"type":"custom_tool_call"');
    expect(firstResponse).toContain('"call_id":"call_1"');
    expect(firstResponse).toContain('"input":"find . -name .git"');
    const previousResponseId = completedResponseId(firstResponse);

    const secondResponse = await postJsonResponse(proxyPort, "/v1/responses", {
      model: "GLM-5.2-FP8",
      previous_response_id: previousResponseId,
      input: [
        {
          type: "custom_tool_call_output",
          call_id: "call_1",
          output: "./repo-a/.git\n./repo-b/.git",
        },
      ],
      tools,
      stream: true,
    });
    expect(secondResponse).toContain("Found 2 repositories.");
    expect(upstreamPaths).toEqual([
      "/v1/chat/completions",
      "/v1/chat/completions",
    ]);
    expect(upstreamBodies).toHaveLength(2);
    expect(upstreamBodies[1].messages).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          role: "assistant",
          tool_calls: [
            expect.objectContaining({
              id: "call_1",
              function: {
                name: "exec",
                arguments: '{"input":"find . -name .git"}',
              },
            }),
          ],
        }),
        expect.objectContaining({
          role: "tool",
          content: "./repo-a/.git\n./repo-b/.git",
        }),
      ]),
    );
  }, 15_000);

  it.skipIf(!pythonBinary || process.platform === "win32")(
    "moves existing official history into and out of the Remote Agent bucket",
    () => {
      home = mkdtempSync(join(tmpdir(), "nexus-remote-agent-unified-history-"));
      const codexDir = join(home, ".codex");
      const sessionDir = join(codexDir, "sessions", "2026", "07", "27");
      const agentDir = join(home, ".nexus-composer", "remote-agent");
      const backupDir = join(agentDir, "backups", "official");
      const sessionPath = join(sessionDir, "rollout-official-thread.jsonl");
      const existingAgentSessionPath = join(
        sessionDir,
        "rollout-existing-agent-thread.jsonl",
      );
      const stateDbPath = join(codexDir, "state_5.sqlite");
      const originalConfig =
        'model_provider = "openai"\nmodel = "gpt-5.6-sol"\n';

      mkdirSync(sessionDir, { recursive: true });
      mkdirSync(backupDir, { recursive: true });
      writeFileSync(join(codexDir, "config.toml"), originalConfig);
      writeFileSync(join(backupDir, "config.toml"), originalConfig);
      writeFileSync(
        join(agentDir, "active-codex-backup.json"),
        JSON.stringify({
          backupDir,
          files: {
            config: {
              existed: true,
              path: join(backupDir, "config.toml"),
            },
          },
        }),
      );
      writeFileSync(
        sessionPath,
        `${JSON.stringify({
          type: "session_meta",
          payload: {
            id: "thread-official",
            model_provider: "openai",
          },
        })}\n${JSON.stringify({
          type: "turn_context",
          payload: { model: "gpt-5.6-sol" },
        })}\n`,
      );
      writeFileSync(
        existingAgentSessionPath,
        `${JSON.stringify({
          type: "session_meta",
          payload: {
            id: "thread-existing-agent",
            model_provider: "nexus-agent",
          },
        })}\n${JSON.stringify({
          type: "turn_context",
          payload: { model: "gpt-5.6-sol" },
        })}\n`,
      );
      const createDb = spawnSync(
        pythonBinary!,
        [
          "-c",
          [
            "import sqlite3,sys",
            "db=sqlite3.connect(sys.argv[1])",
            "db.execute('CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, model TEXT)')",
            "db.execute('INSERT INTO threads VALUES (?, ?, ?)', ('thread-official', 'openai', 'gpt-5.6-sol'))",
            "db.execute('INSERT INTO threads VALUES (?, ?, ?)', ('thread-existing-agent', 'nexus-agent', 'gpt-5.6-sol'))",
            "db.commit()",
            "db.close()",
          ].join(";"),
          stateDbPath,
        ],
        { encoding: "utf8" },
      );
      expect(createDb.status, createDb.stderr).toBe(0);

      const enable = spawnSync(
        pythonBinary!,
        [
          historyMaintainerPath,
          "--home",
          home,
          "--source-provider",
          "openai",
          "--target-provider",
          "nexus-agent",
          "--target-model",
          "GLM-5.2-FP8",
        ],
        { encoding: "utf8" },
      );
      expect(enable.status, enable.stderr || enable.stdout).toBe(0);
      expect(JSON.parse(enable.stdout)).toMatchObject({
        sourceProvider: "openai",
        targetProvider: "nexus-agent",
        targetModel: "GLM-5.2-FP8",
        jsonlFiles: 2,
        stateRows: 2,
      });
      const enabledLines = readFileSync(sessionPath, "utf8")
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line));
      expect(enabledLines[0].payload.model_provider).toBe("nexus-agent");
      expect(enabledLines[1].payload.model).toBe("GLM-5.2-FP8");
      const normalizedAgentLines = readFileSync(
        existingAgentSessionPath,
        "utf8",
      )
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line));
      expect(normalizedAgentLines[0].payload.model_provider).toBe(
        "nexus-agent",
      );
      expect(normalizedAgentLines[1].payload.model).toBe("GLM-5.2-FP8");

      const disable = spawnSync(
        pythonBinary!,
        [
          historyMaintainerPath,
          "--home",
          home,
          "--source-provider",
          "nexus-agent",
        ],
        { encoding: "utf8" },
      );
      expect(disable.status, disable.stderr || disable.stdout).toBe(0);
      expect(JSON.parse(disable.stdout)).toMatchObject({
        targetProvider: "openai",
        targetModel: "gpt-5.6-sol",
        jsonlFiles: 2,
        stateRows: 2,
      });
      const restoredLines = readFileSync(sessionPath, "utf8")
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line));
      expect(restoredLines[0].payload.model_provider).toBe("openai");
      expect(restoredLines[1].payload.model).toBe("gpt-5.6-sol");
    },
    15_000,
  );

  it.skipIf(!pythonBinary || process.platform === "win32")(
    "backs up and retargets Remote Agent threads before restoring the profile",
    async () => {
      home = mkdtempSync(join(tmpdir(), "nexus-remote-agent-history-"));
      const codexDir = join(home, ".codex");
      const sessionDir = join(codexDir, "sessions", "2026", "07", "27");
      const agentDir = join(home, ".nexus-composer", "remote-agent");
      const originalConfig =
        'model_provider = "openai"\nmodel = "gpt-5.6-sol"\n';
      const sessionPath = join(sessionDir, "rollout-old-agent-thread.jsonl");
      const stateDbPath = join(codexDir, "state_5.sqlite");
      const proxyPort = await unusedPort();
      const controlPort = await unusedPort();

      mkdirSync(sessionDir, { recursive: true });
      mkdirSync(agentDir, { recursive: true });
      writeFileSync(join(codexDir, "config.toml"), originalConfig);
      writeFileSync(
        sessionPath,
        `${JSON.stringify({
          type: "session_meta",
          payload: {
            id: "thread-old-agent",
            model_provider: "nexus-agent",
          },
        })}\n${JSON.stringify({
          type: "turn_context",
          payload: { model: "GLM-5.2-FP8" },
        })}\n`,
      );
      const createDb = spawnSync(
        pythonBinary!,
        [
          "-c",
          [
            "import sqlite3,sys",
            "db=sqlite3.connect(sys.argv[1])",
            "db.execute('CREATE TABLE threads (id TEXT PRIMARY KEY, model_provider TEXT, model TEXT)')",
            "db.execute('INSERT INTO threads VALUES (?, ?, ?)', ('thread-old-agent', 'nexus-agent', 'GLM-5.2-FP8'))",
            "db.commit()",
            "db.close()",
          ].join(";"),
          stateDbPath,
        ],
        { encoding: "utf8" },
      );
      expect(createDb.status, createDb.stderr).toBe(0);

      writeFileSync(
        join(agentDir, "agent-config.json"),
        JSON.stringify({
          proxyPort,
          controlPort,
          upstreamUrl: "http://127.0.0.1:30000/v1",
          apiKey: "test-key",
          model: "GLM-5.2-FP8",
        }),
      );
      const executableAgentPath = join(agentDir, "nexus-remote-agent.cjs");
      copyFileSync(agentPath, executableAgentPath);
      copyFileSync(
        historyMaintainerPath,
        join(agentDir, "nexus-remote-history-maintainer.py"),
      );

      const output: string[] = [];
      agent = spawn(process.execPath, [executableAgentPath], {
        env: { ...process.env, HOME: home },
        stdio: ["ignore", "ignore", "pipe"],
      });
      agent.stderr?.on("data", (chunk) => output.push(String(chunk)));
      await waitForAgent(controlPort, agent, output);

      const stopResponse = JSON.parse(
        await postJsonResponse(controlPort, "/stop", {}),
      );
      await waitForExit(agent);
      agent = undefined;

      expect(stopResponse.history_maintenance).toMatchObject({
        ok: true,
        sourceProvider: "nexus-agent",
        targetProvider: "openai",
        targetModel: "gpt-5.6-sol",
        jsonlFiles: 1,
        stateRows: 1,
      });
      expect(
        JSON.parse(readFileSync(sessionPath, "utf8").split("\n")[0]).payload
          .model_provider,
      ).toBe("openai");

      const inspectDb = spawnSync(
        pythonBinary!,
        [
          "-c",
          "import sqlite3,sys; print('|'.join(sqlite3.connect(sys.argv[1]).execute('SELECT model_provider, model FROM threads WHERE id=?', ('thread-old-agent',)).fetchone()))",
          stateDbPath,
        ],
        { encoding: "utf8" },
      );
      expect(inspectDb.status, inspectDb.stderr).toBe(0);
      expect(inspectDb.stdout.trim()).toBe("openai|gpt-5.6-sol");
      expect(
        existsSync(
          join(stopResponse.history_maintenance.backupDir, "manifest.json"),
        ),
      ).toBe(true);
      expect(readFileSync(join(codexDir, "config.toml"), "utf8")).toBe(
        originalConfig,
      );
    },
    15_000,
  );
});
