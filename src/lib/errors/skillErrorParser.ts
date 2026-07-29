import { TFunction } from "i18next";

/**
 * Structured skill error.
 */
export interface SkillError {
  code: string;
  context: Record<string, string>;
  suggestion?: string;
}

/**
 * Attempts to parse an error string returned by the backend.
 * Returns a structured error for valid JSON, or null otherwise.
 */
export function parseSkillError(errorString: string): SkillError | null {
  try {
    const parsed = JSON.parse(errorString);
    if (parsed.code && parsed.context) {
      return parsed as SkillError;
    }
  } catch {
    // The error is not valid JSON.
  }
  return null;
}

/**
 * Maps an error code to an i18n key.
 */
function getErrorI18nKey(code: string): string {
  const mapping: Record<string, string> = {
    SKILL_NOT_FOUND: "skills.error.skillNotFound",
    MISSING_REPO_INFO: "skills.error.missingRepoInfo",
    DOWNLOAD_TIMEOUT: "skills.error.downloadTimeout",
    DOWNLOAD_FAILED: "skills.error.downloadFailed",
    SKILL_DIR_NOT_FOUND: "skills.error.skillDirNotFound",
    SKILL_DIRECTORY_CONFLICT: "skills.error.directoryConflict",
    EMPTY_ARCHIVE: "skills.error.emptyArchive",
    GET_HOME_DIR_FAILED: "skills.error.getHomeDirFailed",
    NO_SKILLS_IN_ZIP: "skills.error.noSkillsInZip",
  };

  return mapping[code] || "skills.error.unknownError";
}

/**
 * Maps a suggestion code to an i18n key.
 */
function getSuggestionI18nKey(suggestion: string): string {
  const mapping: Record<string, string> = {
    checkNetwork: "skills.error.suggestion.checkNetwork",
    checkProxy: "skills.error.suggestion.checkProxy",
    retryLater: "skills.error.suggestion.retryLater",
    checkRepoUrl: "skills.error.suggestion.checkRepoUrl",
    checkPermission: "skills.error.suggestion.checkPermission",
    uninstallFirst: "skills.error.suggestion.uninstallFirst",
    checkZipContent: "skills.error.suggestion.checkZipContent",
    http403: "skills.error.http403",
    http404: "skills.error.http404",
    http429: "skills.error.http429",
  };

  return mapping[suggestion] || suggestion;
}

/**
 * Formats a skill error as a user-friendly message.
 * @param errorString Error string returned by the backend.
 * @param t i18next translation function.
 * @param defaultTitle i18n key for the default title, such as
 * "skills.installFailed".
 * @returns An object containing the title and description.
 */
export function formatSkillError(
  errorString: string,
  t: TFunction,
  defaultTitle: string = "skills.installFailed",
): { title: string; description: string } {
  const parsedError = parseSkillError(errorString);

  if (!parsedError) {
    // Return the original string when the error is not structured.
    return {
      title: t(defaultTitle),
      description: errorString || t("common.error"),
    };
  }

  const { code, context, suggestion } = parsedError;

  // Resolve the error message's i18n key.
  const errorKey = getErrorI18nKey(code);

  // Build the description from the error message and optional suggestion.
  let description = t(errorKey, context);

  // Append a suggestion when one is available.
  if (suggestion) {
    const suggestionKey = getSuggestionI18nKey(suggestion);
    const suggestionText = t(suggestionKey);
    description += `\n\n${suggestionText}`;
  }

  return {
    title: t(defaultTitle),
    description,
  };
}
