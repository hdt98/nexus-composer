import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(path, "utf8");
const expectContainsAll = (contents: string, snippets: string[]) => {
  for (const snippet of snippets) expect(contents).toContain(snippet);
};

const repositoryUrl = "https://github.com/hdt98/nexus-composer";
const updaterManifestUrl = `${repositoryUrl}/releases/latest/download/latest.json`;
const productDescription =
  "Model endpoint management and routing for coding assistants";

const releaseWorkflow = read(".github/workflows/release.yml");
const panicHook = read("src-tauri/src/panic_hook.rs");
const cargoManifest = read("src-tauri/Cargo.toml");
const packageManifest = JSON.parse(read("package.json")) as {
  description: string;
};
const tauriConfig = JSON.parse(read("src-tauri/tauri.conf.json")) as {
  identifier: string;
  mainBinaryName?: string;
  bundle: { createUpdaterArtifacts: boolean };
  plugins: { updater: { endpoints: string[]; pubkey: string } };
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
const repositoryLinkFiles = [
  releaseWorkflow,
  flatpakMetainfo,
  read("src/App.tsx"),
  read("src/components/DatabaseUpgrade.tsx"),
  read("src/components/settings/AboutSection.tsx"),
  read("src-tauri/src/commands/misc.rs"),
  read("src-tauri/src/tray.rs"),
];

describe("release packaging metadata", () => {
  it("uses Nexus Composer in crash-log output", () => {
    expect(panicHook).toContain("[Nexus Composer] Crash log saved");
    expect(panicHook).not.toContain("[CC-Switch] Crash log saved");
  });

  it("packages Nexus Composer artifacts with the configured executable", () => {
    const cargoPackageName = cargoManifest.match(
      /^\[package\][\s\S]*?^name\s*=\s*"([^"]+)"/m,
    )?.[1];
    const binaryName = tauriConfig.mainBinaryName ?? cargoPackageName;

    expectContainsAll(releaseWorkflow, [
      "Nexus-Composer-${VERSION}-macOS.tar.gz",
      "Nexus-Composer-${VERSION}-macOS.zip",
      "Nexus-Composer-${VERSION}-macOS.dmg",
      "Nexus-Composer-$VERSION-Windows$assetSuffix.msi",
      "Nexus-Composer-$VERSION-Windows$assetSuffix-Portable.zip",
      "Nexus-Composer-${VERSION}-Linux-${ARCH}.AppImage",
      "Nexus-Composer-${VERSION}-Linux-${ARCH}.deb",
      "Nexus-Composer-${VERSION}-Linux-${ARCH}.rpm",
      "Nexus Composer.app",
      `${binaryName}.exe`,
    ]);
    expect(cargoPackageName).toBe("nexus-composer");
    expect(binaryName).toBe("nexus-composer");
    expect(windowsConfig.app.windows[0]?.title).toBe("Nexus Composer");
    expect(releaseWorkflow).not.toMatch(/CC[ -]Switch/);
    expect(releaseWorkflow).not.toContain("cc-switch.exe");
  });

  it("connects updater artifacts and signatures to the release manifest", () => {
    expect(tauriConfig.bundle.createUpdaterArtifacts).toBe(true);
    expect(tauriConfig.plugins.updater.endpoints).toEqual([updaterManifestUrl]);
    expect(
      Buffer.from(tauriConfig.plugins.updater.pubkey, "base64").toString(
        "utf8",
      ),
    ).toMatch(/^untrusted comment: minisign public key:/);
    expectContainsAll(releaseWorkflow, [
      "Prepare Tauri signing key",
      "TAURI_SIGNING_PRIVATE_KEY",
      'NEW_TAR_GZ="Nexus-Composer-${VERSION}-macOS.tar.gz"',
      'cp "$TAR_GZ" "release-assets/$NEW_TAR_GZ"',
      'cp "$TAR_GZ.sig" "release-assets/$NEW_TAR_GZ.sig"',
      '$dest = "Nexus-Composer-$VERSION-Windows$assetSuffix.msi"',
      'Copy-Item $sigPath (Join-Path release-assets ("$dest.sig"))',
      'NEW_APPIMAGE="Nexus-Composer-${VERSION}-Linux-${ARCH}.AppImage"',
      'cp "$APPIMAGE.sig" "release-assets/$NEW_APPIMAGE.sig"',
      "Collect Signatures",
      "assemble-latest-json:",
      "Generate latest.json",
      "dl/*.sig",
      "base=${sig%.sig}",
      'fname=$(basename "$base")',
      'url="$base_url/$fname"',
      'sig_content=$(cat "$sig")',
      "prerelease: ${{ contains(github.ref_name, '-') }}",
      "latest.json --clobber",
    ]);
    expect(releaseWorkflow).not.toContain("prerelease: true");
  });

  it("uses the Tauri application ID throughout Flatpak metadata", () => {
    expect(tauriConfig.identifier).toBe("com.nexuscomposer.desktop");
    for (const contents of flatpakFiles) {
      expect(contents).toContain(tauriConfig.identifier);
      expect(contents).not.toContain("com.nexus-composer.desktop");
    }
    expect(flatpakReadme).toContain(`${tauriConfig.identifier}.yml`);
    expect(flatpakDesktop).toContain(`Icon=${tauriConfig.identifier}`);
    expectContainsAll(flatpakMetainfo, [
      `<id>${tauriConfig.identifier}</id>`,
      `<launchable type="desktop-id">${tauriConfig.identifier}.desktop</launchable>`,
    ]);
    expectContainsAll(flatpakManifest, [
      `id: ${tauriConfig.identifier}`,
      `${tauriConfig.identifier}.desktop`,
      `${tauriConfig.identifier}.metainfo.xml`,
    ]);
    expect(flatpakMetainfo).toContain(repositoryUrl);
  });

  it("uses the Nexus repository and generic product description", () => {
    expect(cargoManifest).toContain(`repository = "${repositoryUrl}"`);
    expect(cargoManifest).toContain(`description = "${productDescription}"`);
    expect(packageManifest.description).toBe(productDescription);
    expect(flatpakMetainfo).toContain(
      `<summary>${productDescription}</summary>`,
    );
    expect(flatpakDesktop).toContain(`Comment=${productDescription}`);

    for (const contents of repositoryLinkFiles) {
      expect(contents).toContain(repositoryUrl);
      expect(contents).not.toContain("github.com/farion1231/cc-switch");
    }
  });
});
