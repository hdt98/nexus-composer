//! Rollback-safe application storage resolution and path migration.
//!
//! Physical storage migration runs before SQLite schema migration. It inventories
//! every known database candidate without writing, chooses one source by a stable
//! content-based policy, preserves conflicts, and only then promotes a verified
//! snapshot to the canonical Nexus Composer path.

use crate::config::{
    self, APP_CONFIG_DIR_NAME, APP_DATABASE_FILENAME, LEGACY_APP_CONFIG_DIR_NAME,
    LEGACY_APP_DATABASE_FILENAME,
};
use crate::error::AppError;
use chrono::Utc;
use rusqlite::{backup::Backup, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const MIGRATION_FORMAT_VERSION: u32 = 1;
const OWNED_INVENTORY_VERSION: u32 = 1;
const COMPLETION_MARKER: &str = ".storage-migration-v1.complete.json";
const IN_PROGRESS_MARKER: &str = ".storage-migration-v1.in-progress.json";
const REPORT_FILENAME: &str = "storage-migration-report-v1.json";
const MIGRATION_BACKUP_DIR: &str = "storage-migration-backups";
const OWNED_FILES: &[&str] = &[
    "config.json",
    "config.json.bak",
    "settings.json",
    "skills.json",
    "crash.log",
    "codex_official_auth_snapshot.json",
    "codex_oauth_auth.json",
    "copilot_auth.json",
];
const OWNED_DIRS: &[&str] = &["skills", "skill-backups", "backups", "logs"];
const MEANINGFUL_TABLES: &[&str] = &[
    "providers",
    "mcp_servers",
    "prompts",
    "skills",
    "settings",
    "proxy_config",
    "usage_daily_rollups",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageMigrationReport {
    pub format_version: u32,
    pub owned_inventory_version: u32,
    pub migration_id: String,
    pub completed_at: String,
    pub selected_database: Option<String>,
    pub selected_user_version: Option<i32>,
    pub promoted_database: bool,
    pub interrupted_previous_attempt: bool,
    pub candidates: Vec<DatabaseCandidateReport>,
    pub migrated_entries: Vec<String>,
    pub conflicting_entries: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseCandidateReport {
    pub label: String,
    pub exists: bool,
    pub size_bytes: Option<u64>,
    pub user_version: Option<i32>,
    pub integrity: String,
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone)]
pub enum StoragePreparation {
    Ready {
        database_path: PathBuf,
        report_path: Option<PathBuf>,
        migrated: bool,
    },
    DatabaseTooNew {
        path: PathBuf,
        version: i32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletionMarker {
    format_version: u32,
    #[serde(default)]
    owned_inventory_version: u32,
    completed_at: String,
    database_filename: String,
    selected_database: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InProgressMarker {
    format_version: u32,
    migration_id: String,
    started_at: String,
    selected_database: Option<String>,
    stage_directory: String,
}

#[derive(Debug, Clone)]
struct Candidate {
    label: String,
    path: PathBuf,
    scope_priority: u8,
    path_priority: u8,
}

#[derive(Debug, Clone)]
struct InspectedCandidate {
    candidate: Candidate,
    exists: bool,
    valid: bool,
    size_bytes: Option<u64>,
    user_version: Option<i32>,
    meaningful_rows: u64,
    total_rows: u64,
    integrity: String,
    fingerprint: Option<String>,
}

impl InspectedCandidate {
    fn report(&self) -> DatabaseCandidateReport {
        DatabaseCandidateReport {
            label: self.candidate.label.clone(),
            exists: self.exists,
            size_bytes: self.size_bytes,
            user_version: self.user_version,
            integrity: self.integrity.clone(),
            fingerprint: self.fingerprint.clone(),
        }
    }

    fn selection_key(&self) -> (u8, bool, i32, u64, u64, u8) {
        // An explicit storage override remains authoritative. Otherwise prefer
        // real user state over a newer-but-empty database, then use schema and
        // row counts as deterministic tie-breakers. Modification time is never used.
        (
            self.candidate.scope_priority,
            self.meaningful_rows > 0,
            self.user_version.unwrap_or_default(),
            self.meaningful_rows,
            self.total_rows,
            self.candidate.path_priority,
        )
    }
}

#[derive(Debug, Clone)]
struct StorageLayout {
    target_dir: PathBuf,
    database_path: PathBuf,
    candidates: Vec<Candidate>,
}

#[derive(Debug, Default)]
struct CompletionOutcome {
    migration_id: Option<String>,
    promoted_database: bool,
    interrupted_previous_attempt: bool,
    migrated_entries: Vec<String>,
    conflicting_entries: Vec<String>,
    warnings: Vec<String>,
}

/// Prepare the canonical app-data layout before settings, logs, or SQLite are opened.
pub fn prepare_app_storage(supported_schema_version: i32) -> Result<StoragePreparation, AppError> {
    let target_dir = config::get_app_config_dir();
    let explicit_override = crate::app_store::get_app_config_dir_override().is_some();
    let layout = StorageLayout::for_runtime(target_dir, explicit_override);
    prepare_layout(&layout, supported_schema_version)
}

impl StorageLayout {
    fn for_runtime(target_dir: PathBuf, explicit_override: bool) -> Self {
        let home = config::get_home_dir();
        let default_dir = home.join(APP_CONFIG_DIR_NAME);
        let legacy_dir = home.join(LEGACY_APP_CONFIG_DIR_NAME);
        let mut candidates = Vec::new();

        let target_scope = u8::from(explicit_override);
        push_candidate(
            &mut candidates,
            "target-canonical",
            target_dir.join(APP_DATABASE_FILENAME),
            target_scope,
            100,
        );
        push_candidate(
            &mut candidates,
            "target-legacy-filename",
            target_dir.join(LEGACY_APP_DATABASE_FILENAME),
            target_scope,
            90,
        );
        push_candidate(
            &mut candidates,
            "default-canonical",
            default_dir.join(APP_DATABASE_FILENAME),
            0,
            70,
        );
        push_candidate(
            &mut candidates,
            "default-legacy-filename",
            default_dir.join(LEGACY_APP_DATABASE_FILENAME),
            0,
            60,
        );
        push_candidate(
            &mut candidates,
            "legacy-new-filename",
            legacy_dir.join(APP_DATABASE_FILENAME),
            0,
            50,
        );
        push_candidate(
            &mut candidates,
            "legacy-cc-switch",
            legacy_dir.join(LEGACY_APP_DATABASE_FILENAME),
            0,
            40,
        );

        #[cfg(windows)]
        if let Some(home_env) = std::env::var_os("HOME") {
            if !home_env.is_empty() {
                let env_legacy = PathBuf::from(home_env)
                    .join(LEGACY_APP_CONFIG_DIR_NAME)
                    .join(LEGACY_APP_DATABASE_FILENAME);
                push_candidate(&mut candidates, "windows-home-legacy", env_legacy, 0, 10);
            }
        }

        Self {
            database_path: target_dir.join(APP_DATABASE_FILENAME),
            target_dir,
            candidates,
        }
    }

    #[cfg(test)]
    fn for_test(home: &Path, target_dir: PathBuf, explicit_override: bool) -> Self {
        let default_dir = home.join(APP_CONFIG_DIR_NAME);
        let legacy_dir = home.join(LEGACY_APP_CONFIG_DIR_NAME);
        let target_scope = u8::from(explicit_override);
        let mut candidates = Vec::new();
        push_candidate(
            &mut candidates,
            "target-canonical",
            target_dir.join(APP_DATABASE_FILENAME),
            target_scope,
            100,
        );
        push_candidate(
            &mut candidates,
            "target-legacy-filename",
            target_dir.join(LEGACY_APP_DATABASE_FILENAME),
            target_scope,
            90,
        );
        push_candidate(
            &mut candidates,
            "default-canonical",
            default_dir.join(APP_DATABASE_FILENAME),
            0,
            70,
        );
        push_candidate(
            &mut candidates,
            "default-legacy-filename",
            default_dir.join(LEGACY_APP_DATABASE_FILENAME),
            0,
            60,
        );
        push_candidate(
            &mut candidates,
            "legacy-new-filename",
            legacy_dir.join(APP_DATABASE_FILENAME),
            0,
            50,
        );
        push_candidate(
            &mut candidates,
            "legacy-cc-switch",
            legacy_dir.join(LEGACY_APP_DATABASE_FILENAME),
            0,
            40,
        );
        Self {
            database_path: target_dir.join(APP_DATABASE_FILENAME),
            target_dir,
            candidates,
        }
    }
}

fn push_candidate(
    candidates: &mut Vec<Candidate>,
    label: &str,
    path: PathBuf,
    scope_priority: u8,
    path_priority: u8,
) {
    if candidates.iter().any(|candidate| candidate.path == path) {
        return;
    }
    candidates.push(Candidate {
        label: label.to_string(),
        path,
        scope_priority,
        path_priority,
    });
}

fn prepare_layout(
    layout: &StorageLayout,
    supported_schema_version: i32,
) -> Result<StoragePreparation, AppError> {
    // Everything through selection is read-only. Do not create the target directory
    // until all candidates have been inspected and the selected version is supported.
    let inspected = layout
        .candidates
        .iter()
        .cloned()
        .map(inspect_candidate)
        .collect::<Vec<_>>();
    let marker = read_completion_marker(&layout.target_dir.join(COMPLETION_MARKER));
    let selected_index = select_candidate(layout, &inspected, marker.as_ref())?;

    if let Some(index) = selected_index {
        let selected = &inspected[index];
        if let Some(version) = selected.user_version {
            if version > supported_schema_version {
                return Ok(StoragePreparation::DatabaseTooNew {
                    path: selected.candidate.path.clone(),
                    version,
                });
            }
        }
    }

    let canonical_valid = inspected
        .iter()
        .any(|candidate| candidate.candidate.path == layout.database_path && candidate.valid);
    let inventory_is_current = marker
        .as_ref()
        .is_some_and(|marker| marker.owned_inventory_version == OWNED_INVENTORY_VERSION);
    if inventory_is_current && canonical_valid {
        // A completed migration establishes canonical continuity. Legacy copies
        // remain preserved but are no longer candidates for automatic promotion.
        let stale_state = layout.target_dir.join(IN_PROGRESS_MARKER);
        cleanup_interrupted_state(&layout.target_dir, &stale_state);
        return Ok(StoragePreparation::Ready {
            database_path: layout.database_path.clone(),
            report_path: existing_report_path(layout),
            migrated: false,
        });
    }

    let source_root = selected_index.and_then(|index| {
        inspected[index]
            .candidate
            .path
            .parent()
            .map(Path::to_path_buf)
    });
    let roots = candidate_roots(&inspected);
    let has_owned_data_to_migrate =
        owned_data_needs_migration(&layout.target_dir, source_root.as_deref(), &roots);
    let selected_is_canonical =
        selected_index.is_none_or(|index| inspected[index].candidate.path == layout.database_path);
    let has_database_conflict = inspected.iter().filter(|item| item.exists).count() > 1;
    let needs_migration = marker.is_none()
        && (!selected_is_canonical || has_database_conflict || has_owned_data_to_migrate);

    if marker.is_none() && !needs_migration && selected_index.is_none() {
        // Fresh install: establish the canonical layout without manufacturing a
        // database. Database::init remains the sole owner of schema creation.
        fs::create_dir_all(&layout.target_dir)
            .map_err(|error| AppError::io(&layout.target_dir, error))?;
        restrict_directory_permissions(&layout.target_dir);
        let report =
            write_completion_artifacts(layout, &inspected, None, CompletionOutcome::default())?;
        return Ok(StoragePreparation::Ready {
            database_path: layout.database_path.clone(),
            report_path: Some(report),
            migrated: false,
        });
    }

    execute_migration(layout, &inspected, selected_index, source_root, roots)
}

fn select_candidate(
    layout: &StorageLayout,
    inspected: &[InspectedCandidate],
    marker: Option<&CompletionMarker>,
) -> Result<Option<usize>, AppError> {
    if marker.is_some() {
        if let Some((index, _)) = inspected
            .iter()
            .enumerate()
            .find(|(_, item)| item.candidate.path == layout.database_path && item.valid)
        {
            return Ok(Some(index));
        }
    }

    let selected = inspected
        .iter()
        .enumerate()
        .filter(|(_, item)| item.valid)
        .max_by_key(|(_, item)| item.selection_key())
        .map(|(index, _)| index);

    if selected.is_none() && inspected.iter().any(|item| item.exists) {
        let labels = inspected
            .iter()
            .filter(|item| item.exists)
            .map(|item| item.candidate.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::Database(format!(
            "No usable database candidate passed SQLite integrity checks ({labels}). No files were changed."
        )));
    }

    Ok(selected)
}

fn inspect_candidate(candidate: Candidate) -> InspectedCandidate {
    if !candidate.path.is_file() {
        return InspectedCandidate {
            candidate,
            exists: false,
            valid: false,
            size_bytes: None,
            user_version: None,
            meaningful_rows: 0,
            total_rows: 0,
            integrity: "missing".to_string(),
            fingerprint: None,
        };
    }

    let size_bytes = fs::metadata(&candidate.path).ok().map(|meta| meta.len());
    let fingerprint = fingerprint_database_files(&candidate.path).ok();
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = match Connection::open_with_flags(&candidate.path, flags) {
        Ok(connection) => connection,
        Err(_) => {
            return InspectedCandidate {
                candidate,
                exists: true,
                valid: false,
                size_bytes,
                user_version: None,
                meaningful_rows: 0,
                total_rows: 0,
                integrity: "open-failed".to_string(),
                fingerprint,
            };
        }
    };

    let user_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
        .ok();
    let integrity_ok = sqlite_integrity_ok(&connection).unwrap_or(false);
    let meaningful_rows = if integrity_ok {
        MEANINGFUL_TABLES
            .iter()
            .filter_map(|table| count_table_rows(&connection, table).ok().flatten())
            .sum()
    } else {
        0
    };
    let total_rows = if integrity_ok {
        count_all_user_rows(&connection).unwrap_or_default()
    } else {
        0
    };

    InspectedCandidate {
        candidate,
        exists: true,
        valid: integrity_ok && user_version.is_some(),
        size_bytes,
        user_version,
        meaningful_rows,
        total_rows,
        integrity: if integrity_ok {
            "ok".to_string()
        } else {
            "integrity-failed".to_string()
        },
        fingerprint,
    }
}

fn sqlite_integrity_ok(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let mut rows = statement.query([])?;
    let mut saw_row = false;
    while let Some(row) = rows.next()? {
        saw_row = true;
        let result: String = row.get(0)?;
        if result != "ok" {
            return Ok(false);
        }
    }
    Ok(saw_row)
}

fn count_table_rows(connection: &Connection, table: &str) -> Result<Option<u64>, rusqlite::Error> {
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let quoted = table.replace('"', "\"\"");
    connection
        .query_row(&format!("SELECT COUNT(*) FROM \"{quoted}\""), [], |row| {
            row.get::<_, u64>(0)
        })
        .map(Some)
}

fn count_all_user_rows(connection: &Connection) -> Result<u64, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut total = 0u64;
    for table in tables {
        total = total.saturating_add(count_table_rows(connection, &table)?.unwrap_or_default());
    }
    Ok(total)
}

fn fingerprint_database_files(path: &Path) -> Result<String, AppError> {
    let mut digest = Sha256::new();
    hash_file_into(&mut digest, path)?;
    let wal = PathBuf::from(format!("{}-wal", path.to_string_lossy()));
    if wal.is_file() {
        hash_file_into(&mut digest, &wal)?;
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn hash_file_into(digest: &mut Sha256, path: &Path) -> Result<(), AppError> {
    let mut file = fs::File::open(path).map_err(|error| AppError::io(path, error))?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| AppError::io(path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(())
}

fn candidate_roots(inspected: &[InspectedCandidate]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    inspected
        .iter()
        .filter_map(|item| item.candidate.path.parent().map(Path::to_path_buf))
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn owned_data_needs_migration(
    target_dir: &Path,
    preferred_root: Option<&Path>,
    roots: &[PathBuf],
) -> bool {
    OWNED_FILES.iter().chain(OWNED_DIRS).any(|entry| {
        choose_owned_source(target_dir, preferred_root, roots, entry)
            .is_some_and(|source| source != target_dir.join(entry))
    })
}

fn choose_owned_source(
    target_dir: &Path,
    preferred_root: Option<&Path>,
    roots: &[PathBuf],
    entry: &str,
) -> Option<PathBuf> {
    if let Some(root) = preferred_root {
        let source = root.join(entry);
        if source.exists() {
            return Some(source);
        }
    }
    let canonical = target_dir.join(entry);
    if canonical.exists() {
        return Some(canonical);
    }
    roots
        .iter()
        .map(|root| root.join(entry))
        .find(|path| path.exists())
}

fn execute_migration(
    layout: &StorageLayout,
    inspected: &[InspectedCandidate],
    selected_index: Option<usize>,
    source_root: Option<PathBuf>,
    roots: Vec<PathBuf>,
) -> Result<StoragePreparation, AppError> {
    fs::create_dir_all(&layout.target_dir)
        .map_err(|error| AppError::io(&layout.target_dir, error))?;
    restrict_directory_permissions(&layout.target_dir);

    let migration_id = new_migration_id();
    let backup_root = layout
        .target_dir
        .join(MIGRATION_BACKUP_DIR)
        .join(&migration_id);
    fs::create_dir_all(&backup_root).map_err(|error| AppError::io(&backup_root, error))?;
    restrict_directory_permissions(&backup_root);

    let state_path = layout.target_dir.join(IN_PROGRESS_MARKER);
    let interrupted_previous_attempt = state_path.exists();
    let previous_state = read_in_progress_marker(&state_path);
    let stage_name = format!(".storage-migration-stage-{migration_id}");
    let stage_root = layout.target_dir.join(&stage_name);
    let selected_label = selected_index.map(|index| inspected[index].candidate.label.clone());
    let state = InProgressMarker {
        format_version: MIGRATION_FORMAT_VERSION,
        migration_id: migration_id.clone(),
        started_at: Utc::now().to_rfc3339(),
        selected_database: selected_label.clone(),
        stage_directory: stage_name,
    };
    write_private_json_atomic(&state_path, &state)?;

    // Preserve exact raw database files and SQLite sidecars before promotion.
    for item in inspected.iter().filter(|item| item.exists) {
        backup_database_candidate(item, &backup_root)?;
    }

    let mut migrated_entries = Vec::new();
    let mut conflicting_entries = Vec::new();
    for entry in OWNED_FILES.iter().chain(OWNED_DIRS) {
        let Some(source) =
            choose_owned_source(&layout.target_dir, source_root.as_deref(), &roots, entry)
        else {
            continue;
        };
        let destination = layout.target_dir.join(entry);
        if source == destination {
            continue;
        }
        if destination.exists() {
            conflicting_entries.push((*entry).to_string());
        }
        stage_and_commit_owned_entry(
            &source,
            &destination,
            &stage_root.join(entry),
            &backup_root.join("app-data").join("destination").join(entry),
        )?;
        migrated_entries.push((*entry).to_string());
    }

    let mut promoted_database = false;
    if let Some(index) = selected_index {
        let selected = &inspected[index];
        if selected.candidate.path != layout.database_path {
            promote_database_snapshot(
                &selected.candidate.path,
                &layout.database_path,
                &stage_root,
                selected.user_version.unwrap_or_default(),
            )?;
            promoted_database = true;
        }
    }

    if layout.database_path.exists() {
        let verified = inspect_candidate(Candidate {
            label: "promoted-canonical".to_string(),
            path: layout.database_path.clone(),
            scope_priority: 0,
            path_priority: 0,
        });
        if !verified.valid {
            return Err(AppError::Database(
                "The promoted database failed its final SQLite integrity check; preserved backups were not removed."
                    .to_string(),
            ));
        }
    }

    let report_path = write_completion_artifacts(
        layout,
        inspected,
        selected_index,
        CompletionOutcome {
            migration_id: Some(migration_id),
            promoted_database,
            interrupted_previous_attempt,
            migrated_entries,
            conflicting_entries,
            warnings: Vec::new(),
        },
    )?;

    if stage_root.exists() {
        let _ = fs::remove_dir_all(&stage_root);
    }
    if let Some(previous_state) = previous_state.as_ref() {
        cleanup_stage_directory(&layout.target_dir, &previous_state.stage_directory);
    }
    let _ = fs::remove_file(&state_path);

    Ok(StoragePreparation::Ready {
        database_path: layout.database_path.clone(),
        report_path: Some(report_path),
        migrated: true,
    })
}

fn backup_database_candidate(
    item: &InspectedCandidate,
    backup_root: &Path,
) -> Result<(), AppError> {
    let candidate_root = backup_root.join("databases").join(&item.candidate.label);
    fs::create_dir_all(&candidate_root).map_err(|error| AppError::io(&candidate_root, error))?;
    restrict_directory_permissions(&candidate_root);
    let filename = item
        .candidate
        .path
        .file_name()
        .ok_or_else(|| AppError::Config("Invalid database candidate filename".to_string()))?;
    copy_file_preserving_permissions(&item.candidate.path, &candidate_root.join(filename))?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!(
            "{}{}",
            item.candidate.path.to_string_lossy(),
            suffix
        ));
        if sidecar.is_file() {
            let sidecar_name = sidecar
                .file_name()
                .ok_or_else(|| AppError::Config("Invalid SQLite sidecar filename".to_string()))?;
            copy_file_preserving_permissions(&sidecar, &candidate_root.join(sidecar_name))?;
        }
    }
    if item.valid {
        let snapshot_path = candidate_root.join("verified-snapshot.db");
        let source = Connection::open_with_flags(
            &item.candidate.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut destination = Connection::open(&snapshot_path)?;
        {
            let backup = Backup::new(&source, &mut destination)?;
            backup.step(-1)?;
        }
        drop(destination);
        drop(source);
        let snapshot = inspect_candidate(Candidate {
            label: "backup-snapshot".to_string(),
            path: snapshot_path,
            scope_priority: 0,
            path_priority: 0,
        });
        if !snapshot.valid || snapshot.user_version != item.user_version {
            return Err(AppError::Database(format!(
                "Verified backup failed for database candidate {}",
                item.candidate.label
            )));
        }
    }
    Ok(())
}

fn stage_and_commit_owned_entry(
    source: &Path,
    destination: &Path,
    stage: &Path,
    backup: &Path,
) -> Result<(), AppError> {
    if let Some(parent) = stage.parent() {
        fs::create_dir_all(parent).map_err(|error| AppError::io(parent, error))?;
    }
    if path_exists_no_follow(stage) {
        remove_path(stage)?;
    }

    if source.is_dir() {
        if destination.is_dir() {
            copy_tree(destination, stage, false)?;
            copy_tree(source, stage, true)?;
        } else {
            copy_tree(source, stage, true)?;
        }
    } else {
        copy_file_preserving_permissions(source, stage)?;
    }

    let had_destination = path_exists_no_follow(destination);
    if had_destination {
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent).map_err(|error| AppError::io(parent, error))?;
        }
        if path_exists_no_follow(backup) {
            remove_path(backup)?;
        }
        fs::rename(destination, backup).map_err(|error| AppError::IoContext {
            context: format!(
                "Failed to preserve conflicting app-data entry {}",
                destination.display()
            ),
            source: error,
        })?;
    }

    if let Err(error) = fs::rename(stage, destination) {
        if had_destination && path_exists_no_follow(backup) {
            let _ = fs::rename(backup, destination);
        }
        return Err(AppError::IoContext {
            context: format!("Failed to promote app-data entry {}", destination.display()),
            source: error,
        });
    }
    Ok(())
}

fn promote_database_snapshot(
    source: &Path,
    destination: &Path,
    stage_root: &Path,
    expected_version: i32,
) -> Result<(), AppError> {
    fs::create_dir_all(stage_root).map_err(|error| AppError::io(stage_root, error))?;
    let staged = stage_root.join(APP_DATABASE_FILENAME);
    if staged.exists() {
        fs::remove_file(&staged).map_err(|error| AppError::io(&staged, error))?;
    }

    let source_connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut destination_connection = Connection::open(&staged)?;
    {
        let backup = Backup::new(&source_connection, &mut destination_connection)?;
        backup.step(-1)?;
    }
    drop(destination_connection);
    drop(source_connection);

    let staged_inspection = inspect_candidate(Candidate {
        label: "staged-promotion".to_string(),
        path: staged.clone(),
        scope_priority: 0,
        path_priority: 0,
    });
    if !staged_inspection.valid || staged_inspection.user_version != Some(expected_version) {
        return Err(AppError::Database(
            "The staged database snapshot failed verification; the active database was not changed."
                .to_string(),
        ));
    }
    atomic_replace_file(&staged, destination)
}

fn copy_tree(source: &Path, destination: &Path, overwrite: bool) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(source).map_err(|error| AppError::io(source, error))?;
    if metadata.file_type().is_symlink() {
        return copy_symlink(source, destination, overwrite);
    }
    if metadata.is_file() {
        if path_exists_no_follow(destination) {
            if !overwrite {
                return Ok(());
            }
            let destination_metadata = fs::symlink_metadata(destination)
                .map_err(|error| AppError::io(destination, error))?;
            if destination_metadata.is_dir() || destination_metadata.file_type().is_symlink() {
                remove_path(destination)?;
            }
        }
        return copy_file_preserving_permissions(source, destination);
    }

    if path_exists_no_follow(destination) {
        let destination_metadata =
            fs::symlink_metadata(destination).map_err(|error| AppError::io(destination, error))?;
        if !destination_metadata.is_dir() || destination_metadata.file_type().is_symlink() {
            if !overwrite {
                return Ok(());
            }
            remove_path(destination)?;
        }
    }
    fs::create_dir_all(destination).map_err(|error| AppError::io(destination, error))?;
    for entry in fs::read_dir(source).map_err(|error| AppError::io(source, error))? {
        let entry = entry.map_err(|error| AppError::io(source, error))?;
        copy_tree(
            &entry.path(),
            &destination.join(entry.file_name()),
            overwrite,
        )?;
    }
    Ok(())
}

fn copy_file_preserving_permissions(source: &Path, destination: &Path) -> Result<(), AppError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| AppError::io(parent, error))?;
    }
    fs::copy(source, destination).map_err(|error| AppError::IoContext {
        context: format!("Failed to preserve storage file {}", source.display()),
        source: error,
    })?;
    if let Ok(metadata) = fs::metadata(source) {
        let _ = fs::set_permissions(destination, metadata.permissions());
    }
    Ok(())
}

fn copy_symlink(source: &Path, destination: &Path, overwrite: bool) -> Result<(), AppError> {
    if path_exists_no_follow(destination) {
        if !overwrite {
            return Ok(());
        }
        remove_path(destination)?;
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| AppError::io(parent, error))?;
    }
    let target = fs::read_link(source).map_err(|error| AppError::io(source, error))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, destination)
        .map_err(|error| AppError::io(destination, error))?;
    #[cfg(windows)]
    {
        let points_to_directory = fs::metadata(source)
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if points_to_directory {
            std::os::windows::fs::symlink_dir(target, destination)
                .map_err(|error| AppError::io(destination, error))?;
        } else {
            std::os::windows::fs::symlink_file(target, destination)
                .map_err(|error| AppError::io(destination, error))?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| AppError::io(path, error))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|error| AppError::io(path, error))
    } else {
        fs::remove_file(path).map_err(|error| AppError::io(path, error))
    }
}

fn path_exists_no_follow(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn write_completion_artifacts(
    layout: &StorageLayout,
    inspected: &[InspectedCandidate],
    selected_index: Option<usize>,
    outcome: CompletionOutcome,
) -> Result<PathBuf, AppError> {
    let migration_id = outcome.migration_id.unwrap_or_else(new_migration_id);
    let completed_at = Utc::now().to_rfc3339();
    let selected_database = selected_index.map(|index| inspected[index].candidate.label.clone());
    let selected_user_version = selected_index.and_then(|index| inspected[index].user_version);
    let report = StorageMigrationReport {
        format_version: MIGRATION_FORMAT_VERSION,
        owned_inventory_version: OWNED_INVENTORY_VERSION,
        migration_id,
        completed_at: completed_at.clone(),
        selected_database: selected_database.clone(),
        selected_user_version,
        promoted_database: outcome.promoted_database,
        interrupted_previous_attempt: outcome.interrupted_previous_attempt,
        candidates: inspected.iter().map(InspectedCandidate::report).collect(),
        migrated_entries: outcome.migrated_entries,
        conflicting_entries: outcome.conflicting_entries,
        warnings: outcome.warnings,
    };
    let report_path = layout.target_dir.join(REPORT_FILENAME);
    write_private_json_atomic(&report_path, &report)?;

    let marker = CompletionMarker {
        format_version: MIGRATION_FORMAT_VERSION,
        owned_inventory_version: OWNED_INVENTORY_VERSION,
        completed_at,
        database_filename: APP_DATABASE_FILENAME.to_string(),
        selected_database,
    };
    write_private_json_atomic(&layout.target_dir.join(COMPLETION_MARKER), &marker)?;
    Ok(report_path)
}

fn read_completion_marker(path: &Path) -> Option<CompletionMarker> {
    let bytes = fs::read(path).ok()?;
    let marker: CompletionMarker = serde_json::from_slice(&bytes).ok()?;
    (marker.format_version == MIGRATION_FORMAT_VERSION
        && marker.database_filename == APP_DATABASE_FILENAME)
        .then_some(marker)
}

fn read_in_progress_marker(path: &Path) -> Option<InProgressMarker> {
    let bytes = fs::read(path).ok()?;
    let marker: InProgressMarker = serde_json::from_slice(&bytes).ok()?;
    (marker.format_version == MIGRATION_FORMAT_VERSION).then_some(marker)
}

fn cleanup_interrupted_state(target_dir: &Path, state_path: &Path) {
    if let Some(state) = read_in_progress_marker(state_path) {
        cleanup_stage_directory(target_dir, &state.stage_directory);
    }
    if state_path.exists() {
        let _ = fs::remove_file(state_path);
    }
}

fn cleanup_stage_directory(target_dir: &Path, stage_directory: &str) {
    let stage = Path::new(stage_directory);
    let mut components = stage.components();
    let safe_name = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && stage_directory.starts_with(".storage-migration-stage-");
    if !safe_name {
        return;
    }

    let path = target_dir.join(stage);
    if path_exists_no_follow(&path) {
        let _ = remove_path(&path);
    }
}

fn existing_report_path(layout: &StorageLayout) -> Option<PathBuf> {
    let path = layout.target_dir.join(REPORT_FILENAME);
    path.exists().then_some(path)
}

fn write_private_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), AppError> {
    let data =
        serde_json::to_vec_pretty(value).map_err(|source| AppError::JsonSerialize { source })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| AppError::io(parent, error))?;
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        #[cfg(unix)]
        let mut file = {
            use std::fs::OpenOptions;
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)
                .map_err(|error| AppError::io(&temporary, error))?
        };
        #[cfg(not(unix))]
        let mut file =
            fs::File::create(&temporary).map_err(|error| AppError::io(&temporary, error))?;
        file.write_all(&data)
            .map_err(|error| AppError::io(&temporary, error))?;
        file.sync_all()
            .map_err(|error| AppError::io(&temporary, error))?;
    }
    atomic_replace_file(&temporary, path)
}

#[cfg(not(windows))]
fn atomic_replace_file(source: &Path, destination: &Path) -> Result<(), AppError> {
    fs::rename(source, destination).map_err(|error| AppError::IoContext {
        context: format!(
            "Atomic storage promotion failed: {} -> {}",
            source.display(),
            destination.display()
        ),
        source: error,
    })
}

#[cfg(windows)]
fn atomic_replace_file(source: &Path, destination: &Path) -> Result<(), AppError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_WRITE_THROUGH, REPLACEFILE_WRITE_THROUGH,
    };

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        if destination.exists() {
            ReplaceFileW(
                destination_wide.as_ptr(),
                source_wide.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                source_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if result == 0 {
        return Err(AppError::IoContext {
            context: format!(
                "Atomic storage promotion failed: {} -> {}",
                source.display(),
                destination.display()
            ),
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

fn restrict_directory_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
}

fn new_migration_id() -> String {
    format!(
        "{}-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_database(path: &Path, version: i32, provider_rows: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("database parent");
        }
        let connection = Connection::open(path).expect("database");
        connection
            .execute_batch(&format!(
                "PRAGMA user_version = {version};
                 CREATE TABLE providers (id TEXT PRIMARY KEY, name TEXT NOT NULL);"
            ))
            .expect("schema");
        for index in 0..provider_rows {
            connection
                .execute(
                    "INSERT INTO providers (id, name) VALUES (?1, ?2)",
                    [format!("provider-{index}"), format!("Provider {index}")],
                )
                .expect("provider row");
        }
    }

    fn test_layout(home: &Path) -> StorageLayout {
        StorageLayout::for_test(home, home.join(APP_CONFIG_DIR_NAME), false)
    }

    #[test]
    fn fresh_install_creates_only_canonical_metadata() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        let outcome = prepare_layout(&layout, 13).expect("prepare");
        assert!(matches!(outcome, StoragePreparation::Ready { .. }));
        assert!(!layout.database_path.exists());
        assert!(layout.target_dir.join(COMPLETION_MARKER).exists());
        assert!(layout.target_dir.join(REPORT_FILENAME).exists());
    }

    #[test]
    fn promotes_legacy_database_and_owned_state_without_deleting_source() {
        let home = tempfile::tempdir().expect("home");
        fs::create_dir_all(home.path().join(".codex")).expect("codex directory");
        fs::write(home.path().join(".codex/sentinel"), b"untouched").expect("codex sentinel");
        let legacy = home.path().join(LEGACY_APP_CONFIG_DIR_NAME);
        let legacy_database = legacy.join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy_database, 11, 2);
        fs::write(legacy.join("settings.json"), br#"{"language":"vi"}"#).expect("legacy settings");
        fs::write(
            legacy.join("codex_official_auth_snapshot.json"),
            br#"{"tokens":{"access_token":"private-auth-token"}}"#,
        )
        .expect("legacy auth snapshot");
        fs::create_dir_all(legacy.join("skills/example")).expect("legacy skill");
        fs::write(legacy.join("skills/example/SKILL.md"), b"legacy skill").expect("skill file");

        let layout = test_layout(home.path());
        let outcome = prepare_layout(&layout, 13).expect("prepare");
        assert!(matches!(
            outcome,
            StoragePreparation::Ready { migrated: true, .. }
        ));
        assert!(legacy_database.exists(), "legacy source must be preserved");
        assert!(layout.database_path.exists());
        assert_eq!(
            fs::read(layout.target_dir.join("settings.json")).expect("settings"),
            br#"{"language":"vi"}"#
        );
        assert_eq!(
            fs::read_to_string(layout.target_dir.join("skills/example/SKILL.md")).expect("skill"),
            "legacy skill"
        );
        assert_eq!(
            fs::read(layout.target_dir.join("codex_official_auth_snapshot.json"))
                .expect("auth snapshot"),
            br#"{"tokens":{"access_token":"private-auth-token"}}"#
        );
        let promoted = inspect_candidate(Candidate {
            label: "promoted".to_string(),
            path: layout.database_path.clone(),
            scope_priority: 0,
            path_priority: 0,
        });
        assert!(promoted.valid);
        assert_eq!(promoted.user_version, Some(11));
        assert_eq!(
            fs::read(home.path().join(".codex/sentinel")).expect("codex sentinel"),
            b"untouched"
        );
    }

    #[test]
    fn preserves_every_database_candidate_and_conflicting_settings() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        fs::create_dir_all(&layout.target_dir).expect("target");
        make_database(&layout.database_path, 11, 0);
        fs::write(
            layout.target_dir.join("settings.json"),
            br#"{"language":"en"}"#,
        )
        .expect("target settings");
        let legacy_dir = home.path().join(LEGACY_APP_CONFIG_DIR_NAME);
        let legacy = legacy_dir.join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 11, 3);
        fs::write(legacy_dir.join("settings.json"), br#"{"language":"vi"}"#)
            .expect("legacy settings");

        prepare_layout(&layout, 13).expect("prepare");
        assert_eq!(
            fs::read(layout.target_dir.join("settings.json")).expect("selected settings"),
            br#"{"language":"vi"}"#
        );
        let migration_root = layout.target_dir.join(MIGRATION_BACKUP_DIR);
        let generation = fs::read_dir(&migration_root)
            .expect("backup generations")
            .next()
            .expect("backup generation")
            .expect("backup entry")
            .path();
        assert!(generation
            .join("databases/target-canonical/nexus-composer.db")
            .exists());
        assert!(generation
            .join("databases/legacy-cc-switch/cc-switch.db")
            .exists());
        assert_eq!(
            fs::read(generation.join("app-data/destination/settings.json"))
                .expect("conflicting settings backup"),
            br#"{"language":"en"}"#
        );
    }

    #[test]
    fn failed_snapshot_verification_leaves_existing_database_unchanged() {
        let home = tempfile::tempdir().expect("home");
        let destination = home.path().join("nexus-composer.db");
        make_database(&destination, 11, 1);
        let fingerprint_before = fingerprint_database_files(&destination).expect("fingerprint");
        let invalid_source = home.path().join("invalid.db");
        fs::write(&invalid_source, b"not a sqlite database").expect("invalid source");
        let stage = home.path().join("stage");

        assert!(promote_database_snapshot(&invalid_source, &destination, &stage, 11).is_err());
        assert_eq!(
            fingerprint_database_files(&destination).expect("fingerprint after"),
            fingerprint_before
        );
        assert!(
            inspect_candidate(Candidate {
                label: "destination".to_string(),
                path: destination,
                scope_priority: 0,
                path_priority: 0,
            })
            .valid
        );
    }

    #[test]
    fn richer_same_version_database_wins_without_using_modification_time() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        make_database(&layout.database_path, 11, 0);
        let legacy = home
            .path()
            .join(LEGACY_APP_CONFIG_DIR_NAME)
            .join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 11, 3);

        prepare_layout(&layout, 13).expect("prepare");
        let connection = Connection::open(&layout.database_path).expect("canonical database");
        let providers: i64 = connection
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .expect("provider count");
        assert_eq!(providers, 3);
        assert!(legacy.exists(), "conflicting source must remain available");
    }

    #[test]
    fn populated_supported_database_wins_over_empty_newer_candidate() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        make_database(&layout.database_path, 13, 0);
        let legacy = home
            .path()
            .join(LEGACY_APP_CONFIG_DIR_NAME)
            .join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 12, 3);

        prepare_layout(&layout, 13).expect("prepare");
        let connection = Connection::open(&layout.database_path).expect("canonical database");
        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        let providers: i64 = connection
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .expect("provider count");
        assert_eq!(version, 12);
        assert_eq!(providers, 3);
    }

    #[test]
    fn explicit_override_has_priority_over_default_database() {
        let home = tempfile::tempdir().expect("home");
        let target = home.path().join("custom-storage");
        let layout = StorageLayout::for_test(home.path(), target.clone(), true);
        make_database(&target.join(APP_DATABASE_FILENAME), 10, 1);
        make_database(
            &home
                .path()
                .join(APP_CONFIG_DIR_NAME)
                .join(APP_DATABASE_FILENAME),
            13,
            5,
        );

        prepare_layout(&layout, 13).expect("prepare");
        let connection = Connection::open(target.join(APP_DATABASE_FILENAME)).expect("override");
        let version: i32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, 10);
    }

    #[test]
    fn newer_selected_database_stops_before_writing() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        let legacy = home
            .path()
            .join(LEGACY_APP_CONFIG_DIR_NAME)
            .join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 14, 1);

        let outcome = prepare_layout(&layout, 13).expect("preflight outcome");
        assert!(matches!(
            outcome,
            StoragePreparation::DatabaseTooNew { version: 14, .. }
        ));
        assert!(!layout.target_dir.exists());
        assert!(legacy.exists());
    }

    #[test]
    fn corrupt_database_does_not_replace_a_valid_candidate() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        fs::create_dir_all(&layout.target_dir).expect("target");
        fs::write(&layout.database_path, b"not sqlite").expect("corrupt database");
        let legacy = home
            .path()
            .join(LEGACY_APP_CONFIG_DIR_NAME)
            .join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 11, 1);

        prepare_layout(&layout, 13).expect("prepare");
        let promoted = inspect_candidate(Candidate {
            label: "promoted".to_string(),
            path: layout.database_path.clone(),
            scope_priority: 0,
            path_priority: 0,
        });
        assert!(promoted.valid);
        assert_eq!(promoted.user_version, Some(11));
        let backup_root = layout.target_dir.join(MIGRATION_BACKUP_DIR);
        assert!(backup_root.exists());
    }

    #[test]
    fn completed_marker_keeps_canonical_database_authoritative() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        make_database(&layout.database_path, 11, 1);
        let legacy = home
            .path()
            .join(LEGACY_APP_CONFIG_DIR_NAME)
            .join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 13, 5);
        fs::create_dir_all(&layout.target_dir).expect("target");
        write_private_json_atomic(
            &layout.target_dir.join(COMPLETION_MARKER),
            &CompletionMarker {
                format_version: MIGRATION_FORMAT_VERSION,
                owned_inventory_version: OWNED_INVENTORY_VERSION,
                completed_at: Utc::now().to_rfc3339(),
                database_filename: APP_DATABASE_FILENAME.to_string(),
                selected_database: Some("target-canonical".to_string()),
            },
        )
        .expect("marker");
        let stale_stage_name = ".storage-migration-stage-completed-before-cleanup";
        let stale_stage = layout.target_dir.join(stale_stage_name);
        fs::create_dir_all(&stale_stage).expect("stale stage");
        write_private_json_atomic(
            &layout.target_dir.join(IN_PROGRESS_MARKER),
            &InProgressMarker {
                format_version: MIGRATION_FORMAT_VERSION,
                migration_id: "completed-before-cleanup".to_string(),
                started_at: Utc::now().to_rfc3339(),
                selected_database: Some("target-canonical".to_string()),
                stage_directory: stale_stage_name.to_string(),
            },
        )
        .expect("stale state");

        let outcome = prepare_layout(&layout, 13).expect("prepare");
        assert!(matches!(
            outcome,
            StoragePreparation::Ready {
                migrated: false,
                ..
            }
        ));
        let canonical = Connection::open(&layout.database_path).expect("canonical");
        let providers: i64 = canonical
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .expect("count");
        assert_eq!(providers, 1);
        assert!(!stale_stage.exists());
        assert!(!layout.target_dir.join(IN_PROGRESS_MARKER).exists());
    }

    #[test]
    fn older_inventory_marker_backfills_new_owned_files_without_reselecting_database() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        make_database(&layout.database_path, 13, 1);
        let legacy_dir = home.path().join(LEGACY_APP_CONFIG_DIR_NAME);
        let legacy = legacy_dir.join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 13, 5);
        fs::write(
            legacy_dir.join("codex_oauth_auth.json"),
            br#"{"refresh_token":"preserved-token"}"#,
        )
        .expect("legacy OAuth state");
        fs::create_dir_all(&layout.target_dir).expect("target");
        write_private_json_atomic(
            &layout.target_dir.join(COMPLETION_MARKER),
            &serde_json::json!({
                "formatVersion": MIGRATION_FORMAT_VERSION,
                "completedAt": Utc::now().to_rfc3339(),
                "databaseFilename": APP_DATABASE_FILENAME,
                "selectedDatabase": "target-canonical"
            }),
        )
        .expect("old marker");

        let first = prepare_layout(&layout, 13).expect("inventory upgrade");
        assert!(matches!(
            first,
            StoragePreparation::Ready { migrated: true, .. }
        ));
        assert!(layout.target_dir.join("codex_oauth_auth.json").exists());
        let canonical = Connection::open(&layout.database_path).expect("canonical");
        let providers: i64 = canonical
            .query_row("SELECT COUNT(*) FROM providers", [], |row| row.get(0))
            .expect("count");
        assert_eq!(providers, 1, "completion continuity must keep canonical DB");
        drop(canonical);

        let marker = read_completion_marker(&layout.target_dir.join(COMPLETION_MARKER))
            .expect("updated marker");
        assert_eq!(marker.owned_inventory_version, OWNED_INVENTORY_VERSION);
        let second = prepare_layout(&layout, 13).expect("idempotent restart");
        assert!(matches!(
            second,
            StoragePreparation::Ready {
                migrated: false,
                ..
            }
        ));
    }

    #[test]
    fn interrupted_marker_is_reported_and_retry_completes() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        fs::create_dir_all(&layout.target_dir).expect("target");
        let interrupted_stage = layout
            .target_dir
            .join(".storage-migration-stage-interrupted");
        fs::create_dir_all(&interrupted_stage).expect("interrupted stage");
        fs::write(interrupted_stage.join("partial-file"), b"partial").expect("partial stage");
        write_private_json_atomic(
            &layout.target_dir.join(IN_PROGRESS_MARKER),
            &InProgressMarker {
                format_version: MIGRATION_FORMAT_VERSION,
                migration_id: "interrupted".to_string(),
                started_at: Utc::now().to_rfc3339(),
                selected_database: Some("legacy-cc-switch".to_string()),
                stage_directory: ".storage-migration-stage-interrupted".to_string(),
            },
        )
        .expect("state");
        let legacy = home
            .path()
            .join(LEGACY_APP_CONFIG_DIR_NAME)
            .join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 11, 1);

        prepare_layout(&layout, 13).expect("retry");
        let report: StorageMigrationReport = serde_json::from_slice(
            &fs::read(layout.target_dir.join(REPORT_FILENAME)).expect("report"),
        )
        .expect("report json");
        assert!(report.interrupted_previous_attempt);
        assert!(!layout.target_dir.join(IN_PROGRESS_MARKER).exists());
        assert!(!interrupted_stage.exists());
    }

    #[test]
    fn report_contains_metadata_but_not_database_content() {
        let home = tempfile::tempdir().expect("home");
        let layout = test_layout(home.path());
        let legacy = home
            .path()
            .join(LEGACY_APP_CONFIG_DIR_NAME)
            .join(LEGACY_APP_DATABASE_FILENAME);
        make_database(&legacy, 11, 1);
        let connection = Connection::open(&legacy).expect("legacy");
        connection
            .execute(
                "INSERT INTO providers (id, name) VALUES (?1, ?2)",
                ["secret-token-value", "private-provider-name"],
            )
            .expect("secret row");
        drop(connection);
        fs::write(
            home.path()
                .join(LEGACY_APP_CONFIG_DIR_NAME)
                .join("codex_oauth_auth.json"),
            br#"{"refresh_token":"private-refresh-token"}"#,
        )
        .expect("legacy OAuth state");

        prepare_layout(&layout, 13).expect("prepare");
        let report = fs::read_to_string(layout.target_dir.join(REPORT_FILENAME)).expect("report");
        assert!(!report.contains("secret-token-value"));
        assert!(!report.contains("private-provider-name"));
        assert!(!report.contains("private-refresh-token"));
    }
}
