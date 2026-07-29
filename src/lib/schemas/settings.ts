import { z } from "zod";

const directorySchema = z
  .string()
  .trim()
  .min(1, "Path cannot be empty")
  .optional()
  .or(z.literal(""));

export const settingsSchema = z.object({
  // Device-level UI settings.
  showInTray: z.boolean(),
  minimizeToTrayOnClose: z.boolean(),
  enableClaudePluginIntegration: z.boolean().optional(),
  skipClaudeOnboarding: z.boolean().optional(),
  launchOnStartup: z.boolean().optional(),
  enableLocalProxy: z.boolean().optional(),
  preserveCodexOfficialAuthOnSwitch: z.boolean().optional(),
  unifyCodexSessionHistory: z.boolean().optional(),
  language: z.enum(["en", "vi"]).optional(),

  // Device-level directory overrides.
  claudeConfigDir: directorySchema.nullable().optional(),
  codexConfigDir: directorySchema.nullable().optional(),
  geminiConfigDir: directorySchema.nullable().optional(),
  opencodeConfigDir: directorySchema.nullable().optional(),
  openclawConfigDir: directorySchema.nullable().optional(),

  // Current device-level provider IDs.
  currentProviderClaude: z.string().optional(),
  currentProviderClaudeDesktop: z.string().optional(),
  currentProviderCodex: z.string().optional(),
  currentProviderGemini: z.string().optional(),

  // Skill synchronization settings.
  skillSyncMethod: z.enum(["auto", "symlink", "copy"]).optional(),
  skillStorageLocation: z.enum(["cc_switch", "unified"]).optional(),

  // WebDAV v2 synchronization settings are saved through a dedicated command;
  // this schema is used only for reading.
  webdavSync: z
    .object({
      enabled: z.boolean().optional(),
      autoSync: z.boolean().optional(),
      baseUrl: z.string().trim().optional().or(z.literal("")),
      username: z.string().trim().optional().or(z.literal("")),
      password: z.string().optional(),
      remoteRoot: z.string().trim().optional().or(z.literal("")),
      profile: z.string().trim().optional().or(z.literal("")),
      status: z
        .object({
          lastSyncAt: z.number().nullable().optional(),
          lastError: z.string().nullable().optional(),
          lastErrorSource: z.string().nullable().optional(),
          lastRemoteEtag: z.string().nullable().optional(),
          lastLocalManifestHash: z.string().nullable().optional(),
          lastRemoteManifestHash: z.string().nullable().optional(),
        })
        .optional(),
    })
    .optional(),

  // Local automatic-migration state maintained by the backend. Frontend values
  // are ignored when saving and exposed only for display.
  localMigrations: z
    .object({
      codexThirdPartyHistoryProviderBucketV1: z
        .object({
          completedAt: z.string(),
          targetProviderId: z.string(),
          sourceProviderIds: z.array(z.string()).optional(),
          migratedJsonlFiles: z.number().optional(),
          migratedStateRows: z.number().optional(),
        })
        .optional(),
    })
    .optional(),
});

export type SettingsFormData = z.infer<typeof settingsSchema>;
