import { invoke } from "@tauri-apps/api/core";
import type { EnvConflict, BackupInfo } from "@/types/env";

/**
 * Environment-variable management API.
 */

/**
 * Checks environment-variable conflicts for an application.
 * @param appType Application type ("claude" | "codex" | "gemini").
 * @returns A list of environment-variable conflicts.
 */
export async function checkEnvConflicts(
  appType: string,
): Promise<EnvConflict[]> {
  return invoke<EnvConflict[]>("check_env_conflicts", { app: appType });
}

/**
 * Deletes specified environment variables after creating a backup.
 * @param conflicts Environment-variable conflicts to delete.
 * @returns Backup information.
 */
export async function deleteEnvVars(
  conflicts: EnvConflict[],
): Promise<BackupInfo> {
  return invoke<BackupInfo>("delete_env_vars", { conflicts });
}

/**
 * Restores environment variables from a backup file.
 * @param backupPath Path to the backup file.
 */
export async function restoreEnvBackup(backupPath: string): Promise<void> {
  return invoke<void>("restore_env_backup", { backupPath });
}

/**
 * Checks environment-variable conflicts for all applications.
 * @returns Conflicts grouped by application type.
 */
export async function checkAllEnvConflicts(): Promise<
  Record<string, EnvConflict[]>
> {
  const apps = ["claude", "codex", "gemini"];
  const results: Record<string, EnvConflict[]> = {};

  await Promise.all(
    apps.map(async (app) => {
      try {
        results[app] = await checkEnvConflicts(app);
      } catch (error) {
        console.error(`Failed to check ${app} environment variables:`, error);
        results[app] = [];
      }
    }),
  );

  return results;
}
