import { beforeEach, describe, expect, it } from "vitest";
import {
  getSavedSshConnectionKey,
  loadSavedSshConnections,
  removeSavedSshConnection,
  saveSshConnection,
  updateSavedSshConnectionSnapshot,
} from "@/components/remote/savedSshConnection";

describe("saved SSH connections", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("stores multiple connections without persisting passwords", () => {
    const firstConnectionWithPassword = {
      host: " 192.0.2.10 ",
      user: " locmt ",
      port: 22,
      password: "must-not-be-persisted",
    };

    saveSshConnection(firstConnectionWithPassword);
    saveSshConnection({
      host: "example.com",
      user: "ubuntu",
      port: 2222,
    });

    expect(loadSavedSshConnections()).toEqual([
      {
        host: "example.com",
        user: "ubuntu",
        port: 2222,
      },
      {
        host: "192.0.2.10",
        user: "locmt",
        port: 22,
      },
    ]);
    expect(
      window.localStorage.getItem(
        "nexus-composer.remote.savedSshConnections.v1",
      ),
    ).not.toContain("must-not-be-persisted");
  });

  it("updates the selected connection instead of duplicating it", () => {
    const originalConnection = {
      host: "old.example.com",
      user: "root",
      port: 22,
    };
    saveSshConnection(originalConnection);

    saveSshConnection(
      {
        host: "new.example.com",
        user: "admin",
        port: 2200,
      },
      getSavedSshConnectionKey(originalConnection),
    );

    expect(loadSavedSshConnections()).toEqual([
      {
        host: "new.example.com",
        user: "admin",
        port: 2200,
      },
    ]);
  });

  it("keeps the last detected model across normal reconnect saves", () => {
    const connection = {
      host: "192.0.2.11",
      user: "locmt",
      port: 22,
    };
    saveSshConnection(connection);
    updateSavedSshConnectionSnapshot(getSavedSshConnectionKey(connection), {
      lastStatus: "connected",
      lastProvider: "OpenAI Official",
      lastModel: "GPT (default)",
      lastCheckedAt: 1234,
    });

    saveSshConnection(connection);

    expect(loadSavedSshConnections()).toEqual([
      {
        ...connection,
        lastStatus: "connected",
        lastProvider: "OpenAI Official",
        lastModel: "GPT (default)",
        lastCheckedAt: 1234,
      },
    ]);
  });

  it("updates detection metadata without changing connection order", () => {
    const first = { host: "first.example.com", user: "root", port: 22 };
    const second = { host: "second.example.com", user: "root", port: 22 };
    saveSshConnection(second);
    saveSshConnection(first);

    updateSavedSshConnectionSnapshot(getSavedSshConnectionKey(second), {
      lastStatus: "running",
      lastModel: "GLM-5.2-FP8",
      lastCheckedAt: 5678,
    });

    expect(loadSavedSshConnections().map((item) => item.host)).toEqual([
      "first.example.com",
      "second.example.com",
    ]);
    expect(loadSavedSshConnections()[1]).toMatchObject({
      lastStatus: "running",
      lastModel: "GLM-5.2-FP8",
      lastCheckedAt: 5678,
    });
  });

  it("removes one connection while preserving the others", () => {
    const firstConnection = {
      host: "first.example.com",
      user: "root",
      port: 22,
    };
    const secondConnection = {
      host: "second.example.com",
      user: "ubuntu",
      port: 22,
    };
    saveSshConnection(firstConnection);
    saveSshConnection(secondConnection);

    expect(
      removeSavedSshConnection(getSavedSshConnectionKey(secondConnection)),
    ).toEqual([firstConnection]);
  });

  it("migrates the previous single-connection format", () => {
    window.localStorage.setItem(
      "nexus-composer.remote.savedSshConnection.v1",
      JSON.stringify({
        host: "legacy.example.com",
        user: "legacy",
        port: 22,
      }),
    );

    expect(loadSavedSshConnections()).toEqual([
      {
        host: "legacy.example.com",
        user: "legacy",
        port: 22,
      },
    ]);
    expect(
      window.localStorage.getItem(
        "nexus-composer.remote.savedSshConnection.v1",
      ),
    ).toBeNull();
  });

  it("ignores malformed saved data", () => {
    window.localStorage.setItem(
      "nexus-composer.remote.savedSshConnections.v1",
      JSON.stringify([{ host: "example.com", port: 70000 }]),
    );

    expect(loadSavedSshConnections()).toEqual([]);
  });
});
