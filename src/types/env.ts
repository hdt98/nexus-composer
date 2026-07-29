/**
 * Types for environment-variable conflict detection.
 */

/**
 * Environment-variable conflict details.
 */
export interface EnvConflict {
  /** Environment-variable name. */
  varName: string;
  /** Environment-variable value. */
  varValue: string;
  /** Source type: "system" for system variables, "file" for configuration files. */
  sourceType: "system" | "file";
  /** Source path, either a registry path or a file path with line number. */
  sourcePath: string;
}

/**
 * Backup information.
 */
export interface BackupInfo {
  /** Backup file path. */
  backupPath: string;
  /** Backup timestamp. */
  timestamp: string;
  /** Environment-variable conflicts included in the backup. */
  conflicts: EnvConflict[];
}
