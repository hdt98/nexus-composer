use crate::app_config::AppType;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde_json::{json, Value};
use toml_edit::{value, DocumentMut};

const HOSTED_ENDPOINT: &str = "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
const LEGACY_ENDPOINT: &str = "https://glm-test-glm52-tp4.onenexus-do.cloud/v1";
const MODEL: &str = "GLM-5.2-FP8";
const CLAUDE_MODEL: &str = "GLM-5.2-FP8[1m]";
const MANAGED_VERSION: u64 = 7;
const CONTEXT_WINDOW: i64 = 1_048_576;
const COMPACT_TOKENS: i64 = 252_000;
const MAX_OUTPUT_TOKENS: u64 = 65_536;

fn recognized_endpoint(value: &str) -> bool {
    matches!(
        value.trim_end_matches('/'),
        LEGACY_ENDPOINT | HOSTED_ENDPOINT
    )
}

fn recognized_model(value: &str) -> bool {
    matches!(
        value.trim_end_matches("[1m]"),
        "GLM-5.2-FP8" | "glm-5.2" | "GLM-5.2-SGLang"
    )
}

fn has_hosted_signature(app: &AppType, settings: &Value) -> bool {
    match app {
        AppType::Codex => {
            let Some(config) = settings.get("config").and_then(Value::as_str) else {
                return false;
            };
            let Ok(document) = config.parse::<DocumentMut>() else {
                return false;
            };
            crate::codex_config::extract_codex_base_url(config)
                .as_deref()
                .is_some_and(recognized_endpoint)
                && document
                    .get("model")
                    .and_then(|model| model.as_str())
                    .is_some_and(recognized_model)
        }
        AppType::Claude => {
            let Some(env) = settings.get("env") else {
                return false;
            };
            env.get("ANTHROPIC_BASE_URL")
                .and_then(Value::as_str)
                .is_some_and(recognized_endpoint)
                && env
                    .get("ANTHROPIC_MODEL")
                    .and_then(Value::as_str)
                    .is_some_and(recognized_model)
        }
        AppType::ClaudeDesktop => {
            let Some(env) = settings.get("env") else {
                return false;
            };
            env.get("ANTHROPIC_BASE_URL")
                .and_then(Value::as_str)
                .is_some_and(recognized_endpoint)
                && settings["modelCatalog"]["models"]
                    .as_array()
                    .is_some_and(|models| {
                        models.iter().any(|entry| {
                            entry
                                .get("model")
                                .and_then(Value::as_str)
                                .is_some_and(recognized_model)
                        })
                    })
        }
        _ => false,
    }
}

fn is_legacy_preset(app: &AppType, name: &str, settings: &Value, meta: &Value) -> bool {
    if !matches!(name, "Nexus" | "Nexus GLM-5.2")
        || meta.get("providerType").is_some()
        || meta.get("managedNexusPresetVersion").is_some()
        || meta.get("apiFormat").and_then(Value::as_str) != Some("openai_chat")
    {
        return false;
    }

    match app {
        AppType::Codex => {
            let Some(config) = settings.get("config").and_then(Value::as_str) else {
                return false;
            };
            let Ok(document) = config.parse::<DocumentMut>() else {
                return false;
            };
            let Some(active) = document
                .get("model_provider")
                .and_then(|provider| provider.as_str())
            else {
                return false;
            };
            let provider = &document["model_providers"][active];
            document.get("model").and_then(|model| model.as_str()) == Some("glm-5.2")
                && crate::codex_config::extract_codex_base_url(config).as_deref()
                    == Some(LEGACY_ENDPOINT)
                && provider.get("name").and_then(|name| name.as_str()) == Some("nexus_glm")
                && provider.get("wire_api").and_then(|wire| wire.as_str()) == Some("responses")
        }
        AppType::Claude => {
            let Some(env) = settings.get("env").and_then(Value::as_object) else {
                return false;
            };
            env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str) == Some(LEGACY_ENDPOINT)
                && [
                    "ANTHROPIC_MODEL",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL",
                ]
                .into_iter()
                .all(|key| env.get(key).and_then(Value::as_str) == Some("glm-5.2"))
        }
        _ => false,
    }
}

fn merge_text_only_catalog(settings: &mut Value, app: &AppType) -> Result<(), AppError> {
    let settings = settings
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus settings must be an object".into()))?;
    let catalog = settings
        .entry("modelCatalog")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus modelCatalog must be an object".into()))?;
    let mut models = match catalog.remove("models") {
        Some(Value::Array(models)) => models,
        Some(_) => {
            return Err(AppError::Message(
                "Nexus modelCatalog.models must be an array".into(),
            ));
        }
        None => Vec::new(),
    };
    models.retain(|entry| {
        entry
            .get("model")
            .and_then(Value::as_str)
            .is_none_or(|model| !recognized_model(model))
    });
    let managed = match app {
        AppType::Codex => json!({
            "model": MODEL,
            "displayName": "GLM-5.2",
            "contextWindow": CONTEXT_WINDOW,
            "inputModalities": ["text"]
        }),
        _ => json!({"model": MODEL, "inputModalities": ["text"]}),
    };
    models.insert(0, managed);
    catalog.insert("models".into(), Value::Array(models));
    Ok(())
}

fn merge_request_defaults(meta: &mut Value) -> Result<(), AppError> {
    let meta = meta
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus metadata must be an object".into()))?;
    let overrides = meta
        .entry("localProxyRequestOverrides")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus request overrides must be an object".into()))?;
    let body = overrides
        .entry("body")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus request body override must be an object".into()))?;
    body.entry("max_tokens")
        .or_insert_with(|| json!(MAX_OUTPUT_TOKENS));
    let template = body
        .entry("chat_template_kwargs")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            AppError::Message("Nexus chat template override must be an object".into())
        })?;
    template
        .entry("enable_thinking")
        .or_insert_with(|| json!(true));
    template
        .entry("clear_thinking")
        .or_insert_with(|| json!(false));
    meta.insert("providerType".into(), json!("nexus"));
    meta.insert("managedNexusPresetVersion".into(), json!(MANAGED_VERSION));
    meta.insert("apiFormat".into(), json!("openai_chat"));
    Ok(())
}

fn merge_desktop_routes(meta: &mut Value) -> Result<(), AppError> {
    let meta = meta
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus metadata must be an object".into()))?;
    let routes = meta
        .entry("claudeDesktopModelRoutes")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus Desktop routes must be an object".into()))?;
    for route in [
        "claude-sonnet-5",
        "claude-opus-4-8",
        "claude-fable-5",
        "claude-haiku-4-5",
    ] {
        routes.insert(
            route.into(),
            json!({"model": MODEL, "labelOverride": MODEL, "supports1m": true}),
        );
    }
    meta.insert("claudeDesktopMode".into(), json!("proxy"));
    Ok(())
}

fn upgrade_settings(app: &AppType, settings: &mut Value, meta: &mut Value) -> Result<(), AppError> {
    match app {
        AppType::Codex => {
            let config = settings
                .get("config")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::Message("Nexus Codex config is missing".into()))?;
            let mut document = config.parse::<DocumentMut>().map_err(|error| {
                AppError::Message(format!("Invalid Nexus Codex config: {error}"))
            })?;
            let active = document
                .get("model_provider")
                .and_then(|provider| provider.as_str())
                .ok_or_else(|| AppError::Message("Nexus Codex model_provider is missing".into()))?
                .to_string();
            document["model"] = value(MODEL);
            document["model_context_window"] = value(CONTEXT_WINDOW);
            document["model_auto_compact_token_limit"] = value(COMPACT_TOKENS);
            document.as_table_mut().remove("model_reasoning_effort");
            document["model_providers"][&active]["base_url"] = value(HOSTED_ENDPOINT);
            document["model_providers"][&active]["wire_api"] = value("responses");
            document["model_providers"][&active]["requires_openai_auth"] = value(true);
            document["model_providers"][&active]["stream_idle_timeout_ms"] = value(3_000_000);
            settings["config"] = json!(document.to_string());
        }
        AppType::Claude => {
            let env = settings
                .get_mut("env")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| AppError::Message("Nexus Claude env must be an object".into()))?;
            env.insert("ANTHROPIC_BASE_URL".into(), json!(HOSTED_ENDPOINT));
            for key in [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_FABLE_MODEL",
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
        }
        AppType::ClaudeDesktop => {
            let env = settings
                .get_mut("env")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    AppError::Message("Nexus Claude Desktop env must be an object".into())
                })?;
            env.insert("ANTHROPIC_BASE_URL".into(), json!(HOSTED_ENDPOINT));
            merge_desktop_routes(meta)?;
        }
        _ => return Ok(()),
    }
    merge_text_only_catalog(settings, app)?;
    merge_request_defaults(meta)
}

impl Database {
    pub(crate) fn migrate_managed_nexus_for_app(
        &self,
        app: &AppType,
        current_id: Option<&str>,
    ) -> Result<bool, AppError> {
        if !matches!(
            app,
            AppType::Claude | AppType::ClaudeDesktop | AppType::Codex
        ) {
            return Ok(false);
        }
        let app_name = app.as_str();
        let mut connection = lock_conn!(self.conn);
        let transaction = connection.transaction()?;
        let rows = {
            let mut statement = transaction
                .prepare("SELECT id,name,settings_config,meta FROM providers WHERE app_type=?1")?;
            let rows = statement
                .query_map([app_name], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let mut sync_current = false;
        for (id, name, raw_settings, raw_meta) in rows {
            let Ok(mut settings) = serde_json::from_str::<Value>(&raw_settings) else {
                continue;
            };
            let Ok(mut meta) = serde_json::from_str::<Value>(&raw_meta) else {
                continue;
            };
            let version = meta
                .get("managedNexusPresetVersion")
                .and_then(Value::as_u64);
            if version.is_some_and(|version| version > MANAGED_VERSION) {
                continue;
            }
            let explicitly_managed =
                meta.get("providerType").and_then(Value::as_str) == Some("nexus");
            let legacy = is_legacy_preset(app, &name, &settings, &meta);
            if !explicitly_managed && !legacy {
                continue;
            }

            if explicitly_managed && !has_hosted_signature(app, &settings) {
                let Some(meta) = meta.as_object_mut() else {
                    continue;
                };
                meta.remove("providerType");
                meta.remove("managedNexusPresetVersion");
            } else if version != Some(MANAGED_VERSION) {
                upgrade_settings(app, &mut settings, &mut meta).map_err(|error| {
                    AppError::Message(format!(
                        "Cannot upgrade {app_name} Nexus provider '{id}': {error}"
                    ))
                })?;
            } else {
                continue;
            }

            let migrated_name = if legacy { "Nexus GLM-5.2" } else { &name };
            transaction.execute(
                "UPDATE providers SET name=?1,settings_config=?2,meta=?3 WHERE id=?4 AND app_type=?5",
                params![
                    migrated_name,
                    settings.to_string(),
                    meta.to_string(),
                    id,
                    app_name
                ],
            )?;
            sync_current |= current_id == Some(id.as_str());
        }
        transaction.commit()?;
        Ok(sync_current)
    }
}

#[cfg(test)]
mod tests {
    use super::MANAGED_VERSION;
    use crate::app_config::AppType;
    use crate::database::Database;
    use crate::provider::Provider;
    use serde_json::json;

    const LEGACY_ENDPOINT: &str = "https://glm-test-glm52-tp4.onenexus-do.cloud/v1";
    const HOSTED_ENDPOINT: &str = "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";

    fn save_provider(
        db: &Database,
        app: AppType,
        id: &str,
        name: &str,
        settings: serde_json::Value,
        meta: serde_json::Value,
    ) {
        let mut provider = Provider::with_id(id.into(), name.into(), settings, None);
        provider.meta = Some(serde_json::from_value(meta).unwrap());
        db.save_provider(app.as_str(), &provider).unwrap();
    }

    #[test]
    fn migrates_exact_legacy_codex_and_claude_presets_without_losing_user_fields() {
        let db = Database::memory().unwrap();
        save_provider(
            &db,
            AppType::Codex,
            "codex",
            "Nexus",
            json!({
                "auth":{"OPENAI_API_KEY":"user-key"},
                "config":format!("model_provider='custom'\nmodel='glm-5.2'\nmodel_reasoning_effort='high'\n[model_providers.custom]\nname='nexus_glm'\nbase_url='{LEGACY_ENDPOINT}'\nwire_api='responses'\nrequires_openai_auth=true"),
                "modelCatalog":{"models":[{"model":"user-model","inputModalities":["image"]}]}
            }),
            json!({
                "apiFormat":"openai_chat",
                "customUserAgent":"keep-agent",
                "localProxyRequestOverrides":{"headers":{"x-keep":"yes"},"body":{"temperature":0.2}}
            }),
        );
        save_provider(
            &db,
            AppType::Claude,
            "claude",
            "Nexus GLM-5.2",
            json!({"env":{
                "ANTHROPIC_BASE_URL":LEGACY_ENDPOINT,
                "ANTHROPIC_AUTH_TOKEN":"user-key",
                "ANTHROPIC_MODEL":"glm-5.2",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL":"glm-5.2",
                "ANTHROPIC_DEFAULT_SONNET_MODEL":"glm-5.2",
                "ANTHROPIC_DEFAULT_OPUS_MODEL":"glm-5.2",
                "USER_SETTING":"keep"
            }}),
            json!({"apiFormat":"openai_chat"}),
        );

        assert!(db
            .migrate_managed_nexus_for_app(&AppType::Codex, Some("codex"))
            .unwrap());
        assert!(db
            .migrate_managed_nexus_for_app(&AppType::Claude, Some("claude"))
            .unwrap());

        let codex = db
            .get_provider_by_id("codex", AppType::Codex.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(codex.settings_config["auth"]["OPENAI_API_KEY"], "user-key");
        assert_eq!(
            codex.meta.as_ref().unwrap().custom_user_agent.as_deref(),
            Some("keep-agent")
        );
        assert_eq!(
            codex
                .meta
                .as_ref()
                .unwrap()
                .local_proxy_request_overrides
                .as_ref()
                .unwrap()
                .headers["x-keep"],
            "yes"
        );
        assert_eq!(
            codex.settings_config["modelCatalog"]["models"][1]["model"],
            "user-model"
        );
        let config = codex.settings_config["config"].as_str().unwrap();
        let document = config.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(document["model"].as_str(), Some("GLM-5.2-FP8"));
        assert_eq!(
            document["model_providers"]["custom"]["base_url"].as_str(),
            Some(HOSTED_ENDPOINT)
        );
        assert_eq!(
            document["model_providers"]["custom"]["stream_idle_timeout_ms"].as_integer(),
            Some(3_000_000)
        );
        assert!(document.get("model_reasoning_effort").is_none());

        let claude = db
            .get_provider_by_id("claude", AppType::Claude.as_str())
            .unwrap()
            .unwrap();
        let env = &claude.settings_config["env"];
        assert_eq!(env["ANTHROPIC_BASE_URL"], HOSTED_ENDPOINT);
        assert_eq!(env["ANTHROPIC_AUTH_TOKEN"], "user-key");
        assert_eq!(env["ANTHROPIC_MODEL"], "GLM-5.2-FP8[1m]");
        assert_eq!(env["API_TIMEOUT_MS"], "3000000");
        assert_eq!(env["CLAUDE_CODE_ATTRIBUTION_HEADER"], "0");
        assert_eq!(env["USER_SETTING"], "keep");
    }

    #[test]
    fn preserves_loopback_and_custom_providers_and_detaches_stale_ownership() {
        let db = Database::memory().unwrap();
        for (id, endpoint, managed) in [
            ("loopback", "http://127.0.0.1:30001/v1", false),
            ("custom", "https://custom.example/v1", true),
        ] {
            save_provider(
                &db,
                AppType::Claude,
                id,
                "Nexus GLM-5.2",
                json!({"env":{
                    "ANTHROPIC_BASE_URL":endpoint,
                    "ANTHROPIC_MODEL":"glm-5.2"
                }}),
                if managed {
                    json!({"providerType":"nexus","managedNexusPresetVersion":6,"apiFormat":"openai_chat"})
                } else {
                    json!({"apiFormat":"openai_chat"})
                },
            );
        }

        assert!(!db
            .migrate_managed_nexus_for_app(&AppType::Claude, Some("other"))
            .unwrap());
        let loopback = db
            .get_provider_by_id("loopback", AppType::Claude.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(
            loopback.settings_config["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:30001/v1"
        );
        let custom = db
            .get_provider_by_id("custom", AppType::Claude.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(
            custom.settings_config["env"]["ANTHROPIC_BASE_URL"],
            "https://custom.example/v1"
        );
        let custom_meta = custom.meta.unwrap();
        assert!(custom_meta.provider_type.is_none());
        assert!(custom_meta.managed_nexus_preset_version.is_none());
        assert_eq!(custom_meta.api_format.as_deref(), Some("openai_chat"));
    }

    #[test]
    fn skips_future_versions_and_is_idempotent() {
        let db = Database::memory().unwrap();
        save_provider(
            &db,
            AppType::Claude,
            "future",
            "Nexus GLM-5.2",
            json!({"env":{
                "ANTHROPIC_BASE_URL":HOSTED_ENDPOINT,
                "ANTHROPIC_MODEL":"GLM-5.2-FP8[1m]"
            }}),
            json!({"providerType":"nexus","managedNexusPresetVersion":999,"apiFormat":"openai_chat"}),
        );
        assert!(!db
            .migrate_managed_nexus_for_app(&AppType::Claude, Some("future"))
            .unwrap());

        save_provider(
            &db,
            AppType::Claude,
            "managed",
            "Nexus GLM-5.2",
            json!({"env":{
                "ANTHROPIC_BASE_URL":HOSTED_ENDPOINT,
                "ANTHROPIC_MODEL":"GLM-5.2-FP8[1m]"
            }}),
            json!({"providerType":"nexus","managedNexusPresetVersion":6,"apiFormat":"openai_chat"}),
        );
        assert!(db
            .migrate_managed_nexus_for_app(&AppType::Claude, Some("managed"))
            .unwrap());
        assert!(!db
            .migrate_managed_nexus_for_app(&AppType::Claude, Some("managed"))
            .unwrap());
    }

    #[test]
    fn migrates_managed_claude_desktop_preset() {
        let db = Database::memory().unwrap();
        save_provider(
            &db,
            AppType::ClaudeDesktop,
            "desktop",
            "Nexus GLM-5.2",
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": HOSTED_ENDPOINT,
                    "ANTHROPIC_AUTH_TOKEN": "user-key"
                },
                "modelCatalog": {"models": [{"model": "GLM-5.2-FP8"}]}
            }),
            json!({
                "providerType": "nexus",
                "managedNexusPresetVersion": 6,
                "apiFormat": "openai_chat",
                "claudeDesktopMode": "proxy",
                "claudeDesktopModelRoutes": {
                    "claude-sonnet-5": {"model": "GLM-5.2-FP8", "supports1m": true}
                }
            }),
        );

        assert!(db
            .migrate_managed_nexus_for_app(&AppType::ClaudeDesktop, Some("desktop"))
            .unwrap());
        let provider = db
            .get_provider_by_id("desktop", AppType::ClaudeDesktop.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"],
            "user-key"
        );
        assert_eq!(
            provider.settings_config["env"]["ANTHROPIC_BASE_URL"],
            HOSTED_ENDPOINT
        );
        assert_eq!(
            provider.settings_config["modelCatalog"]["models"][0]["inputModalities"],
            json!(["text"])
        );
        let meta = provider.meta.unwrap();
        assert_eq!(
            meta.managed_nexus_preset_version,
            Some(MANAGED_VERSION as u32)
        );
        assert_eq!(meta.api_format.as_deref(), Some("openai_chat"));
        assert_eq!(
            meta.claude_desktop_mode.as_ref(),
            Some(&crate::provider::ClaudeDesktopMode::Proxy)
        );
        for route in [
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-fable-5",
            "claude-haiku-4-5",
        ] {
            let route = meta
                .claude_desktop_model_routes
                .get(route)
                .expect("managed Desktop route");
            assert_eq!(route.model, "GLM-5.2-FP8");
            assert_eq!(route.supports_1m, Some(true));
        }
        let body = meta
            .local_proxy_request_overrides
            .as_ref()
            .and_then(|overrides| overrides.body.as_ref())
            .expect("managed request defaults");
        assert_eq!(body["chat_template_kwargs"]["clear_thinking"], false);
    }
}
