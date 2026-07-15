use crate::app_config::AppType;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::provider::Provider;
use rusqlite::params;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use toml_edit::{value, DocumentMut};

pub(crate) const ENDPOINT: &str = "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
pub(crate) const MODEL: &str = "GLM-5.2-FP8";
pub(crate) const CLAUDE_MODEL: &str = "GLM-5.2-FP8[1m]";
pub(crate) const VERSION: u32 = 4;
const NAME: &str = "Nexus GLM-5.2";
const CONTEXT: i64 = 1_048_576;
const COMPACT: i64 = 252_000;
const MAX_TOKENS: u64 = 65_536;
const STREAM_IDLE_MS: i64 = 3_000_000;
const LOCAL_ENDPOINT: &str = "http://127.0.0.1:30000/v1";
const DRAFT_ENDPOINT: &str = "http://127.0.0.1:30001/v1";
const HOSTED_ENDPOINT: &str = "https://glm-test-glm52-tp4.onenexus-do.cloud/v1";
const LOCAL_MODEL: &str = "GLM-5.2-SGLang";
const LEGACY_MODEL: &str = "glm-5.2";
const LEGACY_CLAUDE_MODEL: &str = "glm-5.2[1m]";
const LEGACY_NAMES: [&str; 4] = ["Nexus", "Nexus Local", "Nexus GLM-5.2 Hosted", NAME];
const SHIPPED_KEY_PLACEHOLDERS: [&str; 2] = ["nexus-local", "dummy"];
const SHIPPED_KEY_FINGERPRINTS: [&str; 2] = [
    "305da1291142205e2260675309af8963e2394257e2851f9c58759272c85eab73",
    "56c3568cabcc36524156db1191c4cd361710ebb88837d8b9457b2f59986465dc",
];

// (endpoint, Codex model, Claude model)
type Signature = (&'static str, &'static str, &'static str);
const SIGNATURES: [Signature; 4] = [
    (LOCAL_ENDPOINT, LOCAL_MODEL, LOCAL_MODEL),
    (HOSTED_ENDPOINT, LEGACY_MODEL, LEGACY_MODEL),
    (DRAFT_ENDPOINT, LEGACY_MODEL, LEGACY_CLAUDE_MODEL),
    (ENDPOINT, MODEL, CLAUDE_MODEL),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ownership {
    Current,
    Upgrade,
    Detach,
    Unrelated,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct NexusMigrationOutcome {
    pub(crate) migrated: usize,
    pub(crate) current_claude_changed: bool,
    pub(crate) current_claude_desktop_changed: bool,
    pub(crate) current_codex_changed: bool,
}

fn mark_current_changed(
    outcome: &mut NexusMigrationOutcome,
    app: &str,
    id: &str,
    current_claude_id: Option<&str>,
    current_claude_desktop_id: Option<&str>,
    current_codex_id: Option<&str>,
) {
    outcome.current_claude_changed |= app == "claude" && current_claude_id == Some(id);
    outcome.current_claude_desktop_changed |=
        app == "claude-desktop" && current_claude_desktop_id == Some(id);
    outcome.current_codex_changed |= app == "codex" && current_codex_id == Some(id);
}

fn classify(app: &str, name: &str, settings: &Value, meta: &Value) -> Ownership {
    let kind = meta.get("providerType").and_then(Value::as_str);
    let version = meta
        .get("managedNexusPresetVersion")
        .and_then(Value::as_u64);
    if kind != Some("nexus") {
        return if kind.is_none()
            && version.is_none()
            && LEGACY_NAMES.contains(&name)
            && target(app, settings, meta).is_some()
        {
            Ownership::Upgrade
        } else {
            Ownership::Unrelated
        };
    }
    // Older binaries must not reinterpret future preset versions or targets.
    if version.is_some_and(|value| value > u64::from(VERSION)) {
        return Ownership::Current;
    }
    let Some(signature) = target(app, settings, meta) else {
        return Ownership::Detach;
    };
    if version == Some(u64::from(VERSION))
        && signature.0 == ENDPOINT
        && signature.1 == MODEL
        && meta.get("codexChatReasoning") != Some(&owned_reasoning())
    {
        Ownership::Current
    } else {
        Ownership::Upgrade
    }
}

fn target(app: &str, settings: &Value, meta: &Value) -> Option<&'static Signature> {
    if meta.get("apiFormat").and_then(Value::as_str) != Some("openai_chat") {
        return None;
    }
    match app {
        "claude" => {
            let env = settings.get("env")?.as_object()?;
            let endpoint = env.get("ANTHROPIC_BASE_URL")?.as_str()?;
            let model = env.get("ANTHROPIC_MODEL")?.as_str()?;
            let signature = SIGNATURES
                .iter()
                .find(|item| item.0 == endpoint && item.2 == model)?;
            let required = [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
            ];
            required
                .iter()
                .all(|key| env.get(*key).and_then(Value::as_str) == Some(signature.2))
                .then_some(signature)
        }
        "codex" => {
            let config = settings.get("config")?.as_str()?;
            let doc = config.parse::<DocumentMut>().ok()?;
            let endpoint = crate::codex_config::extract_codex_base_url(config)?;
            let model = doc.get("model")?.as_str()?;
            SIGNATURES
                .iter()
                .find(|item| item.0 == endpoint && item.1 == model)
        }
        "claude-desktop" => {
            if meta.get("claudeDesktopMode").and_then(Value::as_str) != Some("proxy") {
                return None;
            }
            let endpoint = settings.pointer("/env/ANTHROPIC_BASE_URL")?.as_str()?;
            let routes = meta.get("claudeDesktopModelRoutes")?.as_object()?;
            let model = routes.values().next()?.get("model")?.as_str()?;
            SIGNATURES.iter().find(|item| {
                item.0 == endpoint
                    && item.1 == model
                    && routes
                        .values()
                        .all(|route| route.get("model").and_then(Value::as_str) == Some(model))
            })
        }
        _ => None,
    }
}

fn owned_reasoning() -> Value {
    json!({
        "supportsThinking": true,
        "supportsEffort": false,
        "thinkingParam": "chat_template_kwargs.enable_thinking",
        "effortParam": "none",
        "outputFormat": "reasoning_content"
    })
}

fn remove_owned_reasoning(meta: &mut Map<String, Value>) {
    if meta.get("codexChatReasoning") == Some(&owned_reasoning()) {
        meta.remove("codexChatReasoning");
    }
}

fn remove_thinking(meta: &mut Map<String, Value>) {
    let Some(overrides) = meta
        .get_mut("localProxyRequestOverrides")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Some(body) = overrides.get_mut("body").and_then(Value::as_object_mut) else {
        return;
    };
    if body.get("max_tokens") == Some(&json!(MAX_TOKENS)) {
        body.remove("max_tokens");
    }
    if let Some(template) = body
        .get_mut("chat_template_kwargs")
        .and_then(Value::as_object_mut)
    {
        if template.get("enable_thinking") == Some(&json!(true)) {
            template.remove("enable_thinking");
        }
        if template.is_empty() {
            body.remove("chat_template_kwargs");
        }
    }
    if body.is_empty() {
        overrides.remove("body");
    }
    if overrides.is_empty() {
        meta.remove("localProxyRequestOverrides");
    }
}

fn is_owned_catalog(entry: &Value) -> bool {
    [
        json!({"model": LOCAL_MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT}),
        json!({"model": LEGACY_MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT}),
        json!({"model": LEGACY_MODEL, "inputModalities": ["text"]}),
        json!({"model": MODEL, "inputModalities": ["text"]}),
        json!({"model": MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT}),
        json!({"model": MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT, "inputModalities": ["text"]}),
    ]
    .contains(entry)
}

fn edit_catalog(settings: &mut Value, remove: impl Fn(&Value) -> bool) {
    let Some(settings) = settings.as_object_mut() else {
        return;
    };
    let empty = settings
        .get_mut("modelCatalog")
        .and_then(Value::as_object_mut)
        .and_then(|catalog| {
            catalog
                .get_mut("models")?
                .as_array_mut()?
                .retain(|entry| !remove(entry));
            Some(catalog.len() == 1 && catalog["models"].as_array().is_some_and(Vec::is_empty))
        })
        .unwrap_or(false);
    if empty {
        settings.remove("modelCatalog");
    }
}

fn merge_catalog(existing: &Value, updated: &mut Value) {
    let Some(source) = existing.get("modelCatalog").and_then(Value::as_object) else {
        return;
    };
    let Some(updated) = updated.as_object_mut() else {
        return;
    };
    let Some(target) = updated
        .entry("modelCatalog")
        .or_insert_with(|| Value::Object(source.clone()))
        .as_object_mut()
    else {
        return;
    };
    for (key, value) in source {
        if key != "models" {
            target.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    let Some(models) = target.get_mut("models").and_then(Value::as_array_mut) else {
        return;
    };
    for entry in source
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let managed = is_owned_catalog(entry);
        if !managed && !models.contains(entry) {
            models.push(entry.clone());
        }
    }
}

fn rewrite_meta(provider: &mut Provider, detach: bool) {
    let Some(meta) = provider.meta.as_mut() else {
        return;
    };
    let Ok(Value::Object(mut value)) = serde_json::to_value(&*meta) else {
        return;
    };
    if detach {
        value.remove("providerType");
        value.remove("managedNexusPresetVersion");
        remove_thinking(&mut value);
    }
    remove_owned_reasoning(&mut value);
    if let Ok(meta) = serde_json::from_value(Value::Object(value)) {
        provider.meta = Some(meta);
    }
}

/// Authoritative ownership check for every provider update path.
pub(crate) fn reconcile_managed_nexus_update(
    app: &AppType,
    existing: &Provider,
    updated: &mut Provider,
) -> bool {
    if [existing, updated].iter().any(|provider| {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.managed_nexus_preset_version)
            .is_some_and(|version| version > VERSION)
    }) {
        return false;
    }
    let existing_managed = existing
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref())
        == Some("nexus");
    let meta = updated
        .meta
        .as_ref()
        .and_then(|meta| serde_json::to_value(meta).ok())
        .unwrap_or_else(|| json!({}));
    let stays_managed = meta.get("providerType").and_then(Value::as_str) == Some("nexus")
        && matches!(
            classify(app.as_str(), &updated.name, &updated.settings_config, &meta),
            Ownership::Current | Ownership::Upgrade
        );
    if stays_managed {
        rewrite_meta(updated, false);
        merge_catalog(&existing.settings_config, &mut updated.settings_config);
    } else if existing_managed {
        rewrite_meta(updated, true);
        let inherited = existing
            .settings_config
            .pointer("/modelCatalog/models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        edit_catalog(&mut updated.settings_config, |entry| {
            is_owned_catalog(entry) && inherited.contains(entry)
        });
    }
    existing_managed && !stays_managed
}

pub(crate) fn managed_nexus_endpoint_candidates() -> impl Iterator<Item = &'static str> {
    SIGNATURES.iter().map(|signature| signature.0)
}

fn matches_credential_fingerprint(value: &str, fingerprints: &[&str]) -> bool {
    let digest = format!("{:x}", Sha256::digest(value.trim().as_bytes()));
    fingerprints.contains(&digest.as_str())
}

fn is_leaked_nexus_credential(value: &str) -> bool {
    matches_credential_fingerprint(value, &SHIPPED_KEY_FINGERPRINTS)
}

fn is_shipped_nexus_credential(value: &str) -> bool {
    let value = value.trim();
    SHIPPED_KEY_PLACEHOLDERS.contains(&value) || is_leaked_nexus_credential(value)
}

fn scrub_if(object: &mut Map<String, Value>, key: &str, predicate: impl Fn(&str) -> bool) -> bool {
    let remove = object
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(predicate);
    if remove {
        object.remove(key);
    }
    remove
}

fn scrub_credentials_if(
    app: &AppType,
    settings: &mut Value,
    predicate: impl Fn(&str) -> bool + Copy,
) -> Result<bool, AppError> {
    Ok(match app {
        AppType::Claude | AppType::ClaudeDesktop => settings
            .get_mut("env")
            .and_then(Value::as_object_mut)
            .map(|env| {
                let mut changed = false;
                for key in [
                    "ANTHROPIC_AUTH_TOKEN",
                    "ANTHROPIC_API_KEY",
                    "OPENROUTER_API_KEY",
                    "OPENAI_API_KEY",
                ] {
                    changed |= scrub_if(env, key, predicate);
                }
                changed
            })
            .unwrap_or(false),
        AppType::Codex => {
            let mut changed = settings
                .get_mut("auth")
                .and_then(Value::as_object_mut)
                .map(|auth| scrub_if(auth, "OPENAI_API_KEY", predicate))
                .unwrap_or(false);
            let filtered = if let Some(config) = settings.get("config").and_then(Value::as_str) {
                crate::codex_config::remove_all_codex_experimental_bearer_tokens_if(
                    config, predicate,
                )?
            } else {
                None
            };
            if let Some(filtered) = filtered {
                settings["config"] = json!(filtered);
                changed = true;
            }
            changed
        }
        _ => false,
    })
}

pub(crate) fn scrub_leaked_nexus_credentials(
    app: &AppType,
    settings: &mut Value,
) -> Result<bool, AppError> {
    scrub_credentials_if(app, settings, is_leaked_nexus_credential)
}

fn canonical_catalog(settings: &mut Value, legacy: bool) -> bool {
    let Some(settings) = settings.as_object_mut() else {
        return false;
    };
    let Some(catalog) = settings
        .entry("modelCatalog")
        .or_insert_with(|| json!({"models": []}))
        .as_object_mut()
    else {
        return false;
    };
    let Some(models) = catalog
        .entry("models")
        .or_insert_with(|| json!([]))
        .as_array_mut()
    else {
        return false;
    };
    let mut siblings = std::mem::take(models);
    siblings.retain(|entry| !is_owned_catalog(entry));
    models.push(json!({
        "model": MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT,
        "inputModalities": ["text"]
    }));
    if legacy {
        models.push(json!({"model": LEGACY_MODEL, "inputModalities": ["text"]}));
    }
    models.append(&mut siblings);
    true
}

fn merge_override(meta: &mut Map<String, Value>) -> bool {
    let Some(body) = meta
        .entry("localProxyRequestOverrides")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .and_then(|overrides| {
            overrides
                .entry("body")
                .or_insert_with(|| json!({}))
                .as_object_mut()
        })
    else {
        return false;
    };
    body.entry("max_tokens")
        .or_insert_with(|| json!(MAX_TOKENS));
    let Some(template) = body
        .entry("chat_template_kwargs")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return false;
    };
    template.insert("enable_thinking".into(), json!(true));
    true
}

fn upgrade(app: &str, settings: &mut Value, meta: &mut Value) -> bool {
    let Some(meta) = meta.as_object_mut() else {
        return false;
    };
    let ok = match app {
        "claude" => {
            settings
                .get_mut("env")
                .and_then(Value::as_object_mut)
                .is_some_and(|env| {
                    env.insert("ANTHROPIC_BASE_URL".into(), json!(ENDPOINT));
                    for key in [
                        "ANTHROPIC_MODEL",
                        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                        "ANTHROPIC_DEFAULT_SONNET_MODEL",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL",
                        "ANTHROPIC_DEFAULT_FABLE_MODEL",
                        "ANTHROPIC_CUSTOM_MODEL_OPTION",
                    ] {
                        env.insert(key.into(), json!(CLAUDE_MODEL));
                    }
                    for (key, value) in [
                        ("API_TIMEOUT_MS", "3000000"),
                        ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "252000"),
                        ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
                        ("CLAUDE_CODE_ATTRIBUTION_HEADER", "0"),
                    ] {
                        env.insert(key.into(), json!(value));
                    }
                    scrub_if(env, "ANTHROPIC_AUTH_TOKEN", is_shipped_nexus_credential);
                    true
                })
                && canonical_catalog(settings, true)
        }
        "claude-desktop" => {
            let env_ok = settings
                .pointer_mut("/env")
                .and_then(Value::as_object_mut)
                .is_some_and(|env| {
                    env.insert("ANTHROPIC_BASE_URL".into(), json!(ENDPOINT));
                    scrub_if(env, "ANTHROPIC_AUTH_TOKEN", is_shipped_nexus_credential);
                    true
                });
            let routes_ok = meta
                .get_mut("claudeDesktopModelRoutes")
                .and_then(Value::as_object_mut)
                .is_some_and(|routes| {
                    routes.values_mut().all(|route| {
                        route.as_object_mut().is_some_and(|route| {
                            route.insert("model".into(), json!(MODEL));
                            true
                        })
                    })
                });
            env_ok && routes_ok && canonical_catalog(settings, true)
        }
        "codex" => settings
            .get("config")
            .and_then(Value::as_str)
            .and_then(|config| config.parse::<DocumentMut>().ok())
            .and_then(|mut doc| {
                let active = doc.get("model_provider")?.as_str()?.to_string();
                doc["model"] = value(MODEL);
                doc["model_context_window"] = value(CONTEXT);
                doc["model_auto_compact_token_limit"] = value(COMPACT);
                doc.as_table_mut().remove("model_reasoning_effort");
                doc["model_providers"][&active]["base_url"] = value(ENDPOINT);
                doc["model_providers"][&active]["stream_idle_timeout_ms"] = value(STREAM_IDLE_MS);
                Some(doc.to_string())
            })
            .is_some_and(|config| {
                settings["config"] = json!(config);
                if let Some(auth) = settings.get_mut("auth").and_then(Value::as_object_mut) {
                    scrub_if(auth, "OPENAI_API_KEY", is_shipped_nexus_credential);
                }
                canonical_catalog(settings, false)
            }),
        _ => false,
    };
    if !ok || !merge_override(meta) {
        return false;
    }
    meta.insert("providerType".into(), json!("nexus"));
    meta.insert("apiFormat".into(), json!("openai_chat"));
    meta.insert("managedNexusPresetVersion".into(), json!(VERSION));
    if app == "claude-desktop" {
        meta.insert("claudeDesktopMode".into(), json!("proxy"));
    }
    remove_owned_reasoning(meta);
    true
}

impl Database {
    pub(crate) fn migrate_legacy_nexus_providers(
        &self,
        current_claude_id: Option<&str>,
        current_claude_desktop_id: Option<&str>,
        current_codex_id: Option<&str>,
    ) -> Result<NexusMigrationOutcome, AppError> {
        self.migrate_legacy_nexus_providers_if(
            current_claude_id,
            current_claude_desktop_id,
            current_codex_id,
            is_leaked_nexus_credential,
        )
    }

    fn migrate_legacy_nexus_providers_if(
        &self,
        current_claude_id: Option<&str>,
        current_claude_desktop_id: Option<&str>,
        current_codex_id: Option<&str>,
        credential_predicate: impl Fn(&str) -> bool + Copy,
    ) -> Result<NexusMigrationOutcome, AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction()?;
        let rows = {
            let mut statement = tx.prepare(
                "SELECT id, app_type, name, settings_config, meta FROM providers
                 WHERE app_type IN ('claude', 'claude-desktop', 'codex')",
            )?;
            let mapped = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut outcome = NexusMigrationOutcome::default();
        for (id, app, name, settings, meta) in rows {
            let mut settings = serde_json::from_str::<Value>(&settings).map_err(|error| {
                AppError::Message(format!(
                    "Invalid {app} provider '{id}' settings_config JSON: {error}"
                ))
            })?;
            let mut meta = serde_json::from_str::<Value>(&meta).map_err(|error| {
                AppError::Message(format!("Invalid {app} provider '{id}' meta JSON: {error}"))
            })?;
            let app_type = app.parse::<AppType>()?;
            let leaked = scrub_credentials_if(&app_type, &mut settings, credential_predicate)?;
            let ownership = classify(&app, &name, &settings, &meta);
            match ownership {
                Ownership::Current | Ownership::Unrelated => {
                    if leaked {
                        tx.execute(
                            "UPDATE providers SET settings_config=?1 WHERE id=?2 AND app_type=?3",
                            params![settings.to_string(), id, app],
                        )?;
                        // Security exception: scrub-only rows otherwise retain their exact
                        // ownership/version. A current row must still be projected once so its
                        // sanitized settings replace the leaked credential in the live client.
                        mark_current_changed(
                            &mut outcome,
                            &app,
                            &id,
                            current_claude_id,
                            current_claude_desktop_id,
                            current_codex_id,
                        );
                    }
                    continue;
                }
                Ownership::Detach => {
                    let Some(object) = meta.as_object_mut() else {
                        continue;
                    };
                    object.remove("providerType");
                    object.remove("managedNexusPresetVersion");
                    remove_owned_reasoning(object);
                    remove_thinking(object);
                    edit_catalog(&mut settings, is_owned_catalog);
                    tx.execute(
                        "UPDATE providers SET settings_config=?1, meta=?2 WHERE id=?3 AND app_type=?4",
                        params![settings.to_string(), meta.to_string(), id, app],
                    )?;
                    for endpoint in SIGNATURES.map(|signature| signature.0) {
                        tx.execute(
                            "DELETE FROM provider_endpoints WHERE provider_id=?1 AND app_type=?2 AND url=?3",
                            params![id, app, endpoint],
                        )?;
                    }
                }
                Ownership::Upgrade => {
                    if !upgrade(&app, &mut settings, &mut meta) {
                        continue;
                    }
                    tx.execute(
                        "UPDATE providers SET name=?1, settings_config=?2, website_url=?3, meta=?4
                         WHERE id=?5 AND app_type=?6",
                        params![
                            NAME,
                            settings.to_string(),
                            ENDPOINT,
                            meta.to_string(),
                            id,
                            app
                        ],
                    )?;
                    for old in [LOCAL_ENDPOINT, DRAFT_ENDPOINT, HOSTED_ENDPOINT] {
                        tx.execute(
                            "DELETE FROM provider_endpoints WHERE provider_id=?1 AND app_type=?2 AND url=?3",
                            params![id, app, old],
                        )?;
                    }
                    tx.execute(
                        "INSERT INTO provider_endpoints(provider_id,app_type,url,added_at)
                         SELECT ?1,?2,?3,?4 WHERE NOT EXISTS(
                           SELECT 1 FROM provider_endpoints WHERE provider_id=?1 AND app_type=?2 AND url=?3)",
                        params![id, app, ENDPOINT, chrono::Utc::now().timestamp_millis()],
                    )?;
                }
            }
            outcome.migrated += 1;
            mark_current_changed(
                &mut outcome,
                &app,
                &id,
                current_claude_id,
                current_claude_desktop_id,
                current_codex_id,
            );
        }
        let backups = {
            let mut statement = tx.prepare(
                "SELECT app_type, original_config FROM proxy_live_backup
                 WHERE app_type IN ('claude', 'claude-desktop', 'codex')",
            )?;
            let mapped = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (app, config) in backups {
            let app_type = app.parse::<AppType>()?;
            let mut config = serde_json::from_str::<Value>(&config).map_err(|error| {
                AppError::Message(format!("Invalid {app} live backup JSON: {error}"))
            })?;
            if scrub_credentials_if(&app_type, &mut config, credential_predicate)? {
                tx.execute(
                    "UPDATE proxy_live_backup SET original_config=?1 WHERE app_type=?2",
                    params![config.to_string(), app],
                )?;
            }
        }
        tx.commit()?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "macos", windows))]
    use serial_test::serial;
    #[cfg(any(target_os = "macos", windows))]
    use std::{ffi::OsString, sync::Arc};
    #[cfg(any(target_os = "macos", windows))]
    use tempfile::TempDir;

    #[cfg(any(target_os = "macos", windows))]
    struct TestHome {
        _dir: TempDir,
        original: Option<OsString>,
    }

    #[cfg(any(target_os = "macos", windows))]
    impl TestHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let original = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            Self {
                _dir: dir,
                original,
            }
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    impl Drop for TestHome {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn save(db: &Database, app: &str, id: &str, name: &str, settings: Value, mut meta: Value) {
        meta["apiFormat"] = json!("openai_chat");
        let mut provider = Provider::with_id(id.into(), name.into(), settings, None);
        provider.meta = Some(serde_json::from_value(meta).unwrap());
        db.save_provider(app, &provider).unwrap();
    }

    #[test]
    fn classifier_uses_shipped_pairs_and_defaults_missing_max_tokens() {
        for signature in SIGNATURES {
            let settings = json!({"config": format!("model_provider='p'\nmodel='{}'\n[model_providers.p]\nbase_url='{}'", signature.1, signature.0)});
            assert_eq!(
                classify(
                    "codex",
                    "Nexus",
                    &settings,
                    &json!({"apiFormat":"openai_chat"})
                ),
                Ownership::Upgrade
            );
        }
        let future = json!({
            "providerType":"nexus",
            "managedNexusPresetVersion":VERSION + 1
        });
        assert_eq!(
            classify("codex", "x", &json!({}), &future),
            Ownership::Current
        );
        let mut meta = json!({});
        merge_override(meta.as_object_mut().unwrap());
        assert_eq!(
            meta.pointer("/localProxyRequestOverrides/body/max_tokens"),
            Some(&json!(MAX_TOKENS))
        );
        let mut updated = json!({});
        merge_catalog(
            &json!({"modelCatalog":{"keep":true,"models":[]}}),
            &mut updated,
        );
        assert_eq!(updated["modelCatalog"]["keep"], true);
    }

    #[test]
    fn canonical_v4_provider_is_a_byte_for_byte_no_op() {
        let db = Database::memory().unwrap();
        save(
            &db,
            "codex",
            "v4",
            NAME,
            json!({
                "auth":{"OPENAI_API_KEY":"rotated-user-key"},
                "config":format!(
                    "model_provider='nexus'\nmodel='{MODEL}'\nmodel_context_window={CONTEXT}\nmodel_auto_compact_token_limit={COMPACT}\n[model_providers.nexus]\nbase_url='{ENDPOINT}'\nstream_idle_timeout_ms={STREAM_IDLE_MS}"
                ),
                "modelCatalog":{"owner":"user","models":[
                    {"model":MODEL,"displayName":"GLM-5.2","contextWindow":CONTEXT,"inputModalities":["text"]},
                    {"model":"user-model","displayName":"Keep me"}
                ]},
                "keep":"unchanged"
            }),
            json!({
                "providerType":"nexus",
                "managedNexusPresetVersion":VERSION,
                "commonConfigEnabled":true,
                "customUserAgent":"keep-agent",
                "localProxyRequestOverrides":{
                    "headers":{"x-keep":"yes"},
                    "body":{
                        "max_tokens":MAX_TOKENS,
                        "temperature":0.25,
                        "chat_template_kwargs":{"enable_thinking":true}
                    }
                }
            }),
        );
        db.set_current_provider("codex", "v4").unwrap();
        db.add_custom_endpoint("codex", "v4", ENDPOINT).unwrap();
        db.add_custom_endpoint("codex", "v4", "https://custom.example/v1")
            .unwrap();

        let snapshot = || {
            let conn = db.conn.lock().unwrap();
            let provider = conn
                .query_row(
                    "SELECT id, app_type, name, settings_config, website_url, category,
                            created_at, sort_index, notes, icon, icon_color, meta,
                            is_current, in_failover_queue
                     FROM providers WHERE id='v4' AND app_type='codex'",
                    [],
                    |row| {
                        (0..14)
                            .map(|index| row.get::<_, rusqlite::types::Value>(index))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    },
                )
                .unwrap();
            let mut statement = conn
                .prepare(
                    "SELECT url, added_at FROM provider_endpoints
                     WHERE provider_id='v4' AND app_type='codex' ORDER BY url",
                )
                .unwrap();
            let endpoints = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (provider, endpoints)
        };
        let before = snapshot();

        for _ in 0..2 {
            assert_eq!(
                db.migrate_legacy_nexus_providers(None, None, Some("v4"))
                    .unwrap(),
                NexusMigrationOutcome::default()
            );
            assert_eq!(snapshot(), before);
        }
    }

    #[test]
    fn credential_fingerprints_match_only_shipped_values() {
        let synthetic = "synthetic-fixture-secret";
        let digest = format!("{:x}", Sha256::digest(synthetic.as_bytes()));
        assert!(matches_credential_fingerprint(
            synthetic,
            &[digest.as_str()]
        ));
        assert!(!matches_credential_fingerprint(
            "rotated-fixture-secret",
            &[digest.as_str()]
        ));
        assert!(!is_leaked_nexus_credential("dummy"));
        assert!(!is_leaked_nexus_credential("nexus-local"));
        assert!(is_shipped_nexus_credential("dummy"));
        assert!(!is_shipped_nexus_credential("rotated-user-key"));
        assert!(SHIPPED_KEY_FINGERPRINTS
            .iter()
            .all(|fingerprint| fingerprint.len() == 64));
        let mut placeholder = json!({"env":{"ANTHROPIC_AUTH_TOKEN":"dummy"}});
        assert!(!scrub_leaked_nexus_credentials(&AppType::Claude, &mut placeholder).unwrap());
    }

    #[test]
    fn codex_scrub_checks_auth_top_level_and_every_provider_table() {
        let mut settings = json!({
            "auth":{"OPENAI_API_KEY":"leaked"},
            "config":"experimental_bearer_token='leaked'\n[model_providers.a]\nexperimental_bearer_token='leaked'\n[model_providers.b]\nexperimental_bearer_token='rotated'"
        });

        assert!(
            scrub_credentials_if(&AppType::Codex, &mut settings, |value| {
                value == "leaked"
            })
            .unwrap()
        );

        assert!(settings.pointer("/auth/OPENAI_API_KEY").is_none());
        let config = settings["config"].as_str().unwrap();
        assert!(!config.contains("'leaked'"));
        assert!(config.contains("'rotated'"));
    }

    #[test]
    fn scrub_only_current_rows_request_one_sanitized_live_projection() {
        for (app, id, name, settings, meta, credential_path, expected) in [
            (
                "codex",
                "future",
                "Future Nexus",
                json!({
                    "auth":{"OPENAI_API_KEY":"synthetic-leak"},
                    "config":"model_provider='p'\n[model_providers.p]\nbase_url='https://future.example/v1'"
                }),
                json!({"providerType":"nexus","managedNexusPresetVersion":VERSION + 1}),
                "/auth/OPENAI_API_KEY",
                NexusMigrationOutcome {
                    current_codex_changed: true,
                    ..Default::default()
                },
            ),
            (
                "claude",
                "unrelated",
                "Custom",
                json!({"env":{
                    "ANTHROPIC_BASE_URL":"https://custom.example/v1",
                    "ANTHROPIC_AUTH_TOKEN":"synthetic-leak"
                }}),
                json!({}),
                "/env/ANTHROPIC_AUTH_TOKEN",
                NexusMigrationOutcome {
                    current_claude_changed: true,
                    ..Default::default()
                },
            ),
            (
                "claude-desktop",
                "desktop",
                "Future Desktop",
                json!({"env":{
                    "ANTHROPIC_BASE_URL":"https://future.example/v1",
                    "ANTHROPIC_AUTH_TOKEN":"synthetic-leak",
                    "KEEP_ME":"unchanged"
                }}),
                json!({
                    "providerType":"nexus",
                    "managedNexusPresetVersion":VERSION + 1,
                    "claudeDesktopMode":"proxy",
                    "claudeDesktopModelRoutes":{"claude-sonnet-5":{"model":"future-model"}}
                }),
                "/env/ANTHROPIC_AUTH_TOKEN",
                NexusMigrationOutcome {
                    current_claude_desktop_changed: true,
                    ..Default::default()
                },
            ),
        ] {
            let db = Database::memory().unwrap();
            save(&db, app, id, name, settings, meta);
            let before = db.get_all_providers(app).unwrap()[id].clone();
            let outcome = db
                .migrate_legacy_nexus_providers_if(
                    (app == "claude").then_some(id),
                    (app == "claude-desktop").then_some(id),
                    (app == "codex").then_some(id),
                    |value| value == "synthetic-leak",
                )
                .unwrap();
            assert_eq!(outcome, expected);

            let after = db.get_all_providers(app).unwrap()[id].clone();
            let mut expected_settings = before.settings_config;
            expected_settings
                .pointer_mut(credential_path)
                .expect("fixture credential")
                .take();
            expected_settings
                .pointer_mut(credential_path.rsplit_once('/').unwrap().0)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .retain(|_, value| !value.is_null());
            assert_eq!(after.settings_config, expected_settings);
            assert_eq!(
                serde_json::to_value(&after.meta).unwrap(),
                serde_json::to_value(&before.meta).unwrap()
            );
            assert_eq!(after.name, before.name);

            assert_eq!(
                db.migrate_legacy_nexus_providers_if(
                    (app == "claude").then_some(id),
                    (app == "claude-desktop").then_some(id),
                    (app == "codex").then_some(id),
                    |value| value == "synthetic-leak",
                )
                .unwrap(),
                NexusMigrationOutcome::default()
            );
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    #[serial]
    fn scrubbed_current_claude_desktop_projects_live_idempotently() {
        let _home = TestHome::new();
        crate::settings::reload_settings().unwrap();
        let db = Arc::new(Database::memory().unwrap());
        save(
            &db,
            "claude-desktop",
            "desktop",
            "Future Desktop",
            json!({"env":{
                "ANTHROPIC_BASE_URL":"https://future.example/v1",
                "ANTHROPIC_AUTH_TOKEN":"synthetic-leak",
                "OPENAI_API_KEY":"rotated-key",
                "KEEP_ME":"unchanged"
            }}),
            json!({
                "providerType":"nexus",
                "managedNexusPresetVersion":VERSION + 1,
                "claudeDesktopMode":"proxy",
                "claudeDesktopModelRoutes":{"claude-sonnet-5":{
                    "model":"future-model","labelOverride":"Future Model"
                }}
            }),
        );
        db.set_current_provider("claude-desktop", "desktop")
            .unwrap();

        let outcome = db
            .migrate_legacy_nexus_providers_if(None, Some("desktop"), None, |value| {
                value == "synthetic-leak"
            })
            .unwrap();
        assert!(outcome.current_claude_desktop_changed);
        let state = crate::store::AppState::new(db.clone());
        crate::services::provider::ProviderService::sync_current_provider_for_app(
            &state,
            AppType::ClaudeDesktop,
        )
        .unwrap();

        let profile_path = crate::claude_desktop_config::get_config_library_path()
            .unwrap()
            .join(format!("{}.json", crate::claude_desktop_config::PROFILE_ID));
        let first = std::fs::read(&profile_path).unwrap();
        assert!(!String::from_utf8_lossy(&first).contains("synthetic-leak"));
        assert_eq!(
            db.get_all_providers("claude-desktop").unwrap()["desktop"].settings_config["env"]
                ["KEEP_ME"],
            "unchanged"
        );

        assert_eq!(
            db.migrate_legacy_nexus_providers_if(None, Some("desktop"), None, |value| {
                value == "synthetic-leak"
            })
            .unwrap(),
            NexusMigrationOutcome::default()
        );
        crate::services::provider::ProviderService::sync_current_provider_for_app(
            &state,
            AppType::ClaudeDesktop,
        )
        .unwrap();
        assert_eq!(std::fs::read(profile_path).unwrap(), first);
    }

    #[test]
    fn migration_rejects_malformed_provider_json_with_row_context() -> Result<(), AppError> {
        for column in ["settings_config", "meta"] {
            let db = Database::memory().unwrap();
            save(&db, "claude", "broken", "Broken", json!({}), json!({}));
            lock_conn!(db.conn)
                .execute(
                    &format!(
                        "UPDATE providers SET {column}=?1 WHERE id='broken' AND app_type='claude'"
                    ),
                    ["{not-json"],
                )
                .unwrap();

            let error = db
                .migrate_legacy_nexus_providers(None, None, None)
                .unwrap_err()
                .to_string();
            assert!(error.contains("claude provider 'broken'"), "{error}");
            assert!(error.contains(column), "{error}");
        }
        Ok(())
    }

    #[test]
    fn migration_rejects_malformed_applicable_live_backup() {
        let db = Database::memory().unwrap();
        futures::executor::block_on(db.save_live_backup("codex", "{not-json")).unwrap();

        let error = db
            .migrate_legacy_nexus_providers(None, None, None)
            .unwrap_err();
        assert!(error.to_string().contains("Invalid codex live backup JSON"));
    }

    #[test]
    fn migration_preserves_request_values_and_detaches_custom_current_codex() {
        let db = Database::memory().unwrap();
        save(
            &db,
            "claude",
            "claude",
            "Nexus",
            json!({"env": {
                "ANTHROPIC_BASE_URL": HOSTED_ENDPOINT,
                "ANTHROPIC_AUTH_TOKEN": "dummy",
                "ANTHROPIC_MODEL": LEGACY_MODEL,
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": LEGACY_MODEL,
                "ANTHROPIC_DEFAULT_SONNET_MODEL": LEGACY_MODEL,
                "ANTHROPIC_DEFAULT_OPUS_MODEL": LEGACY_MODEL,
                "KEEP_ME":"custom"
            }}),
            json!({"localProxyRequestOverrides":{"body":{"model":"keep","max_tokens":4096}}}),
        );
        save(
            &db,
            "codex",
            "current",
            "renamed",
            json!({
                "config": "model_provider='p'\nmodel='custom'\n[model_providers.p]\nbase_url='https://custom.example/v1'",
                "modelCatalog":{"models":[
                    {"model":MODEL,"inputModalities":["text"]},
                    {"model":MODEL,"displayName":"User GLM","keep":true}
                ]}
            }),
            json!({
                "providerType":"nexus", "managedNexusPresetVersion":2,
                "codexChatReasoning": owned_reasoning()
            }),
        );
        for endpoint in [
            LOCAL_ENDPOINT,
            DRAFT_ENDPOINT,
            HOSTED_ENDPOINT,
            ENDPOINT,
            "https://custom.example/v1",
        ] {
            db.add_custom_endpoint("codex", "current", endpoint)
                .unwrap();
        }

        let outcome = db
            .migrate_legacy_nexus_providers(None, None, Some("current"))
            .unwrap();
        assert_eq!(
            outcome,
            NexusMigrationOutcome {
                migrated: 2,
                current_claude_changed: false,
                current_claude_desktop_changed: false,
                current_codex_changed: true
            }
        );
        let claude = db.get_provider_by_id("claude", "claude").unwrap().unwrap();
        let body = claude
            .meta
            .unwrap()
            .local_proxy_request_overrides
            .unwrap()
            .body
            .unwrap();
        assert_eq!(
            (body["model"].as_str(), body["max_tokens"].as_u64()),
            (Some("keep"), Some(4096))
        );
        assert!(claude
            .settings_config
            .pointer("/env/ANTHROPIC_AUTH_TOKEN")
            .is_none());
        let env = claude.settings_config["env"].as_object().unwrap();
        for (key, expected) in [
            ("API_TIMEOUT_MS", "3000000"),
            ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", "252000"),
            ("CLAUDE_CODE_ATTRIBUTION_HEADER", "0"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("KEEP_ME", "custom"),
        ] {
            assert_eq!(env[key], expected);
        }
        let codex = db.get_all_providers("codex").unwrap()["current"].clone();
        let meta = codex.meta.as_ref().unwrap();
        assert!(meta.provider_type.is_none() && meta.codex_chat_reasoning.is_none());
        assert_eq!(meta.custom_endpoints.len(), 1);
        assert!(meta
            .custom_endpoints
            .contains_key("https://custom.example/v1"));
        assert_eq!(
            codex.settings_config.pointer("/modelCatalog/models"),
            Some(&json!([{"model":MODEL,"displayName":"User GLM","keep":true}]))
        );
    }

    #[test]
    fn migration_canonicalizes_desktop_routes() {
        let db = Database::memory().unwrap();
        save(
            &db,
            "claude-desktop",
            "desktop",
            "renamed",
            json!({"env":{"ANTHROPIC_BASE_URL":DRAFT_ENDPOINT}}),
            json!({
                "providerType":"nexus", "claudeDesktopMode":"proxy",
                "claudeDesktopModelRoutes":{"claude-sonnet-5":{
                    "model":LEGACY_MODEL,"labelOverride":"keep","supports1m":true
                }}
            }),
        );
        assert_eq!(
            db.migrate_legacy_nexus_providers(None, None, None)
                .unwrap()
                .migrated,
            1
        );
        assert_eq!(
            db.get_provider_by_id("desktop", "claude-desktop")
                .unwrap()
                .unwrap()
                .meta
                .unwrap()
                .claude_desktop_model_routes["claude-sonnet-5"]
                .model,
            MODEL
        );
    }

    #[test]
    fn migration_upgrades_v3_codex_stream_timeout() {
        let db = Database::memory().unwrap();
        save(
            &db,
            "codex",
            "codex",
            NAME,
            json!({"config":format!(
                "model_provider='p'\nmodel='{MODEL}'\n[model_providers.p]\nbase_url='{ENDPOINT}'\nstream_idle_timeout_ms=900000"
            )}),
            json!({"providerType":"nexus","managedNexusPresetVersion":3}),
        );

        let outcome = db
            .migrate_legacy_nexus_providers(None, None, Some("codex"))
            .unwrap();
        assert!(outcome.current_codex_changed);
        let provider = db.get_all_providers("codex").unwrap()["codex"].clone();
        let config = provider.settings_config["config"]
            .as_str()
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            config["model_providers"]["p"]["stream_idle_timeout_ms"].as_integer(),
            Some(3_000_000)
        );
    }

    #[test]
    fn managed_update_preserves_user_catalog_and_detach_removes_only_owned_defaults() {
        let settings = json!({
            "config":format!("model_provider='p'\nmodel='{MODEL}'\n[model_providers.p]\nbase_url='{ENDPOINT}'"),
            "modelCatalog": {
                "owner": "user",
                "models": [
                    {"model": MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT, "inputModalities":["text"]},
                    {"model": MODEL, "displayName": "User alias", "keep": true}
                ]
            }
        });
        let mut existing = Provider::with_id("nexus".into(), NAME.into(), settings, None);
        existing.meta = Some(
            serde_json::from_value(json!({
                "providerType":"nexus",
                "managedNexusPresetVersion":VERSION,
                "apiFormat":"openai_chat",
                "localProxyRequestOverrides":{"headers":{"x-keep":"yes"},"body":{
                    "max_tokens":MAX_TOKENS,
                    "temperature":0.25,
                    "chat_template_kwargs":{"enable_thinking":true,"keep":"yes"}
                }}
            }))
            .unwrap(),
        );
        let mut updated = existing.clone();
        updated.settings_config["modelCatalog"]["models"] = json!([
            {"model": MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT, "inputModalities":["text"]}
        ]);

        reconcile_managed_nexus_update(&AppType::Codex, &existing, &mut updated);

        assert_eq!(
            updated.settings_config["modelCatalog"],
            json!({"owner":"user","models":[
                {"model": MODEL, "displayName": "GLM-5.2", "contextWindow": CONTEXT, "inputModalities":["text"]},
                {"model": MODEL, "displayName": "User alias", "keep": true}
            ]})
        );

        let mut detached = existing.clone();
        detached.settings_config["config"] = json!(
            "model_provider='p'\nmodel='custom'\n[model_providers.p]\nbase_url='https://custom.example/v1'"
        );
        detached.meta.as_mut().unwrap().provider_type = None;

        reconcile_managed_nexus_update(&AppType::Codex, &existing, &mut detached);

        let meta = serde_json::to_value(detached.meta.unwrap()).unwrap();
        assert_eq!(
            meta["localProxyRequestOverrides"],
            json!({"headers":{"x-keep":"yes"},"body":{
                "temperature":0.25,"chat_template_kwargs":{"keep":"yes"}
            }})
        );
        assert_eq!(
            detached.settings_config["modelCatalog"]["models"],
            json!([{"model": MODEL, "displayName": "User alias", "keep": true}])
        );
    }
}
