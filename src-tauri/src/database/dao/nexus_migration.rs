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
const NEXUS_MANAGED_PRESET_VERSION: u32 = 1;
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
        None => LEGACY_ENDPOINTS.contains(&endpoint) && legacy_models.contains(&model),
        Some("nexus") => {
            (endpoint == NEXUS_ENDPOINT || LEGACY_ENDPOINTS.contains(&endpoint))
                && (current_models.contains(&model) || legacy_models.contains(&model))
        }
        _ => false,
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
        if let Some(entry) = models
            .iter_mut()
            .find(|entry| entry.get("model").and_then(Value::as_str) == Some(*required_model))
        {
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
    let index = models
        .iter()
        .position(|entry| {
            entry
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| [LEGACY_MODEL, NEXUS_MODEL].contains(&model))
        })
        .unwrap_or_else(|| {
            models.push(json!({}));
            models.len() - 1
        });
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
            if !LEGACY_NAMES.contains(&name.as_str()) {
                continue;
            }
            let (Ok(mut settings), Ok(mut meta)) = (
                serde_json::from_str::<Value>(&settings_json),
                serde_json::from_str::<Value>(&meta_json),
            ) else {
                continue;
            };
            if meta
                .get("managedNexusPresetVersion")
                .and_then(Value::as_u64)
                .is_some_and(|version| version >= u64::from(NEXUS_MANAGED_PRESET_VERSION))
            {
                continue;
            }
            let original_settings = settings.clone();
            let original_meta = meta.clone();
            let provider_type = meta
                .get("providerType")
                .and_then(Value::as_str)
                .map(str::to_string);
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
        assert_eq!(meta.managed_nexus_preset_version, Some(1));
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
