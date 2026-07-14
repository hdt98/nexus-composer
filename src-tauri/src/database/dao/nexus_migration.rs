use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde_json::{json, Map, Value};
use toml_edit::{value, DocumentMut};

// Keep aligned with src/config/nexus.ts.
const NEXUS_NAME: &str = "Nexus GLM-5.2";
const NEXUS_ENDPOINT: &str = "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
const NEXUS_MODEL: &str = "GLM-5.2-FP8";
const NEXUS_CLAUDE_MODEL: &str = "GLM-5.2-FP8[1m]";
const NEXUS_CONTEXT_WINDOW: i64 = 1_048_576;
const NEXUS_AUTO_COMPACT_TOKENS: i64 = 252_000;
const NEXUS_MANAGED_PRESET_VERSION: u32 = 2;
const LEGACY_MODEL: &str = "glm-5.2";
const LEGACY_CLAUDE_MODEL: &str = "glm-5.2[1m]";
const LEGACY_CLAUDE_DESKTOP_ID: &str = "nexus-glm-5-2-hosted";
const LEGACY_ENDPOINTS: [&str; 3] = [
    "http://127.0.0.1:30000/v1",
    "http://127.0.0.1:30001/v1",
    "https://glm-test-glm52-tp4.onenexus-do.cloud/v1",
];
const LEGACY_NAMES: [&str; 4] = ["Nexus", "Nexus Local", "Nexus GLM-5.2 Hosted", NEXUS_NAME];

type ProviderRow = (String, String, String, String, Option<String>, String);

fn is_managed_signature(
    endpoint: Option<&str>,
    model: Option<&str>,
    provider_type: Option<&str>,
    current_models: &[&str],
    legacy_models: &[&str],
) -> bool {
    let (Some(endpoint), Some(model)) = (endpoint, model) else {
        return false;
    };
    match provider_type {
        None => {
            (endpoint == NEXUS_ENDPOINT || LEGACY_ENDPOINTS.contains(&endpoint))
                && (current_models.contains(&model) || legacy_models.contains(&model))
        }
        Some("nexus") => {
            (endpoint == NEXUS_ENDPOINT || LEGACY_ENDPOINTS.contains(&endpoint))
                && (current_models.contains(&model) || legacy_models.contains(&model))
        }
        _ => false,
    }
}

fn is_custom_signature(
    endpoint: Option<&str>,
    model: Option<&str>,
    current_models: &[&str],
    legacy_models: &[&str],
) -> bool {
    endpoint.is_some()
        && model.is_some()
        && !is_managed_signature(
            endpoint,
            model,
            Some("nexus"),
            current_models,
            legacy_models,
        )
}

fn is_customized_v1_target(app_type: &str, id: &str, settings: &Value, meta: &Value) -> bool {
    match app_type {
        "claude" => is_custom_signature(
            settings
                .pointer("/env/ANTHROPIC_BASE_URL")
                .and_then(Value::as_str),
            settings
                .pointer("/env/ANTHROPIC_MODEL")
                .and_then(Value::as_str),
            &[NEXUS_MODEL, NEXUS_CLAUDE_MODEL],
            &[LEGACY_MODEL, LEGACY_CLAUDE_MODEL],
        ),
        "codex" => {
            let Some(config) = settings.get("config").and_then(Value::as_str) else {
                return false;
            };
            let Ok(document) = config.parse::<DocumentMut>() else {
                return false;
            };
            let endpoint = crate::codex_config::extract_codex_base_url(config);
            is_custom_signature(
                endpoint.as_deref(),
                document.get("model").and_then(|item| item.as_str()),
                &[NEXUS_MODEL],
                &[LEGACY_MODEL],
            )
        }
        "claude-desktop" => {
            settings
                .pointer("/env/ANTHROPIC_BASE_URL")
                .and_then(Value::as_str)
                .is_some()
                && meta
                    .get("claudeDesktopModelRoutes")
                    .and_then(Value::as_object)
                    .is_some_and(|routes| {
                        !routes.is_empty()
                            && routes
                                .values()
                                .all(|route| route.get("model").and_then(Value::as_str).is_some())
                    })
                && !is_managed_claude_desktop(id, settings, meta, Some("nexus"))
        }
        _ => false,
    }
}

fn remove_managed_nexus_catalog(settings: &mut Value) {
    let Some(settings) = settings.as_object_mut() else {
        return;
    };
    let remove_catalog = {
        let Some(catalog) = settings
            .get_mut("modelCatalog")
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let Some(models) = catalog.get_mut("models").and_then(Value::as_array_mut) else {
            return;
        };
        models.retain(|entry| {
            entry.get("role").is_some()
                || !entry
                    .get("model")
                    .and_then(Value::as_str)
                    .is_some_and(|model| {
                        [
                            NEXUS_MODEL,
                            NEXUS_CLAUDE_MODEL,
                            LEGACY_MODEL,
                            LEGACY_CLAUDE_MODEL,
                        ]
                        .contains(&model)
                    })
        });
        models.is_empty() && catalog.len() == 1
    };
    if remove_catalog {
        settings.remove("modelCatalog");
    }
}

fn remove_managed_reasoning_override(meta: &mut Map<String, Value>) {
    let remove_overrides = {
        let Some(overrides) = meta
            .get_mut("localProxyRequestOverrides")
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let remove_body =
            if let Some(body) = overrides.get_mut("body").and_then(Value::as_object_mut) {
                let remove_template = if let Some(template) = body
                    .get_mut("chat_template_kwargs")
                    .and_then(Value::as_object_mut)
                {
                    if template.get("enable_thinking") == Some(&json!(true)) {
                        template.remove("enable_thinking");
                    }
                    template.is_empty()
                } else {
                    false
                };
                if remove_template {
                    body.remove("chat_template_kwargs");
                }
                body.is_empty()
            } else {
                false
            };
        if remove_body {
            overrides.remove("body");
        }
        overrides.is_empty()
    };
    if remove_overrides {
        meta.remove("localProxyRequestOverrides");
    }
}

fn catalog_models_mut(settings: &mut Value) -> Option<&mut Vec<Value>> {
    let settings = settings.as_object_mut()?;
    let catalog = settings
        .entry("modelCatalog".to_string())
        .or_insert_with(|| json!({ "models": [] }))
        .as_object_mut()?;
    catalog
        .entry("models".to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
}

fn ensure_text_only_models(settings: &mut Value, required: &[&str]) -> bool {
    let Some(models) = catalog_models_mut(settings) else {
        return false;
    };
    for required_model in required {
        if let Some(entry) = models.iter_mut().find(|entry| {
            entry.get("role").is_none()
                && entry.get("model").and_then(Value::as_str) == Some(*required_model)
        }) {
            let Some(entry) = entry.as_object_mut() else {
                return false;
            };
            entry.insert("inputModalities".to_string(), json!(["text"]));
        } else {
            models.push(json!({
                "model": required_model,
                "inputModalities": ["text"]
            }));
        }
    }
    true
}

fn upsert_codex_model(settings: &mut Value) -> bool {
    let Some(models) = catalog_models_mut(settings) else {
        return false;
    };
    let is_nexus = |entry: &Value| {
        entry.get("role").is_none()
            && entry
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| [LEGACY_MODEL, NEXUS_MODEL].contains(&model))
    };
    let mut merged = models
        .iter()
        .find(|entry| {
            entry.get("role").is_none()
                && entry.get("model").and_then(Value::as_str) == Some(NEXUS_MODEL)
        })
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for entry in models.iter().filter(|entry| is_nexus(entry)) {
        let Some(entry) = entry.as_object() else {
            return false;
        };
        for (key, value) in entry {
            merged.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    let index = if models.iter().any(is_nexus) {
        let mut kept = false;
        models.retain(|entry| {
            if !is_nexus(entry) {
                true
            } else if kept {
                false
            } else {
                kept = true;
                true
            }
        });
        let index = models
            .iter()
            .position(is_nexus)
            .expect("retained Nexus catalog entry");
        models[index] = Value::Object(merged);
        index
    } else {
        models.push(Value::Object(merged));
        models.len() - 1
    };
    let Some(entry) = models[index].as_object_mut() else {
        return false;
    };
    entry.insert("model".to_string(), json!(NEXUS_MODEL));
    entry.insert("displayName".to_string(), json!("GLM-5.2"));
    entry.insert("contextWindow".to_string(), json!(NEXUS_CONTEXT_WINDOW));
    entry.insert("inputModalities".to_string(), json!(["text"]));
    true
}

fn migrate_claude_settings(settings: &mut Value, provider_type: Option<&str>) -> bool {
    let Some(env) = settings.get("env").and_then(Value::as_object) else {
        return false;
    };
    if !is_managed_signature(
        env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str),
        env.get("ANTHROPIC_MODEL").and_then(Value::as_str),
        provider_type,
        &[NEXUS_MODEL, NEXUS_CLAUDE_MODEL],
        &[LEGACY_MODEL, LEGACY_CLAUDE_MODEL],
    ) {
        return false;
    }

    let env = settings
        .get_mut("env")
        .and_then(Value::as_object_mut)
        .expect("validated Claude env");
    env.insert("ANTHROPIC_BASE_URL".to_string(), json!(NEXUS_ENDPOINT));
    for field in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
    ] {
        env.insert(field.to_string(), json!(NEXUS_CLAUDE_MODEL));
    }
    for (field, value) in [
        ("ANTHROPIC_CUSTOM_MODEL_OPTION", NEXUS_CLAUDE_MODEL),
        ("ANTHROPIC_CUSTOM_MODEL_OPTION_NAME", NEXUS_NAME),
        (
            "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
            "GLM-5.2 through Nexus",
        ),
    ] {
        env.insert(field.to_string(), json!(value));
    }
    env.insert("API_TIMEOUT_MS".to_string(), json!("3000000"));
    env.insert(
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW".to_string(),
        json!(NEXUS_AUTO_COMPACT_TOKENS.to_string()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string(),
        json!("1"),
    );
    env.insert("CLAUDE_CODE_ATTRIBUTION_HEADER".to_string(), json!("0"));
    env.remove("ANTHROPIC_CUSTOM_MODEL_OPTION_SUPPORTED_CAPABILITIES");
    ensure_text_only_models(settings, &[NEXUS_MODEL, LEGACY_MODEL])
}

fn is_managed_claude_desktop(
    id: &str,
    settings: &Value,
    meta: &Value,
    provider_type: Option<&str>,
) -> bool {
    let Some(endpoint) = settings
        .pointer("/env/ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
    else {
        return false;
    };
    if endpoint != NEXUS_ENDPOINT && !LEGACY_ENDPOINTS.contains(&endpoint) {
        return false;
    }
    if meta.get("claudeDesktopMode").and_then(Value::as_str) != Some("proxy")
        || !matches!(
            meta.get("apiFormat").and_then(Value::as_str),
            Some("anthropic" | "openai_chat")
        )
        || (provider_type != Some("nexus")
            && !(provider_type.is_none() && id == LEGACY_CLAUDE_DESKTOP_ID))
    {
        return false;
    }

    meta.get("claudeDesktopModelRoutes")
        .and_then(Value::as_object)
        .filter(|routes| !routes.is_empty())
        .is_some_and(|routes| {
            routes.iter().all(|(route_id, route)| {
                crate::claude_desktop_config::is_claude_safe_model_id(route_id)
                    && route
                        .get("model")
                        .and_then(Value::as_str)
                        .is_some_and(|model| {
                            [
                                NEXUS_MODEL,
                                NEXUS_CLAUDE_MODEL,
                                LEGACY_MODEL,
                                LEGACY_CLAUDE_MODEL,
                            ]
                            .contains(&model)
                        })
            })
        })
}

fn migrate_claude_desktop_settings(settings: &mut Value) -> bool {
    let Some(env) = settings.get_mut("env").and_then(Value::as_object_mut) else {
        return false;
    };
    env.insert("ANTHROPIC_BASE_URL".to_string(), json!(NEXUS_ENDPOINT));
    ensure_text_only_models(settings, &[NEXUS_MODEL, LEGACY_MODEL])
}

fn migrate_codex_settings(settings: &mut Value, provider_type: Option<&str>) -> bool {
    let Some(config) = settings.get("config").and_then(Value::as_str) else {
        return false;
    };
    let Ok(mut document) = config.parse::<DocumentMut>() else {
        return false;
    };
    if !is_managed_signature(
        crate::codex_config::extract_codex_base_url(config).as_deref(),
        document.get("model").and_then(|item| item.as_str()),
        provider_type,
        &[NEXUS_MODEL],
        &[LEGACY_MODEL],
    ) {
        return false;
    }
    let Some(active_provider) = document
        .get("model_provider")
        .and_then(|item| item.as_str())
        .map(str::to_string)
    else {
        return false;
    };

    document["model"] = value(NEXUS_MODEL);
    document["model_context_window"] = value(NEXUS_CONTEXT_WINDOW);
    document["model_auto_compact_token_limit"] = value(NEXUS_AUTO_COMPACT_TOKENS);
    document.as_table_mut().remove("model_reasoning_effort");
    document["model_providers"][&active_provider]["base_url"] = value(NEXUS_ENDPOINT);
    settings["config"] = json!(document.to_string());
    upsert_codex_model(settings)
}

fn merge_reasoning_override(meta: &mut Map<String, Value>) -> bool {
    if let Some(overrides) = meta.get("localProxyRequestOverrides") {
        let Some(overrides) = overrides.as_object() else {
            return false;
        };
        if serde_json::from_value::<crate::provider::LocalProxyRequestOverrides>(Value::Object(
            overrides.clone(),
        ))
        .is_err()
        {
            return false;
        }
        if let Some(body) = overrides.get("body") {
            let Some(body) = body.as_object() else {
                return false;
            };
            if let Some(model) = body.get("model") {
                let Some(model) = model.as_str() else {
                    return false;
                };
                if ![
                    NEXUS_MODEL,
                    NEXUS_CLAUDE_MODEL,
                    LEGACY_MODEL,
                    LEGACY_CLAUDE_MODEL,
                ]
                .contains(&model)
                {
                    return false;
                }
            }
            if body
                .get("chat_template_kwargs")
                .is_some_and(|value| !value.is_object())
            {
                return false;
            }
        }
    }

    let overrides = meta
        .entry("localProxyRequestOverrides".to_string())
        .or_insert_with(|| json!({}));
    let Some(overrides) = overrides.as_object_mut() else {
        return false;
    };
    let body = overrides
        .entry("body".to_string())
        .or_insert_with(|| json!({}));
    let Some(body) = body.as_object_mut() else {
        return false;
    };
    if body.get("max_tokens").and_then(Value::as_u64) == Some(4096) {
        body.remove("max_tokens");
    }
    body.remove("model");
    let template_kwargs = body
        .entry("chat_template_kwargs".to_string())
        .or_insert_with(|| json!({}));
    let Some(template_kwargs) = template_kwargs.as_object_mut() else {
        return false;
    };
    template_kwargs.insert("enable_thinking".to_string(), json!(true));
    true
}

impl Database {
    /// Upgrade only exact historical or unversioned managed Nexus presets.
    pub(crate) fn migrate_legacy_nexus_providers(&self) -> Result<usize, AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction()?;
        let rows: Vec<ProviderRow> = {
            let mut statement = tx.prepare(
                "SELECT id, app_type, name, settings_config, website_url, meta
                 FROM providers WHERE app_type IN ('claude', 'claude-desktop', 'codex')",
            )?;
            let mapped = statement.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?;
            mapped.collect::<rusqlite::Result<_>>()?
        };

        let mut migrated = 0;
        for (id, app_type, name, settings_json, website_url, meta_json) in rows {
            let (Ok(mut settings), Ok(mut meta)) = (
                serde_json::from_str::<Value>(&settings_json),
                serde_json::from_str::<Value>(&meta_json),
            ) else {
                continue;
            };
            let preset_version = meta
                .get("managedNexusPresetVersion")
                .and_then(Value::as_u64);
            if preset_version
                .is_some_and(|version| version >= u64::from(NEXUS_MANAGED_PRESET_VERSION))
            {
                continue;
            }
            let provider_type = meta
                .get("providerType")
                .and_then(Value::as_str)
                .map(str::to_string);
            let is_version_one_managed =
                preset_version == Some(1) && provider_type.as_deref() == Some("nexus");
            if !is_version_one_managed && !LEGACY_NAMES.contains(&name.as_str()) {
                continue;
            }
            let original_settings = settings.clone();
            let original_meta = meta.clone();
            let recognized = match app_type.as_str() {
                "claude" => migrate_claude_settings(&mut settings, provider_type.as_deref()),
                "claude-desktop" => {
                    is_managed_claude_desktop(&id, &settings, &meta, provider_type.as_deref())
                        && migrate_claude_desktop_settings(&mut settings)
                }
                "codex" => migrate_codex_settings(&mut settings, provider_type.as_deref()),
                _ => false,
            };
            if !recognized {
                if is_version_one_managed
                    && is_customized_v1_target(&app_type, &id, &settings, &meta)
                {
                    remove_managed_nexus_catalog(&mut settings);
                    let Some(meta_object) = meta.as_object_mut() else {
                        continue;
                    };
                    meta_object.remove("providerType");
                    meta_object.remove("managedNexusPresetVersion");
                    remove_managed_reasoning_override(meta_object);
                    tx.execute(
                        "UPDATE providers SET settings_config = ?1, meta = ?2
                         WHERE id = ?3 AND app_type = ?4",
                        params![settings.to_string(), meta.to_string(), id, app_type],
                    )?;
                    migrated += 1;
                }
                continue;
            }

            let Some(meta_object) = meta.as_object_mut() else {
                continue;
            };
            meta_object.insert("providerType".to_string(), json!("nexus"));
            meta_object.insert("apiFormat".to_string(), json!("openai_chat"));
            if app_type == "claude-desktop" {
                meta_object.insert("claudeDesktopMode".to_string(), json!("proxy"));
            }
            if !merge_reasoning_override(meta_object) {
                continue;
            }
            if app_type == "codex" {
                meta_object.insert(
                    "codexChatReasoning".to_string(),
                    json!({
                        "supportsThinking": true,
                        "supportsEffort": false,
                        "thinkingParam": "chat_template_kwargs.enable_thinking",
                        "effortParam": "none",
                        "outputFormat": "reasoning_content"
                    }),
                );
            }
            meta_object.insert(
                "managedNexusPresetVersion".to_string(),
                json!(NEXUS_MANAGED_PRESET_VERSION),
            );
            settings
                .as_object_mut()
                .expect("recognized settings are objects")
                .remove("nexusCapabilities");

            let row_changed = name != NEXUS_NAME
                || website_url.as_deref() != Some(NEXUS_ENDPOINT)
                || settings != original_settings
                || meta != original_meta;
            if row_changed {
                tx.execute(
                    "UPDATE providers
                     SET name = ?1, settings_config = ?2, website_url = ?3, meta = ?4
                     WHERE id = ?5 AND app_type = ?6",
                    params![
                        NEXUS_NAME,
                        settings.to_string(),
                        NEXUS_ENDPOINT,
                        meta.to_string(),
                        id,
                        app_type,
                    ],
                )?;
            }
            let deleted = tx.execute(
                "DELETE FROM provider_endpoints
                 WHERE provider_id = ?1 AND app_type = ?2 AND url IN (?3, ?4, ?5)",
                params![
                    id,
                    app_type,
                    LEGACY_ENDPOINTS[0],
                    LEGACY_ENDPOINTS[1],
                    LEGACY_ENDPOINTS[2],
                ],
            )?;
            let inserted = tx.execute(
                "INSERT INTO provider_endpoints (provider_id, app_type, url, added_at)
                 SELECT ?1, ?2, ?3, ?4 WHERE NOT EXISTS (
                     SELECT 1 FROM provider_endpoints
                     WHERE provider_id = ?1 AND app_type = ?2 AND url = ?3
                 )",
                params![
                    id,
                    app_type,
                    NEXUS_ENDPOINT,
                    chrono::Utc::now().timestamp_millis(),
                ],
            )?;
            if row_changed || deleted > 0 || inserted > 0 {
                migrated += 1;
            }
        }

        tx.commit()?;
        Ok(migrated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        ClaudeDesktopMode, ClaudeDesktopModelRoute, LocalProxyRequestOverrides, Provider,
        ProviderMeta,
    };
    use std::collections::HashMap;

    fn save_provider(
        db: &Database,
        app: &str,
        id: &str,
        name: &str,
        endpoint: &str,
        provider_type: Option<&str>,
        settings: Value,
    ) {
        let mut provider = Provider::with_id(
            id.to_string(),
            name.to_string(),
            settings,
            Some(endpoint.to_string()),
        );
        provider.notes = Some("keep-note".to_string());
        provider.meta = Some(ProviderMeta {
            provider_type: provider_type.map(str::to_string),
            custom_user_agent: Some("keep-agent".to_string()),
            ..ProviderMeta::default()
        });
        db.save_provider(app, &provider).unwrap();
        db.add_custom_endpoint(app, id, endpoint).unwrap();
        db.add_custom_endpoint(app, id, "https://debug.example/v1")
            .unwrap();
    }

    #[test]
    fn migrates_managed_claude_and_codex_once_without_losing_siblings() {
        let db = Database::memory().unwrap();
        save_provider(
            &db,
            "claude",
            "claude-nexus",
            "Nexus Local",
            LEGACY_ENDPOINTS[2],
            None,
            json!({
                "keep": {"nested": true},
                "env": {
                    "ANTHROPIC_BASE_URL": LEGACY_ENDPOINTS[2],
                    "ANTHROPIC_AUTH_TOKEN": "keep-claude-key",
                    "ANTHROPIC_MODEL": LEGACY_MODEL
                }
            }),
        );
        let mut claude = db
            .get_provider_by_id("claude-nexus", "claude")
            .unwrap()
            .unwrap();
        claude.meta.as_mut().unwrap().local_proxy_request_overrides =
            Some(LocalProxyRequestOverrides {
                headers: HashMap::from([("keep-header".to_string(), "yes".to_string())]),
                body: Some(json!({
                    "model": LEGACY_MODEL,
                    "max_tokens": 4096,
                    "keep": true,
                    "chat_template_kwargs": {"keep": true}
                })),
            });
        db.save_provider("claude", &claude).unwrap();
        save_provider(
            &db,
            "codex",
            "codex-nexus",
            "Nexus",
            LEGACY_ENDPOINTS[0],
            None,
            json!({
                "auth": {"OPENAI_API_KEY": "keep-codex-key"},
                "config": format!(
                    "model_provider = \"custom\"\nmodel = \"{}\"\nmodel_reasoning_effort = \"high\"\nkeep = true\n[model_providers.custom]\nbase_url = \"{}\"\nwire_api = \"responses\"\n",
                    LEGACY_MODEL, LEGACY_ENDPOINTS[0]
                ),
                "modelCatalog": {"models": [{"model": LEGACY_MODEL, "keep": true}]},
                "keepTop": true
            }),
        );

        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 2);

        let claude = db
            .get_all_providers("claude")
            .unwrap()
            .get("claude-nexus")
            .unwrap()
            .clone();
        assert_eq!(
            claude.settings_config.pointer("/env/ANTHROPIC_AUTH_TOKEN"),
            Some(&json!("keep-claude-key"))
        );
        assert_eq!(
            claude.settings_config.pointer("/keep/nested"),
            Some(&json!(true))
        );
        assert_eq!(
            claude
                .settings_config
                .pointer("/modelCatalog/models/0/inputModalities"),
            Some(&json!(["text"]))
        );
        let meta = claude.meta.as_ref().unwrap();
        assert_eq!(
            meta.managed_nexus_preset_version,
            Some(NEXUS_MANAGED_PRESET_VERSION)
        );
        let body = meta
            .local_proxy_request_overrides
            .as_ref()
            .and_then(|overrides| overrides.body.as_ref())
            .unwrap();
        assert!(body.get("model").is_none());
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body.get("keep"), Some(&json!(true)));
        assert_eq!(
            body.pointer("/chat_template_kwargs/enable_thinking"),
            Some(&json!(true))
        );
        assert!(meta.custom_endpoints.contains_key(NEXUS_ENDPOINT));
        assert!(meta
            .custom_endpoints
            .contains_key("https://debug.example/v1"));

        let codex = db
            .get_provider_by_id("codex-nexus", "codex")
            .unwrap()
            .unwrap();
        assert_eq!(
            codex.settings_config.pointer("/auth/OPENAI_API_KEY"),
            Some(&json!("keep-codex-key"))
        );
        assert_eq!(codex.settings_config.get("keepTop"), Some(&json!(true)));
        let config = codex.settings_config["config"]
            .as_str()
            .unwrap()
            .parse::<toml::Value>()
            .unwrap();
        assert_eq!(config["model"].as_str(), Some(NEXUS_MODEL));
        assert!(config.get("model_reasoning_effort").is_none());
        assert_eq!(
            config["model_auto_compact_token_limit"].as_integer(),
            Some(NEXUS_AUTO_COMPACT_TOKENS)
        );
        assert_eq!(
            codex
                .settings_config
                .pointer("/modelCatalog/models/0/inputModalities"),
            Some(&json!(["text"]))
        );
        assert_eq!(
            codex.settings_config.pointer("/modelCatalog/models/0/keep"),
            Some(&json!(true))
        );

        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 0);
    }

    #[test]
    fn codex_catalog_coalesces_current_and_legacy_entries_in_either_order() {
        for models in [
            json!([
                {"model": LEGACY_MODEL, "legacyOnly": true, "shared": "legacy"},
                {"model": "custom-model", "keep": true},
                {"model": NEXUS_MODEL, "role": "custom", "keep": true},
                {"model": NEXUS_MODEL, "currentOnly": true, "shared": "current"}
            ]),
            json!([
                {"model": NEXUS_MODEL, "currentOnly": true, "shared": "current"},
                {"model": NEXUS_MODEL, "role": "custom", "keep": true},
                {"model": "custom-model", "keep": true},
                {"model": LEGACY_MODEL, "legacyOnly": true, "shared": "legacy"}
            ]),
        ] {
            let mut settings = json!({"modelCatalog": {"models": models}});
            assert!(upsert_codex_model(&mut settings));
            let models = settings
                .pointer("/modelCatalog/models")
                .and_then(Value::as_array)
                .unwrap();
            assert_eq!(
                models
                    .iter()
                    .filter(|entry| {
                        entry.get("role").is_none()
                            && entry.get("model") == Some(&json!(NEXUS_MODEL))
                    })
                    .count(),
                1
            );
            assert!(!models
                .iter()
                .any(|entry| entry.get("model") == Some(&json!(LEGACY_MODEL))));
            assert!(models.contains(&json!({"model": "custom-model", "keep": true})));
            assert!(models.contains(&json!({"model": NEXUS_MODEL, "role": "custom", "keep": true})));
            let nexus = models
                .iter()
                .find(|entry| entry.get("model") == Some(&json!(NEXUS_MODEL)))
                .unwrap();
            assert_eq!(nexus.get("legacyOnly"), Some(&json!(true)));
            assert_eq!(nexus.get("currentOnly"), Some(&json!(true)));
            assert_eq!(nexus.get("shared"), Some(&json!("current")));
            assert_eq!(nexus.get("inputModalities"), Some(&json!(["text"])));
        }
    }

    #[test]
    fn migrates_known_partial_cutover_pairs_without_marker() {
        let db = Database::memory().unwrap();
        let pairs = [
            (NEXUS_ENDPOINT, NEXUS_MODEL, NEXUS_CLAUDE_MODEL),
            (NEXUS_ENDPOINT, LEGACY_MODEL, LEGACY_CLAUDE_MODEL),
            (LEGACY_ENDPOINTS[0], NEXUS_MODEL, NEXUS_CLAUDE_MODEL),
        ];
        let mut providers = Vec::new();
        for (index, (endpoint, codex_model, claude_model)) in pairs.into_iter().enumerate() {
            let claude_id = format!("claude-partial-{index}");
            save_provider(
                &db,
                "claude",
                &claude_id,
                NEXUS_NAME,
                endpoint,
                None,
                json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": endpoint,
                        "ANTHROPIC_AUTH_TOKEN": "keep-claude-key",
                        "ANTHROPIC_MODEL": claude_model
                    }
                }),
            );
            providers.push(("claude", claude_id));

            let codex_id = format!("codex-partial-{index}");
            save_provider(
                &db,
                "codex",
                &codex_id,
                NEXUS_NAME,
                endpoint,
                None,
                json!({
                    "auth": {"OPENAI_API_KEY": "keep-codex-key"},
                    "config": format!(
                        "model_provider = \"custom\"\nmodel = \"{}\"\n[model_providers.custom]\nbase_url = \"{}\"\nwire_api = \"responses\"\n",
                        codex_model, endpoint
                    )
                }),
            );
            providers.push(("codex", codex_id));
        }

        assert_eq!(
            db.migrate_legacy_nexus_providers().unwrap(),
            providers.len()
        );
        for (app, id) in providers {
            let provider = db.get_provider_by_id(&id, app).unwrap().unwrap();
            assert_eq!(
                provider
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.managed_nexus_preset_version),
                Some(NEXUS_MANAGED_PRESET_VERSION)
            );
        }
    }

    #[test]
    fn upgrades_version_one_managed_row_once() {
        let db = Database::memory().unwrap();
        save_provider(
            &db,
            "claude",
            "claude-v1",
            NEXUS_NAME,
            NEXUS_ENDPOINT,
            Some("nexus"),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": NEXUS_ENDPOINT,
                    "ANTHROPIC_AUTH_TOKEN": "keep-key",
                    "ANTHROPIC_MODEL": NEXUS_CLAUDE_MODEL
                }
            }),
        );
        let mut provider = db
            .get_provider_by_id("claude-v1", "claude")
            .unwrap()
            .unwrap();
        provider.meta.as_mut().unwrap().managed_nexus_preset_version = Some(1);
        db.save_provider("claude", &provider).unwrap();

        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 1);
        assert_eq!(
            db.get_provider_by_id("claude-v1", "claude")
                .unwrap()
                .unwrap()
                .meta
                .and_then(|meta| meta.managed_nexus_preset_version),
            Some(NEXUS_MANAGED_PRESET_VERSION)
        );
        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 0);
    }

    #[test]
    fn handles_renamed_version_one_managed_and_custom_rows() {
        let db = Database::memory().unwrap();
        for (id, name, endpoint, model) in [
            (
                "renamed-managed",
                "My Nexus",
                NEXUS_ENDPOINT,
                NEXUS_CLAUDE_MODEL,
            ),
            (
                "renamed-custom",
                "My Custom GLM",
                "https://custom.example/v1",
                "custom-model",
            ),
        ] {
            save_provider(
                &db,
                "claude",
                id,
                name,
                endpoint,
                Some("nexus"),
                json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": endpoint,
                        "ANTHROPIC_AUTH_TOKEN": "keep-key",
                        "ANTHROPIC_MODEL": model
                    },
                    "modelCatalog": {"models": [
                        {"model": NEXUS_MODEL},
                        {"model": "custom-model", "keep": true}
                    ]}
                }),
            );
            let mut provider = db.get_provider_by_id(id, "claude").unwrap().unwrap();
            let meta = provider.meta.as_mut().unwrap();
            meta.managed_nexus_preset_version = Some(1);
            meta.local_proxy_request_overrides = Some(LocalProxyRequestOverrides {
                headers: HashMap::new(),
                body: Some(json!({
                    "chat_template_kwargs": {"enable_thinking": true}
                })),
            });
            db.save_provider("claude", &provider).unwrap();
        }

        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 2);
        let managed = db
            .get_provider_by_id("renamed-managed", "claude")
            .unwrap()
            .unwrap();
        assert_eq!(managed.name, NEXUS_NAME);
        assert_eq!(
            managed.meta.unwrap().managed_nexus_preset_version,
            Some(NEXUS_MANAGED_PRESET_VERSION)
        );

        let customized = db
            .get_provider_by_id("renamed-custom", "claude")
            .unwrap()
            .unwrap();
        assert_eq!(customized.name, "My Custom GLM");
        let meta = customized.meta.unwrap();
        assert_eq!(meta.provider_type, None);
        assert_eq!(meta.managed_nexus_preset_version, None);
        assert!(meta.local_proxy_request_overrides.is_none());
        assert_eq!(
            customized.settings_config.pointer("/modelCatalog/models"),
            Some(&json!([{"model": "custom-model", "keep": true}]))
        );
        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 0);
    }

    #[test]
    fn version_one_custom_target_drops_only_managed_state() {
        let db = Database::memory().unwrap();
        save_provider(
            &db,
            "claude",
            "claude-custom-v1",
            NEXUS_NAME,
            "https://custom.example/v1",
            Some("nexus"),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://custom.example/v1",
                    "ANTHROPIC_AUTH_TOKEN": "keep-key",
                    "ANTHROPIC_MODEL": "custom-model"
                },
                "modelCatalog": {"models": [
                    {"model": NEXUS_MODEL},
                    {"model": NEXUS_CLAUDE_MODEL},
                    {"model": LEGACY_MODEL},
                    {"model": LEGACY_CLAUDE_MODEL},
                    {"model": NEXUS_MODEL, "role": "custom", "keep": true},
                    {"model": "custom-model", "keep": true}
                ]},
                "keepTop": true
            }),
        );
        let mut provider = db
            .get_provider_by_id("claude-custom-v1", "claude")
            .unwrap()
            .unwrap();
        let meta = provider.meta.as_mut().unwrap();
        meta.managed_nexus_preset_version = Some(1);
        meta.local_proxy_request_overrides = Some(LocalProxyRequestOverrides {
            headers: HashMap::from([("keep-header".to_string(), "yes".to_string())]),
            body: Some(json!({
                "keep": true,
                "chat_template_kwargs": {
                    "enable_thinking": true,
                    "keep_template": true
                }
            })),
        });
        db.save_provider("claude", &provider).unwrap();

        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 1);
        let cleaned = db
            .get_provider_by_id("claude-custom-v1", "claude")
            .unwrap()
            .unwrap();
        assert_eq!(
            cleaned.settings_config.pointer("/env/ANTHROPIC_BASE_URL"),
            Some(&json!("https://custom.example/v1"))
        );
        assert_eq!(
            cleaned.settings_config.pointer("/env/ANTHROPIC_MODEL"),
            Some(&json!("custom-model"))
        );
        assert_eq!(
            cleaned.settings_config.pointer("/env/ANTHROPIC_AUTH_TOKEN"),
            Some(&json!("keep-key"))
        );
        assert_eq!(cleaned.settings_config.get("keepTop"), Some(&json!(true)));
        assert_eq!(
            cleaned.settings_config.pointer("/modelCatalog/models"),
            Some(&json!([
                {"model": NEXUS_MODEL, "role": "custom", "keep": true},
                {"model": "custom-model", "keep": true}
            ]))
        );
        let meta = cleaned.meta.unwrap();
        assert_eq!(meta.provider_type, None);
        assert_eq!(meta.managed_nexus_preset_version, None);
        let overrides = meta.local_proxy_request_overrides.unwrap();
        assert_eq!(
            overrides.headers.get("keep-header"),
            Some(&"yes".to_string())
        );
        assert_eq!(
            overrides.body.as_ref().unwrap().get("keep"),
            Some(&json!(true))
        );
        assert_eq!(
            overrides
                .body
                .as_ref()
                .unwrap()
                .pointer("/chat_template_kwargs/keep_template"),
            Some(&json!(true))
        );
        assert!(overrides
            .body
            .as_ref()
            .unwrap()
            .pointer("/chat_template_kwargs/enable_thinking")
            .is_none());
        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 0);
    }

    #[test]
    fn migrates_desktop_without_replacing_credentials_routes_or_catalog_siblings() {
        let db = Database::memory().unwrap();
        let mut provider = Provider::with_id(
            LEGACY_CLAUDE_DESKTOP_ID.to_string(),
            "Nexus GLM-5.2 Hosted".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": LEGACY_ENDPOINTS[1],
                    "ANTHROPIC_AUTH_TOKEN": "keep-desktop-key"
                },
                "modelCatalog": {"models": [
                    {"model": LEGACY_MODEL, "inputModalities": ["text", "image"], "keep": true},
                    {"model": "custom-model", "keep": true}
                ]}
            }),
            Some(LEGACY_ENDPOINTS[1].to_string()),
        );
        let routes = HashMap::from([(
            "claude-sonnet-5".to_string(),
            ClaudeDesktopModelRoute {
                model: LEGACY_MODEL.to_string(),
                label_override: Some("keep-label".to_string()),
                supports_1m: Some(true),
            },
        )]);
        provider.meta = Some(ProviderMeta {
            claude_desktop_mode: Some(ClaudeDesktopMode::Proxy),
            api_format: Some("anthropic".to_string()),
            claude_desktop_model_routes: routes.clone(),
            ..ProviderMeta::default()
        });
        db.save_provider("claude-desktop", &provider).unwrap();

        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 1);
        let migrated = db
            .get_provider_by_id(LEGACY_CLAUDE_DESKTOP_ID, "claude-desktop")
            .unwrap()
            .unwrap();
        assert_eq!(
            migrated
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN"),
            Some(&json!("keep-desktop-key"))
        );
        assert_eq!(
            migrated
                .settings_config
                .pointer("/modelCatalog/models/0/keep"),
            Some(&json!(true))
        );
        assert_eq!(
            migrated.settings_config.pointer("/modelCatalog/models/1"),
            Some(&json!({"model": "custom-model", "keep": true}))
        );
        assert_eq!(
            migrated.meta.as_ref().unwrap().claude_desktop_model_routes,
            routes
        );
    }

    #[test]
    fn rejects_unrelated_or_malformed_overrides_without_partial_updates() {
        let db = Database::memory().unwrap();
        let mut malformed_headers = Map::from_iter([(
            "localProxyRequestOverrides".to_string(),
            json!({"headers": {"x-test": 7}}),
        )]);
        assert!(!merge_reasoning_override(&mut malformed_headers));
        let mut custom_cap = Map::from_iter([(
            "localProxyRequestOverrides".to_string(),
            json!({"body": {"model": LEGACY_MODEL, "max_tokens": 8192}}),
        )]);
        assert!(merge_reasoning_override(&mut custom_cap));
        assert_eq!(
            custom_cap
                .get("localProxyRequestOverrides")
                .and_then(|value| value.pointer("/body/max_tokens")),
            Some(&json!(8192))
        );

        for (id, body) in [
            ("unrelated", json!({"model": "another-model", "keep": true})),
            ("malformed", json!({"model": 7, "keep": true})),
        ] {
            save_provider(
                &db,
                "claude",
                id,
                NEXUS_NAME,
                LEGACY_ENDPOINTS[1],
                Some("nexus"),
                json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": LEGACY_ENDPOINTS[1],
                        "ANTHROPIC_AUTH_TOKEN": format!("keep-{id}-key"),
                        "ANTHROPIC_MODEL": NEXUS_CLAUDE_MODEL
                    }
                }),
            );
            let mut provider = db.get_provider_by_id(id, "claude").unwrap().unwrap();
            provider
                .meta
                .as_mut()
                .unwrap()
                .local_proxy_request_overrides = Some(LocalProxyRequestOverrides {
                body: Some(body),
                ..LocalProxyRequestOverrides::default()
            });
            db.save_provider("claude", &provider).unwrap();
        }
        let before: Vec<_> = ["unrelated", "malformed"]
            .into_iter()
            .map(|id| {
                serde_json::to_value(db.get_all_providers("claude").unwrap().get(id).unwrap())
                    .unwrap()
            })
            .collect();

        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 0);
        for (index, id) in ["unrelated", "malformed"].into_iter().enumerate() {
            let after =
                serde_json::to_value(db.get_all_providers("claude").unwrap().get(id).unwrap())
                    .unwrap();
            assert_eq!(after, before[index]);
        }
    }

    #[test]
    fn ignores_similarly_named_custom_model() {
        let db = Database::memory().unwrap();
        save_provider(
            &db,
            "claude",
            "custom",
            NEXUS_NAME,
            LEGACY_ENDPOINTS[0],
            None,
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": LEGACY_ENDPOINTS[0],
                    "ANTHROPIC_AUTH_TOKEN": "keep-key",
                    "ANTHROPIC_MODEL": "another-model"
                }
            }),
        );
        let before = db.get_provider_by_id("custom", "claude").unwrap().unwrap();
        assert_eq!(db.migrate_legacy_nexus_providers().unwrap(), 0);
        assert_eq!(
            serde_json::to_value(db.get_provider_by_id("custom", "claude").unwrap().unwrap())
                .unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}
