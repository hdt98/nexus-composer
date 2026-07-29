import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";

const HAN_SCRIPT = /\p{Script=Han}/gu;
const UNICODE_ESCAPE = /\\u(?:\{([0-9a-fA-F]{1,6})\}|([0-9a-fA-F]{4}))/g;
const MAINTAINED_EXTENSIONS = new Set([
  ".cjs",
  ".css",
  ".html",
  ".js",
  ".json",
  ".jsx",
  ".md",
  ".mjs",
  ".plist",
  ".ps1",
  ".rs",
  ".sh",
  ".sql",
  ".svg",
  ".toml",
  ".ts",
  ".tsx",
  ".txt",
  ".yaml",
  ".yml",
]);
const EXCLUDED_FILES = new Set(["pnpm-lock.yaml", "src-tauri/Cargo.lock"]);
const ALLOWLIST_PATH = "scripts/language-allowlist.json";

function trackedFiles() {
  return execFileSync("git", ["ls-files", "-z"], { encoding: "utf8" })
    .split("\0")
    .filter(Boolean)
    .filter((file) => {
      if (EXCLUDED_FILES.has(file)) return false;
      return MAINTAINED_EXTENSIONS.has(path.extname(file).toLowerCase());
    });
}

function loadAllowlist() {
  const entries = JSON.parse(readFileSync(ALLOWLIST_PATH, "utf8"));
  if (!Array.isArray(entries)) {
    throw new Error(ALLOWLIST_PATH + " must contain a JSON array");
  }

  return entries.map((entry, index) => {
    if (
      !entry ||
      typeof entry.path !== "string" ||
      typeof entry.text !== "string" ||
      typeof entry.reason !== "string" ||
      !entry.path ||
      !entry.text ||
      !entry.reason
    ) {
      throw new Error(
        ALLOWLIST_PATH +
          " entry " +
          (index + 1) +
          " requires path, text, and reason",
      );
    }
    if (!HAN_SCRIPT.test(entry.text)) {
      HAN_SCRIPT.lastIndex = 0;
      throw new Error(
        ALLOWLIST_PATH +
          " entry " +
          (index + 1) +
          " does not contain Han script",
      );
    }
    HAN_SCRIPT.lastIndex = 0;
    return { ...entry, used: false };
  });
}

function isHanCodePoint(codePoint) {
  return (
    (codePoint >= 0x3400 && codePoint <= 0x4dbf) ||
    (codePoint >= 0x4e00 && codePoint <= 0x9fff) ||
    (codePoint >= 0xf900 && codePoint <= 0xfaff) ||
    (codePoint >= 0x20000 && codePoint <= 0x323af)
  );
}

function containsEscapedHan(line) {
  UNICODE_ESCAPE.lastIndex = 0;
  for (const match of line.matchAll(UNICODE_ESCAPE)) {
    const codePoint = Number.parseInt(match[1] ?? match[2], 16);
    if (isHanCodePoint(codePoint)) return true;
  }
  return false;
}

const allowlist = loadAllowlist();
const violations = [];

for (const file of trackedFiles()) {
  const lines = readFileSync(file, "utf8").split(/\r?\n/);
  lines.forEach((line, lineIndex) => {
    const hasLiteralHan = HAN_SCRIPT.test(line);
    HAN_SCRIPT.lastIndex = 0;
    const hasEscapedHan = containsEscapedHan(line);
    if (!hasLiteralHan && !hasEscapedHan) {
      return;
    }

    let remaining = line;
    for (const entry of allowlist) {
      if (entry.path !== file || !remaining.includes(entry.text)) continue;
      remaining = remaining.split(entry.text).join("");
      entry.used = true;
    }

    if (HAN_SCRIPT.test(remaining) || hasEscapedHan) {
      violations.push({
        file,
        line: lineIndex + 1,
        text: line.trim(),
        escaped: hasEscapedHan,
      });
    }
    HAN_SCRIPT.lastIndex = 0;
  });
}

const unusedAllowlist = allowlist.filter((entry) => !entry.used);
if (unusedAllowlist.length > 0) {
  console.error("Unused language allowlist entries:");
  for (const entry of unusedAllowlist) {
    console.error(
      "- " + entry.path + ": " + entry.text + " (" + entry.reason + ")",
    );
  }
}

if (violations.length > 0) {
  console.error(
    "Found " +
      violations.length +
      " maintained-text line(s) containing Han script:",
  );
  for (const violation of violations.slice(0, 200)) {
    console.error(
      violation.file +
        ":" +
        violation.line +
        (violation.escaped ? " (escaped Han)" : "") +
        ": " +
        violation.text,
    );
  }
  if (violations.length > 200) {
    console.error("...and " + (violations.length - 200) + " more.");
  }
}

if (violations.length > 0 || unusedAllowlist.length > 0) {
  process.exit(1);
}

console.log(
  "Language check passed for maintained text (" +
    allowlist.length +
    " allowlisted value(s)).",
);
