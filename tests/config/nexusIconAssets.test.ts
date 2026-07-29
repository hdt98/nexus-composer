import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const sha256 = (path: string) =>
  createHash("sha256").update(readFileSync(path)).digest("hex");

const approvedIconHashes: Record<string, string> = {
  "src-tauri/icons/icon.png":
    "66d29b99863c2891072c67deb21c00ec87fca82db489eaacc2a09c550e6a5360",
  "src-tauri/icons/icon.icns":
    "45ea16a4425e53394e311d02959f20634f80485459d933c3037a452b742d5922",
  "src-tauri/icons/icon.ico":
    "10038344301b03c1a5e517246237d1ed8518e496252c438f7b8585c2db081f23",
  "src-tauri/icons/tray/macos/statusTemplate.png":
    "b0eb43e975723e807aedd2773006c05460e4cd0710c1528e487015d83418a70d",
  "src-tauri/icons/tray/macos/statusTemplate@2x.png":
    "cf2651a5cb9cda8eb913170a0005edc2ca8759bc54400ab137cbbbf7b38d5a37",
  "src-tauri/icons/tray/macos/statusbar_template_3x.png":
    "4c1e382db1571a4d37464d40e9205e8aa3669e3f7d01166603418f3fd3553227",
};

describe("Nexus Composer icon assets", () => {
  it("uses the approved Nexus artwork for application and tray icons", () => {
    for (const [path, expectedHash] of Object.entries(approvedIconHashes)) {
      expect(sha256(path), path).toBe(expectedHash);
    }
  });
});
