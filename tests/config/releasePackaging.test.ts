import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(path, "utf8");

const releaseWorkflow = read(".github/workflows/release.yml");
const cargoManifest = read("src-tauri/Cargo.toml");
const tauriConfig = JSON.parse(read("src-tauri/tauri.conf.json")) as {
  identifier: string;
  mainBinaryName?: string;
  bundle: { createUpdaterArtifacts: boolean };
  plugins: { updater?: unknown };
};
const windowsConfig = JSON.parse(read("src-tauri/tauri.windows.conf.json")) as {
  app: { windows: Array<{ title: string }> };
};

const flatpakMetainfo = read("flatpak/com.nexuscomposer.desktop.metainfo.xml");
const flatpakReadme = read("flatpak/README.md");
const flatpakDesktop = read("flatpak/com.nexuscomposer.desktop.desktop");
const flatpakManifest = read("flatpak/com.nexuscomposer.desktop.yml");
const flatpakFiles = [
  flatpakReadme,
  flatpakDesktop,
  flatpakMetainfo,
  flatpakManifest,
];

describe("release packaging metadata", () => {
  it("packages Nexus Composer artifacts and the Nexus executable", () => {
    for (const artifact of [
      "Nexus-Composer-${VERSION}-macOS.tar.gz",
      "Nexus-Composer-${VERSION}-macOS.zip",
      "Nexus-Composer-${VERSION}-macOS.dmg",
      "Nexus-Composer-$VERSION-Windows$assetSuffix.msi",
      "Nexus-Composer-$VERSION-Windows$assetSuffix-Portable.zip",
      "Nexus-Composer-${VERSION}-Linux-${ARCH}.AppImage",
      "Nexus-Composer-${VERSION}-Linux-${ARCH}.deb",
      "Nexus-Composer-${VERSION}-Linux-${ARCH}.rpm",
    ]) {
      expect(releaseWorkflow).toContain(artifact);
    }

    expect(releaseWorkflow).toContain("Nexus Composer.app");
    expect(releaseWorkflow).toContain("nexus-composer.exe");
    expect(releaseWorkflow).not.toMatch(/CC[ -]Switch/);
    expect(releaseWorkflow).not.toContain("cc-switch.exe");
  });

  it("uses the Cargo binary name for Windows portable builds", () => {
    const cargoPackageName = cargoManifest.match(
      /^\[package\][\s\S]*?^name\s*=\s*"([^"]+)"/m,
    )?.[1];
    const binaryName = tauriConfig.mainBinaryName ?? cargoPackageName;

    expect(cargoPackageName).toBe("nexus-composer");
    expect(binaryName).toBe("nexus-composer");
    expect(releaseWorkflow).toContain(`${binaryName}.exe`);
  });

  it("keeps updater signing and latest.json assembly enabled", () => {
    expect(tauriConfig.bundle.createUpdaterArtifacts).toBe(true);
    expect(tauriConfig.plugins.updater).toBeDefined();
    for (const required of [
      "Prepare Tauri signing key",
      "TAURI_SIGNING_PRIVATE_KEY",
      'cp "$TAR_GZ" "release-assets/$NEW_TAR_GZ"',
      'cp "$TAR_GZ.sig" "release-assets/$NEW_TAR_GZ.sig"',
      "Collect Signatures",
      "assemble-latest-json:",
      "Generate latest.json",
      "dl/*.sig",
      "latest.json --clobber",
    ]) {
      expect(releaseWorkflow).toContain(required);
    }
  });

  it("joins renamed updater artifacts with their signatures in latest.json", () => {
    for (const required of [
      'NEW_TAR_GZ="Nexus-Composer-${VERSION}-macOS.tar.gz"',
      'cp "$TAR_GZ.sig" "release-assets/$NEW_TAR_GZ.sig"',
      '$dest = "Nexus-Composer-$VERSION-Windows$assetSuffix.msi"',
      'Copy-Item $sigPath (Join-Path release-assets ("$dest.sig"))',
      'NEW_APPIMAGE="Nexus-Composer-${VERSION}-Linux-${ARCH}.AppImage"',
      'cp "$APPIMAGE.sig" "release-assets/$NEW_APPIMAGE.sig"',
      "base=${sig%.sig}",
      'fname=$(basename "$base")',
      'url="$base_url/$fname"',
      'sig_content=$(cat "$sig")',
    ]) {
      expect(releaseWorkflow).toContain(required);
    }
  });

  it("uses the Nexus title in the Windows bundle", () => {
    expect(windowsConfig.app.windows[0]?.title).toBe("Nexus Composer");
  });

  it("uses the Tauri application ID throughout Flatpak metadata", () => {
    expect(tauriConfig.identifier).toBe("com.nexuscomposer.desktop");
    for (const contents of flatpakFiles) {
      expect(contents).toContain(tauriConfig.identifier);
      expect(contents).not.toContain("com.nexus-composer.desktop");
    }
    expect(flatpakReadme).toContain(`${tauriConfig.identifier}.yml`);
    expect(flatpakDesktop).toContain(`Icon=${tauriConfig.identifier}`);
    expect(flatpakMetainfo).toContain(`<id>${tauriConfig.identifier}</id>`);
    expect(flatpakMetainfo).toContain(
      `<launchable type="desktop-id">${tauriConfig.identifier}.desktop</launchable>`,
    );
    expect(flatpakManifest).toContain(`id: ${tauriConfig.identifier}`);
    expect(flatpakManifest).toContain(`${tauriConfig.identifier}.desktop`);
    expect(flatpakManifest).toContain(`${tauriConfig.identifier}.metainfo.xml`);
    expect(flatpakMetainfo).toContain(
      "https://github.com/hdt98/nexus-composer",
    );
  });
});
