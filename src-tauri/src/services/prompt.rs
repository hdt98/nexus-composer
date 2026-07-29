use indexmap::IndexMap;

use crate::app_config::AppType;
use crate::config::write_text_file;
use crate::error::AppError;
use crate::prompt::Prompt;
use crate::prompt_files::prompt_file_path;
use crate::store::AppState;

/// Return the current Unix timestamp without panicking.
fn get_unix_timestamp() -> Result<i64, AppError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| AppError::Message(format!("Failed to get system time: {e}")))
}

pub struct PromptService;

impl PromptService {
    pub fn get_prompts(
        state: &AppState,
        app: AppType,
    ) -> Result<IndexMap<String, Prompt>, AppError> {
        state.db.get_prompts(app.as_str())
    }

    pub fn upsert_prompt(
        state: &AppState,
        app: AppType,
        _id: &str,
        prompt: Prompt,
    ) -> Result<(), AppError> {
        // Check whether this prompt is enabled.
        let is_enabled = prompt.enabled;

        state.db.save_prompt(app.as_str(), &prompt)?;

        if is_enabled {
            // Write enabled prompt content to the live file.
            let target_path = prompt_file_path(&app)?;
            write_text_file(&target_path, &prompt.content)?;
        } else {
            // Check whether another prompt remains enabled.
            let prompts = state.db.get_prompts(app.as_str())?;
            let any_enabled = prompts.values().any(|p| p.enabled);

            if !any_enabled {
                // Clear the live file when all prompts are disabled.
                let target_path = prompt_file_path(&app)?;
                if target_path.exists() {
                    write_text_file(&target_path, "")?;
                }
            }
        }

        Ok(())
    }

    pub fn delete_prompt(state: &AppState, app: AppType, id: &str) -> Result<(), AppError> {
        let prompts = state.db.get_prompts(app.as_str())?;

        if let Some(prompt) = prompts.get(id) {
            if prompt.enabled {
                return Err(AppError::InvalidInput(
                    "Cannot delete an enabled prompt".to_string(),
                ));
            }
        }

        state.db.delete_prompt(app.as_str(), id)?;
        Ok(())
    }

    pub fn enable_prompt(state: &AppState, app: AppType, id: &str) -> Result<(), AppError> {
        // Save live file content to the enabled prompt or create a backup.
        let target_path = prompt_file_path(&app)?;
        if target_path.exists() {
            if let Ok(live_content) = std::fs::read_to_string(&target_path) {
                if !live_content.trim().is_empty() {
                    let mut prompts = state.db.get_prompts(app.as_str())?;

                    // Save the live content to the currently enabled prompt.
                    if let Some((enabled_id, enabled_prompt)) = prompts
                        .iter_mut()
                        .find(|(_, p)| p.enabled)
                        .map(|(id, p)| (id.clone(), p))
                    {
                        let timestamp = get_unix_timestamp()?;
                        enabled_prompt.content = live_content.clone();
                        enabled_prompt.updated_at = Some(timestamp);
                        log::info!("Saved live prompt content to enabled prompt: {enabled_id}");
                        state.db.save_prompt(app.as_str(), enabled_prompt)?;
                    } else {
                        // Create one backup when no prompt is enabled, avoiding duplicates.
                        let content_exists = prompts
                            .values()
                            .any(|p| p.content.trim() == live_content.trim());
                        if !content_exists {
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64;
                            let backup_id = format!("backup-{timestamp}");
                            let backup_prompt = Prompt {
                                id: backup_id.clone(),
                                name: format!(
                                    "Original Prompt {}",
                                    chrono::Local::now().format("%Y-%m-%d %H:%M")
                                ),
                                content: live_content,
                                description: Some(
                                    "Automatically backed up original prompt".to_string(),
                                ),
                                enabled: false,
                                created_at: Some(timestamp),
                                updated_at: Some(timestamp),
                            };
                            log::info!("Backed up live prompt content: {backup_id}");
                            state.db.save_prompt(app.as_str(), &backup_prompt)?;
                        }
                    }
                }
            }
        }

        // Enable the target prompt and write it to the live file.
        let mut prompts = state.db.get_prompts(app.as_str())?;

        for prompt in prompts.values_mut() {
            prompt.enabled = false;
        }

        if let Some(prompt) = prompts.get_mut(id) {
            prompt.enabled = true;
            write_text_file(&target_path, &prompt.content)?; // Atomic write.
            state.db.save_prompt(app.as_str(), prompt)?;
        } else {
            return Err(AppError::InvalidInput(format!(
                "Prompt {id} does not exist"
            )));
        }

        // Save all prompts to disable others
        for (_, prompt) in prompts.iter() {
            state.db.save_prompt(app.as_str(), prompt)?;
        }

        Ok(())
    }

    pub fn import_from_file(state: &AppState, app: AppType) -> Result<String, AppError> {
        let file_path = prompt_file_path(&app)?;

        if !file_path.exists() {
            return Err(AppError::Message("Prompt file does not exist".to_string()));
        }

        let content =
            std::fs::read_to_string(&file_path).map_err(|e| AppError::io(&file_path, e))?;
        let timestamp = get_unix_timestamp()?;

        let id = format!("imported-{timestamp}");
        let prompt = Prompt {
            id: id.clone(),
            name: format!(
                "Imported Prompt {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content,
            description: Some("Imported from an existing configuration file".to_string()),
            enabled: false,
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
        };

        Self::upsert_prompt(state, app, &id, prompt)?;
        Ok(id)
    }

    pub fn get_current_file_content(app: AppType) -> Result<Option<String>, AppError> {
        let file_path = prompt_file_path(&app)?;
        if !file_path.exists() {
            return Ok(None);
        }
        let content =
            std::fs::read_to_string(&file_path).map_err(|e| AppError::io(&file_path, e))?;
        Ok(Some(content))
    }

    /// Import an existing prompt file on first launch, if present.
    /// Returns the number of imported prompts.
    pub fn import_from_file_on_first_launch(
        state: &AppState,
        app: AppType,
    ) -> Result<usize, AppError> {
        // Skip import when this application already has prompts.
        let existing = state.db.get_prompts(app.as_str())?;
        if !existing.is_empty() {
            return Ok(0);
        }

        let file_path = prompt_file_path(&app)?;

        // Check whether the prompt file exists.
        if !file_path.exists() {
            return Ok(0);
        }

        // Read the prompt file.
        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Failed to read prompt file {file_path:?}: {e}");
                return Ok(0);
            }
        };

        // Ignore empty prompt files.
        if content.trim().is_empty() {
            return Ok(0);
        }

        log::info!("Found prompt file; importing automatically: {file_path:?}");

        // Create the prompt record.
        let timestamp = get_unix_timestamp()?;
        let id = format!("auto-imported-{timestamp}");
        let prompt = Prompt {
            id: id.clone(),
            name: format!(
                "Auto-imported Prompt {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content,
            description: Some("Automatically imported on first launch".to_string()),
            enabled: true, // Enable automatically on first import.
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
        };

        // Save to the database.
        state.db.save_prompt(app.as_str(), &prompt)?;

        log::info!("Automatic prompt import completed: {}", app.as_str());
        Ok(1)
    }
}
