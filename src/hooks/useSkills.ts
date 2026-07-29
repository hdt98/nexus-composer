import {
  useMutation,
  useQuery,
  useQueryClient,
  keepPreviousData,
} from "@tanstack/react-query";
import {
  skillsApi,
  type SkillBackupEntry,
  type DiscoverableSkill,
  type ImportSkillSelection,
  type InstalledSkill,
  type SkillUpdateInfo,
  type SkillsShSearchResult,
} from "@/lib/api/skills";
import type { AppId } from "@/lib/api/types";
import { mergeImportedSkills } from "@/hooks/useSkills.helpers";

/**
 * Query all installed skills.
 * staleTime: Infinity and placeholderData: keepPreviousData reuse cached data
 * on initial entry and fetch again only when explicitly refreshed.
 */
export function useInstalledSkills() {
  return useQuery({
    queryKey: ["skills", "installed"],
    queryFn: () => skillsApi.getInstalled(),
    staleTime: Infinity,
    placeholderData: keepPreviousData,
  });
}

export function useSkillBackups() {
  return useQuery({
    queryKey: ["skills", "backups"],
    queryFn: () => skillsApi.getBackups(),
    enabled: false,
  });
}

export function useDeleteSkillBackup() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (backupId: string) => skillsApi.deleteBackup(backupId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["skills", "backups"] });
    },
  });
}

/**
 * Discover installable skills from repositories.
 * staleTime: Infinity and placeholderData: keepPreviousData reuse cached data
 * on initial entry and fetch again only when explicitly refreshed.
 */
export function useDiscoverableSkills() {
  return useQuery({
    queryKey: ["skills", "discoverable"],
    queryFn: () => skillsApi.discoverAvailable(),
    staleTime: Infinity,
    placeholderData: keepPreviousData,
  });
}

/**
 * Install a skill and update the cache directly without reloading or refetching.
 */
export function useInstallSkill() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      skill,
      currentApp,
    }: {
      skill: DiscoverableSkill;
      currentApp: AppId;
    }) => skillsApi.installUnified(skill, currentApp),
    onSuccess: (installedSkill, _vars, _ctx) => {
      const { skill } = _vars;
      // Update the installed cache directly.
      queryClient.setQueryData<InstalledSkill[]>(
        ["skills", "installed"],
        (oldData) => {
          if (!oldData) return [installedSkill];
          return [...oldData, installedSkill];
        },
      );

      // Update the matching skill's installation state in the discoverable cache.
      const installName =
        skill.directory.split(/[/\\]/).pop()?.toLowerCase() ||
        skill.directory.toLowerCase();
      const skillKey = `${installName}:${skill.repoOwner.toLowerCase()}:${skill.repoName.toLowerCase()}`;

      queryClient.setQueryData<DiscoverableSkill[]>(
        ["skills", "discoverable"],
        (oldData) => {
          if (!oldData) return oldData;
          return oldData.map((s) => {
            if (s.key === skillKey) {
              return { ...s, installed: true };
            }
            return s;
          });
        },
      );
    },
  });
}

/**
 * Uninstall a skill and update the cache directly without reloading or refetching.
 */
export function useUninstallSkill() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, skillKey }: { id: string; skillKey: string }) =>
      skillsApi
        .uninstallUnified(id)
        .then((result) => ({ ...result, skillKey })),
    onSuccess: ({ skillKey }, _vars) => {
      // Remove the skill directly from the installed cache.
      queryClient.setQueryData<InstalledSkill[]>(
        ["skills", "installed"],
        (oldData) => {
          if (!oldData) return oldData;
          return oldData.filter((s) => s.id !== _vars.id);
        },
      );

      // Update the matching skill's installation state in the discoverable cache.
      queryClient.setQueryData<DiscoverableSkill[]>(
        ["skills", "discoverable"],
        (oldData) => {
          if (!oldData) return oldData;
          return oldData.map((s) => {
            if (s.key === skillKey) {
              return { ...s, installed: false };
            }
            return s;
          });
        },
      );
    },
  });
}

export function useRestoreSkillBackup() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      backupId,
      currentApp,
    }: {
      backupId: string;
      currentApp: AppId;
    }) => skillsApi.restoreBackup(backupId, currentApp),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["skills", "installed"] });
      queryClient.invalidateQueries({ queryKey: ["skills", "backups"] });
    },
  });
}

/**
 * Toggle a skill for a specific application.
 */
export function useToggleSkillApp() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      id,
      app,
      enabled,
    }: {
      id: string;
      app: AppId;
      enabled: boolean;
    }) => skillsApi.toggleApp(id, app, enabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["skills", "installed"] });
    },
  });
}

/**
 * Scan for unmanaged skills.
 *
 * - Passing { enabled: true } when the Skills panel mounts performs one silent
 *   scan on entry and reuses the result for 30 seconds to avoid repeated disk I/O.
 * - The default enabled: false only subscribes to the shared cache, such as for
 *   the import button's status indicator, without starting a scan. Both modes
 *   share one queryKey, so the indicator updates after the panel scan completes.
 */
export function useScanUnmanagedSkills(options?: { enabled?: boolean }) {
  return useQuery({
    queryKey: ["skills", "unmanaged"],
    queryFn: () => skillsApi.scanUnmanaged(),
    enabled: options?.enabled ?? false,
    staleTime: 30 * 1000,
    placeholderData: keepPreviousData,
  });
}

/**
 * Import skills from application directories and update the cache directly
 * without reloading or refetching.
 */
export function useImportSkillsFromApps() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (imports: ImportSkillSelection[]) =>
      skillsApi.importFromApps(imports),
    onSuccess: (importedSkills) => {
      // Update the installed cache directly.
      queryClient.setQueryData<InstalledSkill[]>(
        ["skills", "installed"],
        (oldData) => mergeImportedSkills(oldData, importedSkills),
      );
      // Refresh the unmanaged list so imported skills are removed.
      queryClient.invalidateQueries({ queryKey: ["skills", "unmanaged"] });
    },
  });
}

/**
 * Get configured skill repositories.
 */
export function useSkillRepos() {
  return useQuery({
    queryKey: ["skills", "repos"],
    queryFn: () => skillsApi.getRepos(),
  });
}

/**
 * Add a skill repository.
 */
export function useAddSkillRepo() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: skillsApi.addRepo,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["skills", "repos"] });
      queryClient.invalidateQueries({ queryKey: ["skills", "discoverable"] });
    },
  });
}

/**
 * Remove a skill repository.
 */
export function useRemoveSkillRepo() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ owner, name }: { owner: string; name: string }) =>
      skillsApi.removeRepo(owner, name),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["skills", "repos"] });
      queryClient.invalidateQueries({ queryKey: ["skills", "discoverable"] });
    },
  });
}

/**
 * Install skills from a ZIP file and update the cache directly without
 * reloading or refetching.
 */
export function useInstallSkillsFromZip() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      filePath,
      currentApp,
    }: {
      filePath: string;
      currentApp: AppId;
    }) => skillsApi.installFromZip(filePath, currentApp),
    onSuccess: (installedSkills) => {
      // Update the installed cache directly.
      queryClient.setQueryData<InstalledSkill[]>(
        ["skills", "installed"],
        (oldData) => {
          if (!oldData) return installedSkills;
          return [...oldData, ...installedSkills];
        },
      );
    },
  });
}

// Update detection.

/**
 * Check for skill updates on demand.
 */
export function useCheckSkillUpdates() {
  return useQuery({
    queryKey: ["skills", "updates"],
    queryFn: () => skillsApi.checkUpdates(),
    enabled: false,
    staleTime: 5 * 60 * 1000,
  });
}

/**
 * Update a single skill.
 */
export function useUpdateSkill() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => skillsApi.updateSkill(id),
    onSuccess: (updatedSkill) => {
      queryClient.setQueryData<InstalledSkill[]>(
        ["skills", "installed"],
        (oldData) => {
          if (!oldData) return [updatedSkill];
          return oldData.map((s) =>
            s.id === updatedSkill.id ? updatedSkill : s,
          );
        },
      );
      queryClient.setQueryData<SkillUpdateInfo[]>(
        ["skills", "updates"],
        (oldData) => {
          if (!oldData) return oldData;
          return oldData.filter((u) => u.id !== updatedSkill.id);
        },
      );
    },
  });
}

// skills.sh search.

/**
 * Search the public skills.sh directory.
 * A 300 ms stale time and keepPreviousData provide a smooth search experience.
 */
export function useSearchSkillsSh(
  query: string,
  limit: number,
  offset: number,
) {
  return useQuery({
    queryKey: ["skills", "skillssh", query, limit, offset],
    queryFn: () => skillsApi.searchSkillsSh(query, limit, offset),
    enabled: query.length >= 2,
    staleTime: 5 * 60 * 1000,
    placeholderData: keepPreviousData,
  });
}

// Helper types.

export type {
  InstalledSkill,
  DiscoverableSkill,
  ImportSkillSelection,
  SkillBackupEntry,
  SkillUpdateInfo,
  SkillsShSearchResult,
  AppId,
};
