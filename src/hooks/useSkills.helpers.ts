import type { InstalledSkill } from "@/lib/api/skills";

/**
 * Merges imported results into the installed cache and deduplicates by ID.
 *
 * New records win when a skill appears more than once, preventing duplicates
 * after repeated mutations. An empty imported list returns the original
 * reference so React Query skips unnecessary subscriber notifications.
 */
export function mergeImportedSkills(
  existing: InstalledSkill[] | undefined,
  imported: InstalledSkill[],
): InstalledSkill[] {
  if (!existing) return imported;
  if (imported.length === 0) return existing;
  const importedIds = new Set(imported.map((s) => s.id));
  const preserved = existing.filter((s) => !importedIds.has(s.id));
  return [...preserved, ...imported];
}
