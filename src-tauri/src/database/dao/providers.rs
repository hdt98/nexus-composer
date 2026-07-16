use crate::app_config::AppType;
use crate::codex_config::CC_SWITCH_CODEX_MODEL_CATALOG_FILENAME;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::provider::{Provider, ProviderMeta};
use indexmap::IndexMap;
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use toml_edit::{value, DocumentMut};

type OmoProviderRow = (
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<usize>,
    Option<String>,
    String,
);

const NEXUS_ENDPOINT: &str = "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
const NEXUS_MODEL: &str = "GLM-5.2-FP8";
const NEXUS_CLAUDE_MODEL: &str = "GLM-5.2-FP8[1m]";
const NEXUS_VERSION: u64 = 6;
const NEXUS_CONTEXT: i64 = 1_048_576;
const NEXUS_COMPACT: i64 = 252_000;
const NEXUS_MAX_TOKENS: u64 = 65_536;
const LEGACY_NEXUS_ENDPOINT: &str = "https://glm-test-glm52-tp4.onenexus-do.cloud/v1";
const LEGACY_NEXUS_MODEL: &str = "glm-5.2";
const LEGACY_NEXUS_NAMES: [&str; 2] = ["Nexus", "Nexus GLM-5.2"];
const NEXUS_ENDPOINTS: [&str; 4] = [
    "http://127.0.0.1:30000/v1",
    "http://127.0.0.1:30001/v1",
    LEGACY_NEXUS_ENDPOINT,
    NEXUS_ENDPOINT,
];
const SHIPPED_KEY_FINGERPRINTS: [&str; 2] = [
    "305da1291142205e2260675309af8963e2394257e2851f9c58759272c85eab73",
    "56c3568cabcc36524156db1191c4cd361710ebb88837d8b9457b2f59986465dc",
];

fn scrub_credentials(app: &AppType, settings: &mut Value, fingerprints: &[&str]) -> bool {
    let paths: &[&str] = match app {
        AppType::Codex => &["/auth/OPENAI_API_KEY"],
        AppType::Claude | AppType::ClaudeDesktop => {
            &["/env/ANTHROPIC_AUTH_TOKEN", "/env/ANTHROPIC_API_KEY"]
        }
        _ => &[],
    };
    let mut changed = false;
    for path in paths {
        let leaked = settings
            .pointer(path)
            .and_then(Value::as_str)
            .is_some_and(|value| {
                let digest = format!("{:x}", Sha256::digest(value.trim().as_bytes()));
                fingerprints.contains(&digest.as_str())
            });
        if leaked {
            *settings.pointer_mut(path).expect("credential path exists") = json!("");
            changed = true;
        }
    }
    changed
}

fn is_known_nexus_endpoint(value: &str) -> bool {
    let value = value.trim_end_matches('/');
    NEXUS_ENDPOINTS
        .iter()
        .any(|candidate| candidate.trim_end_matches('/') == value)
}

fn is_known_nexus_model(value: &str) -> bool {
    matches!(
        value.trim_end_matches("[1m]"),
        "GLM-5.2-FP8" | "glm-5.2" | "GLM-5.2-SGLang"
    )
}

fn known_nexus_pair(endpoint: Option<&str>, model: Option<&str>) -> bool {
    endpoint.is_some_and(is_known_nexus_endpoint) && model.is_some_and(is_known_nexus_model)
}

fn has_nexus_signature(app: &AppType, settings: &Value, meta: &Value) -> bool {
    match app {
        AppType::Codex => {
            let Some(config) = settings.get("config").and_then(Value::as_str) else {
                return false;
            };
            let Ok(document) = config.parse::<DocumentMut>() else {
                return false;
            };
            known_nexus_pair(
                crate::codex_config::extract_codex_base_url(config).as_deref(),
                document.get("model").and_then(|model| model.as_str()),
            )
        }
        AppType::Claude => known_nexus_pair(
            settings
                .pointer("/env/ANTHROPIC_BASE_URL")
                .and_then(Value::as_str),
            settings
                .pointer("/env/ANTHROPIC_MODEL")
                .and_then(Value::as_str),
        ),
        AppType::ClaudeDesktop => {
            let routes = meta
                .get("claudeDesktopModelRoutes")
                .and_then(Value::as_object);
            settings
                .pointer("/env/ANTHROPIC_BASE_URL")
                .and_then(Value::as_str)
                .is_some_and(is_known_nexus_endpoint)
                && routes.is_some_and(|routes| {
                    !routes.is_empty()
                        && routes.values().all(|route| {
                            route
                                .get("model")
                                .and_then(Value::as_str)
                                .is_some_and(is_known_nexus_model)
                        })
                })
        }
        _ => false,
    }
}

fn is_shipped_legacy_nexus(app: &AppType, name: &str, settings: &Value, meta: &Value) -> bool {
    if !LEGACY_NEXUS_NAMES.contains(&name)
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
                .and_then(|item| item.as_str())
            else {
                return false;
            };
            let provider = &document["model_providers"][active];
            document.get("model").and_then(|item| item.as_str()) == Some(LEGACY_NEXUS_MODEL)
                && crate::codex_config::extract_codex_base_url(config).as_deref()
                    == Some(LEGACY_NEXUS_ENDPOINT)
                && provider.get("name").and_then(|item| item.as_str()) == Some("nexus_glm")
                && provider.get("wire_api").and_then(|item| item.as_str()) == Some("responses")
                && provider
                    .get("requires_openai_auth")
                    .and_then(|item| item.as_bool())
                    == Some(true)
        }
        AppType::Claude => {
            let Some(env) = settings.get("env").and_then(Value::as_object) else {
                return false;
            };
            env.get("ANTHROPIC_BASE_URL").and_then(Value::as_str) == Some(LEGACY_NEXUS_ENDPOINT)
                && [
                    "ANTHROPIC_MODEL",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL",
                ]
                .into_iter()
                .all(|key| env.get(key).and_then(Value::as_str) == Some(LEGACY_NEXUS_MODEL))
        }
        _ => false,
    }
}

fn replace_nexus_catalog(settings: &mut Value, app: &AppType) -> Result<(), AppError> {
    let settings = settings
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus settings must be an object".into()))?;
    let catalog = settings
        .entry("modelCatalog")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| AppError::Message("Nexus modelCatalog must be an object".into()))?;
    let mut existing = match catalog.remove("models") {
        Some(Value::Array(models)) => models,
        Some(_) => {
            return Err(AppError::Message(
                "Nexus modelCatalog.models must be an array".into(),
            ))
        }
        None => Vec::new(),
    };
    existing.retain(|entry| {
        entry
            .get("model")
            .and_then(Value::as_str)
            .is_none_or(|model| !is_known_nexus_model(model))
    });
    let mut models = match app {
        AppType::Codex => vec![
            json!({"model":NEXUS_MODEL,"displayName":"GLM-5.2","contextWindow":NEXUS_CONTEXT,"inputModalities":["text"]}),
        ],
        _ => vec![
            json!({"model":NEXUS_MODEL,"inputModalities":["text"]}),
            json!({"model":"glm-5.2","inputModalities":["text"]}),
        ],
    };
    models.append(&mut existing);
    catalog.insert("models".into(), Value::Array(models));
    Ok(())
}

fn merge_nexus_overrides(meta: &mut Value) -> Result<(), AppError> {
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
        .or_insert_with(|| json!(NEXUS_MAX_TOKENS));
    let template = body
        .entry("chat_template_kwargs")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            AppError::Message("Nexus chat template override must be an object".into())
        })?;
    template.insert("enable_thinking".into(), json!(true));
    template.insert("clear_thinking".into(), json!(false));
    meta.insert("providerType".into(), json!("nexus"));
    meta.insert("apiFormat".into(), json!("openai_chat"));
    meta.insert("managedNexusPresetVersion".into(), json!(NEXUS_VERSION));
    meta.remove("codexChatReasoning");
    Ok(())
}

fn remove_managed_codex_catalog_pointer(settings: &mut Value) -> Result<bool, AppError> {
    let config = settings
        .get("config")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Message("Nexus Codex config is missing".into()))?;
    let mut document = config
        .parse::<DocumentMut>()
        .map_err(|error| AppError::Message(format!("Invalid Nexus Codex config: {error}")))?;
    let owned = crate::codex_config::resolve_nexus_catalog_path(
        config,
        Path::new(CC_SWITCH_CODEX_MODEL_CATALOG_FILENAME),
    )
    .is_some();
    let removed = owned
        && document
            .as_table_mut()
            .remove("model_catalog_json")
            .is_some();
    if removed {
        settings["config"] = json!(document.to_string());
    }
    Ok(removed)
}

fn upgrade_nexus_provider(
    app: &AppType,
    settings: &mut Value,
    meta: &mut Value,
) -> Result<(), AppError> {
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
                .and_then(|item| item.as_str())
                .ok_or_else(|| AppError::Message("Nexus Codex model_provider is missing".into()))?
                .to_string();
            document["model"] = value(NEXUS_MODEL);
            document["model_context_window"] = value(NEXUS_CONTEXT);
            document["model_auto_compact_token_limit"] = value(NEXUS_COMPACT);
            document.as_table_mut().remove("model_reasoning_effort");
            document["model_providers"][&active]["base_url"] = value(NEXUS_ENDPOINT);
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
            env.insert("ANTHROPIC_BASE_URL".into(), json!(NEXUS_ENDPOINT));
            for key in [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_FABLE_MODEL",
                "ANTHROPIC_CUSTOM_MODEL_OPTION",
            ] {
                env.insert(key.into(), json!(NEXUS_CLAUDE_MODEL));
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
            settings
                .pointer_mut("/env/ANTHROPIC_BASE_URL")
                .ok_or_else(|| {
                    AppError::Message("Nexus Claude Desktop base URL is missing".into())
                })?
                .clone_from(&json!(NEXUS_ENDPOINT));
            let routes = meta
                .get_mut("claudeDesktopModelRoutes")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    AppError::Message("Nexus Claude Desktop routes must be an object".into())
                })?;
            for route in routes.values_mut() {
                let route = route.as_object_mut().ok_or_else(|| {
                    AppError::Message("Nexus Claude Desktop route must be an object".into())
                })?;
                route.insert("model".into(), json!(NEXUS_MODEL));
                route.insert("supports1m".into(), json!(true));
            }
            meta["claudeDesktopMode"] = json!("proxy");
        }
        _ => return Ok(()),
    }
    replace_nexus_catalog(settings, app)?;
    merge_nexus_overrides(meta)
}

impl Database {
    pub fn get_all_providers(
        &self,
        app_type: &str,
    ) -> Result<IndexMap<String, Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta, in_failover_queue
             FROM providers WHERE app_type = ?1
             ORDER BY COALESCE(sort_index, 999999), created_at ASC, id ASC"
        ).map_err(|e| AppError::Database(e.to_string()))?;

        let provider_iter = stmt
            .query_map(params![app_type], |row| {
                let id: String = row.get(0)?;
                let name: String = row.get(1)?;
                let settings_config_str: String = row.get(2)?;
                let website_url: Option<String> = row.get(3)?;
                let category: Option<String> = row.get(4)?;
                let created_at: Option<i64> = row.get(5)?;
                let sort_index: Option<usize> = row.get(6)?;
                let notes: Option<String> = row.get(7)?;
                let icon: Option<String> = row.get(8)?;
                let icon_color: Option<String> = row.get(9)?;
                let meta_str: String = row.get(10)?;
                let in_failover_queue: bool = row.get(11)?;

                let settings_config =
                    serde_json::from_str(&settings_config_str).unwrap_or(serde_json::Value::Null);
                let meta: ProviderMeta = serde_json::from_str(&meta_str).unwrap_or_default();

                Ok((
                    id,
                    Provider {
                        id: "".to_string(), // Placeholder, set below
                        name,
                        settings_config,
                        website_url,
                        category,
                        created_at,
                        sort_index,
                        notes,
                        meta: Some(meta),
                        icon,
                        icon_color,
                        in_failover_queue,
                    },
                ))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut providers = IndexMap::new();
        for provider_res in provider_iter {
            let (id, mut provider) = provider_res.map_err(|e| AppError::Database(e.to_string()))?;
            provider.id = id.clone();

            let mut stmt_endpoints = conn.prepare(
                "SELECT url, added_at FROM provider_endpoints WHERE provider_id = ?1 AND app_type = ?2 ORDER BY added_at ASC, url ASC"
            ).map_err(|e| AppError::Database(e.to_string()))?;

            let endpoints_iter = stmt_endpoints
                .query_map(params![id, app_type], |row| {
                    let url: String = row.get(0)?;
                    let added_at: Option<i64> = row.get(1)?;
                    Ok((
                        url,
                        crate::settings::CustomEndpoint {
                            url: "".to_string(),
                            added_at: added_at.unwrap_or(0),
                            last_used: None,
                        },
                    ))
                })
                .map_err(|e| AppError::Database(e.to_string()))?;

            let mut custom_endpoints = HashMap::new();
            for ep_res in endpoints_iter {
                let (url, mut ep) = ep_res.map_err(|e| AppError::Database(e.to_string()))?;
                ep.url = url.clone();
                custom_endpoints.insert(url, ep);
            }

            if let Some(meta) = &mut provider.meta {
                meta.custom_endpoints = custom_endpoints;
            }

            providers.insert(id, provider);
        }

        Ok(providers)
    }

    pub fn get_current_provider(&self, app_type: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT id FROM providers WHERE app_type = ?1 AND is_current = 1 LIMIT 1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query(params![app_type])
            .map_err(|e| AppError::Database(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Ok(Some(
                row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            ))
        } else {
            Ok(None)
        }
    }

    pub fn get_provider_by_id(
        &self,
        id: &str,
        app_type: &str,
    ) -> Result<Option<Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta, in_failover_queue
             FROM providers WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
            |row| {
                let name: String = row.get(0)?;
                let settings_config_str: String = row.get(1)?;
                let website_url: Option<String> = row.get(2)?;
                let category: Option<String> = row.get(3)?;
                let created_at: Option<i64> = row.get(4)?;
                let sort_index: Option<usize> = row.get(5)?;
                let notes: Option<String> = row.get(6)?;
                let icon: Option<String> = row.get(7)?;
                let icon_color: Option<String> = row.get(8)?;
                let meta_str: String = row.get(9)?;
                let in_failover_queue: bool = row.get(10)?;

                let settings_config = serde_json::from_str(&settings_config_str).unwrap_or(serde_json::Value::Null);
                let meta: ProviderMeta = serde_json::from_str(&meta_str).unwrap_or_default();

                Ok(Provider {
                    id: id.to_string(),
                    name,
                    settings_config,
                    website_url,
                    category,
                    created_at,
                    sort_index,
                    notes,
                    meta: Some(meta),
                    icon,
                    icon_color,
                    in_failover_queue,
                })
            },
        );

        match result {
            Ok(provider) => Ok(Some(provider)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    pub fn save_provider(&self, app_type: &str, provider: &Provider) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut meta_clone = provider.meta.clone().unwrap_or_default();
        let endpoints = std::mem::take(&mut meta_clone.custom_endpoints);

        let existing: Option<(bool, bool)> = tx
            .query_row(
                "SELECT is_current, in_failover_queue FROM providers WHERE id = ?1 AND app_type = ?2",
                params![provider.id, app_type],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        let is_update = existing.is_some();
        let (is_current, in_failover_queue) =
            existing.unwrap_or((false, provider.in_failover_queue));

        if is_update {
            tx.execute(
                "UPDATE providers SET
                    name = ?1,
                    settings_config = ?2,
                    website_url = ?3,
                    category = ?4,
                    created_at = ?5,
                    sort_index = ?6,
                    notes = ?7,
                    icon = ?8,
                    icon_color = ?9,
                    meta = ?10,
                    is_current = ?11,
                    in_failover_queue = ?12
                WHERE id = ?13 AND app_type = ?14",
                params![
                    provider.name,
                    serde_json::to_string(&provider.settings_config).map_err(|e| {
                        AppError::Database(format!("Failed to serialize settings_config: {e}"))
                    })?,
                    provider.website_url,
                    provider.category,
                    provider.created_at,
                    provider.sort_index,
                    provider.notes,
                    provider.icon,
                    provider.icon_color,
                    serde_json::to_string(&meta_clone).map_err(|e| AppError::Database(format!(
                        "Failed to serialize meta: {e}"
                    )))?,
                    is_current,
                    in_failover_queue,
                    provider.id,
                    app_type,
                ],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        } else {
            tx.execute(
                "INSERT INTO providers (
                    id, app_type, name, settings_config, website_url, category,
                    created_at, sort_index, notes, icon, icon_color, meta, is_current, in_failover_queue
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    provider.id,
                    app_type,
                    provider.name,
                    serde_json::to_string(&provider.settings_config)
                        .map_err(|e| AppError::Database(format!("Failed to serialize settings_config: {e}")))?,
                    provider.website_url,
                    provider.category,
                    provider.created_at,
                    provider.sort_index,
                    provider.notes,
                    provider.icon,
                    provider.icon_color,
                    serde_json::to_string(&meta_clone)
                        .map_err(|e| AppError::Database(format!("Failed to serialize meta: {e}")))?,
                    is_current,
                    in_failover_queue,
                ],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

            for (url, endpoint) in endpoints {
                tx.execute(
                    "INSERT INTO provider_endpoints (provider_id, app_type, url, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![provider.id, app_type, url, endpoint.added_at],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            }
        }

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn delete_provider(&self, app_type: &str, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM providers WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn set_current_provider(&self, app_type: &str, id: &str) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        tx.execute(
            "UPDATE providers SET is_current = 0 WHERE app_type = ?1",
            params![app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        tx.execute(
            "UPDATE providers SET is_current = 1 WHERE id = ?1 AND app_type = ?2",
            params![id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn update_provider_settings_config(
        &self,
        app_type: &str,
        provider_id: &str,
        settings_config: &serde_json::Value,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE providers SET settings_config = ?1 WHERE id = ?2 AND app_type = ?3",
            params![
                serde_json::to_string(settings_config).map_err(|e| AppError::Database(format!(
                    "Failed to serialize settings_config: {e}"
                )))?,
                provider_id,
                app_type
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn add_custom_endpoint(
        &self,
        app_type: &str,
        provider_id: &str,
        url: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let added_at = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO provider_endpoints (provider_id, app_type, url, added_at) VALUES (?1, ?2, ?3, ?4)",
            params![provider_id, app_type, url, added_at],
        ).map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn remove_custom_endpoint(
        &self,
        app_type: &str,
        provider_id: &str,
        url: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM provider_endpoints WHERE provider_id = ?1 AND app_type = ?2 AND url = ?3",
            params![provider_id, app_type, url],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn set_omo_provider_current(
        &self,
        app_type: &str,
        provider_id: &str,
        category: &str,
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        tx.execute(
            "UPDATE providers SET is_current = 0 WHERE app_type = ?1 AND category = ?2",
            params![app_type, category],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        // OMO ↔ OMO Slim mutually exclusive: deactivate the opposite category
        let opposite = match category {
            "omo" => Some("omo-slim"),
            "omo-slim" => Some("omo"),
            _ => None,
        };
        if let Some(opp) = opposite {
            tx.execute(
                "UPDATE providers SET is_current = 0 WHERE app_type = ?1 AND category = ?2",
                params![app_type, opp],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }
        let updated = tx
            .execute(
                "UPDATE providers SET is_current = 1 WHERE id = ?1 AND app_type = ?2 AND category = ?3",
                params![provider_id, app_type, category],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if updated != 1 {
            return Err(AppError::Database(format!(
                "Failed to set {category} provider current: provider '{provider_id}' not found in app '{app_type}'"
            )));
        }
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn is_omo_provider_current(
        &self,
        app_type: &str,
        provider_id: &str,
        category: &str,
    ) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        match conn.query_row(
            "SELECT is_current FROM providers
             WHERE id = ?1 AND app_type = ?2 AND category = ?3",
            params![provider_id, app_type, category],
            |row| row.get(0),
        ) {
            Ok(is_current) => Ok(is_current),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    pub fn clear_omo_provider_current(
        &self,
        app_type: &str,
        provider_id: &str,
        category: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE providers SET is_current = 0
             WHERE id = ?1 AND app_type = ?2 AND category = ?3",
            params![provider_id, app_type, category],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get_current_omo_provider(
        &self,
        app_type: &str,
        category: &str,
    ) -> Result<Option<Provider>, AppError> {
        let conn = lock_conn!(self.conn);
        let row_data: Result<OmoProviderRow, rusqlite::Error> = conn.query_row(
            "SELECT id, name, settings_config, category, created_at, sort_index, notes, meta
             FROM providers
             WHERE app_type = ?1 AND category = ?2 AND is_current = 1
             LIMIT 1",
            params![app_type, category],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        );

        let (id, name, settings_config_str, _row_category, created_at, sort_index, notes, meta_str) =
            match row_data {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(e) => return Err(AppError::Database(e.to_string())),
            };

        let settings_config = serde_json::from_str(&settings_config_str).map_err(|e| {
            AppError::Database(format!(
                "Failed to parse {category} provider settings_config (provider_id={id}): {e}"
            ))
        })?;
        let meta: crate::provider::ProviderMeta = if meta_str.trim().is_empty() {
            crate::provider::ProviderMeta::default()
        } else {
            serde_json::from_str(&meta_str).map_err(|e| {
                AppError::Database(format!(
                    "Failed to parse {category} provider meta (provider_id={id}): {e}"
                ))
            })?
        };

        Ok(Some(Provider {
            id,
            name,
            settings_config,
            website_url: None,
            category: Some(category.to_string()),
            created_at,
            sort_index,
            notes,
            meta: Some(meta),
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }))
    }

    /// 判断 providers 表是否为空（全 app_type 一起算）。
    ///
    /// 用于区分"全新安装"和"升级用户"：在启动流程 import/seed 之前调用。
    /// 使用 `EXISTS` 短路查询，比 `COUNT(*)` 在将来表变大时更高效。
    pub fn is_providers_empty(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let exists: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM providers)", [], |row| {
                row.get(0)
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(!exists)
    }

    /// 仅获取指定 app 下所有 provider 的 id 集合。
    ///
    /// 比 `get_all_providers` 轻量得多：只读 id 列、无 endpoint 子查询。
    /// 用于只需要做存在性检查的场景（如 additive 模式的 live 同步去重）。
    pub fn get_provider_ids(&self, app_type: &str) -> Result<HashSet<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT id FROM providers WHERE app_type = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map(params![app_type], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut ids = HashSet::new();
        for row in rows {
            ids.insert(row.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(ids)
    }

    /// 判断指定 app 下是否已存在任意 provider。
    ///
    /// 启动阶段的 live import 需要使用这个更严格的判断：
    /// 只要该 app 已经有任何 provider（包括官方 seed），就不应再自动导入 `default`。
    pub fn has_any_provider_for_app(&self, app_type: &str) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM providers WHERE app_type = ?1)",
                params![app_type],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(exists)
    }

    /// 判断指定 app 下是否存在非官方种子的供应商。
    ///
    /// 比 `get_all_providers` 轻量得多：只读 id 列、无 endpoint 子查询、首条命中即返回。
    /// 用于 `import_default_config` 决定是否跳过 live 导入。
    pub fn has_non_official_seed_provider(&self, app_type: &str) -> Result<bool, AppError> {
        use crate::database::dao::providers_seed::is_official_seed_id;
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT id FROM providers WHERE app_type = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let mut rows = stmt
            .query(params![app_type])
            .map_err(|e| AppError::Database(e.to_string()))?;
        while let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            let id: String = row.get(0).map_err(|e| AppError::Database(e.to_string()))?;
            if !is_official_seed_id(&id) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn scrub_shipped_nexus_credentials_for_app(
        &self,
        app: &AppType,
        current_id: Option<&str>,
    ) -> Result<bool, AppError> {
        self.scrub_nexus_credentials_for_app_with_fingerprints(
            app,
            current_id,
            &SHIPPED_KEY_FINGERPRINTS,
        )
    }

    fn scrub_nexus_credentials_for_app_with_fingerprints(
        &self,
        app: &AppType,
        current_id: Option<&str>,
        credential_fingerprints: &[&str],
    ) -> Result<bool, AppError> {
        if !matches!(
            app,
            AppType::Claude | AppType::ClaudeDesktop | AppType::Codex
        ) {
            return Ok(false);
        }
        let app_name = app.as_str();
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction()?;
        let rows = {
            let mut statement = transaction
                .prepare("SELECT id,settings_config FROM providers WHERE app_type=?1")?;
            let rows = statement
                .query_map([app_name], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut sync_current = false;
        for (id, raw_settings) in rows {
            let Ok(mut settings) = serde_json::from_str::<Value>(&raw_settings) else {
                continue;
            };
            if scrub_credentials(app, &mut settings, credential_fingerprints) {
                transaction.execute(
                    "UPDATE providers SET settings_config=?1 WHERE id=?2 AND app_type=?3",
                    params![settings.to_string(), id, app_name],
                )?;
                sync_current |= current_id == Some(id.as_str());
            }
        }
        transaction.commit()?;
        Ok(sync_current)
    }

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
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction()?;
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
            let parsed_meta = serde_json::from_str::<Value>(&raw_meta);
            let explicitly_owned = parsed_meta
                .as_ref()
                .ok()
                .and_then(|meta| meta.get("providerType"))
                .and_then(Value::as_str)
                == Some("nexus");
            let mut settings: Value = match serde_json::from_str(&raw_settings) {
                Ok(settings) => settings,
                Err(error) if explicitly_owned => {
                    return Err(AppError::Message(format!(
                        "Invalid {app_name} Nexus provider '{id}' settings: {error}"
                    )))
                }
                Err(_) => continue,
            };
            let Ok(mut meta) = parsed_meta else {
                continue;
            };
            let original_settings = settings.clone();
            let original_meta = meta.clone();
            let version = meta
                .get("managedNexusPresetVersion")
                .and_then(Value::as_u64);
            let owned = explicitly_owned || is_shipped_legacy_nexus(app, &name, &settings, &meta);
            let supported_version = version.is_none_or(|version| version <= NEXUS_VERSION);
            let valid_signature = has_nexus_signature(app, &settings, &meta);
            if explicitly_owned
                && matches!(app, AppType::ClaudeDesktop)
                && supported_version
                && !valid_signature
            {
                if let Some(meta) = meta.as_object_mut() {
                    meta.remove("providerType");
                    meta.remove("managedNexusPresetVersion");
                }
            } else if owned && supported_version && valid_signature {
                if matches!(app, AppType::Codex) {
                    remove_managed_codex_catalog_pointer(&mut settings).map_err(|error| {
                        AppError::Message(format!(
                            "Cannot clean {app_name} Nexus provider '{id}': {error}"
                        ))
                    })?;
                }
                if version != Some(NEXUS_VERSION) {
                    upgrade_nexus_provider(app, &mut settings, &mut meta).map_err(|error| {
                        AppError::Message(format!(
                            "Cannot upgrade {app_name} Nexus provider '{id}': {error}"
                        ))
                    })?;
                }
            }

            if settings != original_settings || meta != original_meta {
                transaction.execute(
                    "UPDATE providers SET settings_config=?1,meta=?2 WHERE id=?3 AND app_type=?4",
                    params![settings.to_string(), meta.to_string(), id, app_name],
                )?;
                sync_current |= current_id == Some(id.as_str());
            }
        }
        transaction.commit()?;
        Ok(sync_current)
    }

    /// 计算指定 app 下一个可用的 sort_index（追加到末尾）。
    fn next_sort_index_for_app(&self, app_type: &str) -> Result<usize, AppError> {
        let conn = lock_conn!(self.conn);
        let max: Option<i64> = conn
            .query_row(
                "SELECT MAX(sort_index) FROM providers WHERE app_type = ?1",
                params![app_type],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(max.map(|v| (v + 1) as usize).unwrap_or(0))
    }

    /// 启动时调用：补齐缺失的官方预设供应商（Claude / Codex / Gemini）。
    ///
    /// 使用 settings flag `official_providers_seeded` 保证每个数据库只执行一次：
    /// - 全新用户：seed 三条官方预设
    /// - 老用户升级：同样会触发一次（flag 不存在），追加到末尾，不影响已有排序
    /// - 用户删除 seed 后：不再重建（flag 已为 true），尊重用户意图
    ///
    /// 与 `Database::save_provider` 的 UPSERT 语义配合，即使被意外重复调用
    /// 也不会覆盖用户当前激活的供应商（is_current 字段会被保留）。
    pub fn init_default_official_providers(&self) -> Result<usize, AppError> {
        use crate::database::dao::providers_seed::OFFICIAL_SEEDS;

        if self
            .get_bool_flag("official_providers_seeded")
            .unwrap_or(false)
        {
            return Ok(0);
        }

        let mut inserted = 0_usize;
        let now_ms = chrono::Utc::now().timestamp_millis();

        for seed in OFFICIAL_SEEDS {
            let app_type_str = seed.app_type.as_str();

            // 若该 id 已存在（极端情况：用户曾手动用过同 id），跳过
            if self.get_provider_by_id(seed.id, app_type_str)?.is_some() {
                continue;
            }

            let next_sort_index = self.next_sort_index_for_app(app_type_str)?;

            let settings_config: serde_json::Value =
                serde_json::from_str(seed.settings_config_json).map_err(|e| {
                    AppError::Database(format!("Seed JSON parse failed for {}: {e}", seed.id))
                })?;

            let mut provider = Provider::with_id(
                seed.id.to_string(),
                seed.name.to_string(),
                settings_config,
                Some(seed.website_url.to_string()),
            );
            provider.category = Some("official".to_string());
            provider.icon = Some(seed.icon.to_string());
            provider.icon_color = Some(seed.icon_color.to_string());
            provider.sort_index = Some(next_sort_index);
            provider.created_at = Some(now_ms);

            self.save_provider(app_type_str, &provider)?;
            inserted += 1;
            log::info!(
                "✓ Seeded official provider: {} ({})",
                seed.name,
                app_type_str
            );
        }

        // 即使 inserted=0（例如用户手动创建过同 id）也设置 flag 防止反复检查
        self.set_setting("official_providers_seeded", "true")?;

        Ok(inserted)
    }

    /// 按 id 兜底插入单条 official seed（仅当目标表中该 id 不存在时插入）。
    ///
    /// 与 `init_default_official_providers` 不同：
    /// - 不触碰 `official_providers_seeded` 全局 flag，是 on-demand 修复
    /// - 只处理一条 seed，由调用方决定 id + app_type
    /// - 已存在则尊重用户自定义，不覆盖
    ///
    /// 返回 Ok(true) 表示插入了新行，Ok(false) 表示已存在被跳过。
    pub fn ensure_official_seed_by_id(
        &self,
        seed_id: &str,
        app_type: crate::app_config::AppType,
    ) -> Result<bool, AppError> {
        use crate::database::dao::providers_seed::OFFICIAL_SEEDS;

        let seed = OFFICIAL_SEEDS
            .iter()
            .find(|s| s.id == seed_id && s.app_type == app_type)
            .ok_or_else(|| {
                AppError::Database(format!(
                    "unknown official seed: id={seed_id}, app_type={}",
                    app_type.as_str()
                ))
            })?;

        let app_type_str = seed.app_type.as_str();

        if self.get_provider_by_id(seed_id, app_type_str)?.is_some() {
            return Ok(false);
        }

        let settings_config: serde_json::Value = serde_json::from_str(seed.settings_config_json)
            .map_err(|e| {
                AppError::Database(format!("Seed JSON parse failed for {}: {e}", seed.id))
            })?;

        let next_sort_index = self.next_sort_index_for_app(app_type_str)?;
        let now_ms = chrono::Utc::now().timestamp_millis();

        let mut provider = Provider::with_id(
            seed.id.to_string(),
            seed.name.to_string(),
            settings_config,
            Some(seed.website_url.to_string()),
        );
        provider.category = Some("official".to_string());
        provider.icon = Some(seed.icon.to_string());
        provider.icon_color = Some(seed.icon_color.to_string());
        provider.sort_index = Some(next_sort_index);
        provider.created_at = Some(now_ms);

        self.save_provider(app_type_str, &provider)?;

        Ok(true)
    }
}

#[cfg(test)]
mod ensure_official_seed_tests {
    use super::{
        has_nexus_signature, is_shipped_legacy_nexus, remove_managed_codex_catalog_pointer,
        LEGACY_NEXUS_ENDPOINT, LEGACY_NEXUS_MODEL, NEXUS_ENDPOINT, NEXUS_MODEL, NEXUS_VERSION,
    };
    use crate::app_config::AppType;
    use crate::database::{Database, CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID};
    use crate::provider::Provider;
    use serde_json::json;
    use sha2::Digest;

    fn save_nexus(
        db: &Database,
        app: &str,
        id: &str,
        settings: serde_json::Value,
        meta: serde_json::Value,
    ) {
        let mut provider = Provider::with_id(id.into(), "Nexus GLM-5.2".into(), settings, None);
        provider.meta = Some(serde_json::from_value(meta).unwrap());
        db.save_provider(app, &provider).unwrap();
    }

    fn nexus(db: &Database, app: &str, id: &str) -> Provider {
        db.get_provider_by_id(id, app).unwrap().unwrap()
    }

    #[test]
    fn ensure_inserts_when_missing() {
        let db = Database::memory().expect("memory db");
        let inserted = db
            .ensure_official_seed_by_id(CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID, AppType::ClaudeDesktop)
            .expect("ensure ok");
        assert!(inserted, "should insert when missing");

        let provider = db
            .get_provider_by_id(
                CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID,
                AppType::ClaudeDesktop.as_str(),
            )
            .expect("query ok")
            .expect("provider exists after ensure");

        assert_eq!(provider.id, CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID);
        assert_eq!(provider.name, "Claude Desktop Official");
        assert_eq!(provider.category.as_deref(), Some("official"));
        assert_eq!(provider.icon.as_deref(), Some("anthropic"));
        assert_eq!(provider.icon_color.as_deref(), Some("#D4915D"));
    }

    #[test]
    fn ensure_skips_when_present_and_preserves_customization() {
        let db = Database::memory().expect("memory db");
        db.init_default_official_providers().expect("seed");

        let mut renamed = db
            .get_provider_by_id(
                CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID,
                AppType::ClaudeDesktop.as_str(),
            )
            .expect("query ok")
            .expect("seed present");
        renamed.name = "My Custom Backup".to_string();
        db.save_provider(AppType::ClaudeDesktop.as_str(), &renamed)
            .expect("save customization");

        let inserted = db
            .ensure_official_seed_by_id(CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID, AppType::ClaudeDesktop)
            .expect("ensure ok");
        assert!(!inserted, "should skip when present");

        let after = db
            .get_provider_by_id(
                CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID,
                AppType::ClaudeDesktop.as_str(),
            )
            .expect("query ok")
            .expect("still present");
        assert_eq!(
            after.name, "My Custom Backup",
            "customization must not be overwritten"
        );
    }

    #[test]
    fn ensure_rejects_unknown_seed() {
        let db = Database::memory().expect("memory db");
        let result = db.ensure_official_seed_by_id("nonexistent-id", AppType::ClaudeDesktop);
        assert!(result.is_err(), "unknown seed id should be Err");
    }

    #[test]
    fn ensure_rejects_seed_app_type_mismatch() {
        let db = Database::memory().expect("memory db");
        let result =
            db.ensure_official_seed_by_id(CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID, AppType::Claude);
        assert!(result.is_err(), "(id, app_type) mismatch should be Err");
    }

    #[test]
    fn managed_nexus_v5_upgrades_each_app_without_losing_user_fields() {
        let db = Database::memory().unwrap();
        let endpoint = "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
        let shared_meta = json!({
            "providerType":"nexus", "managedNexusPresetVersion":5,
            "apiFormat":"openai_chat", "customUserAgent":"keep-agent",
            "localProxyRequestOverrides":{"headers":{"x-keep":"yes"},"body":{"temperature":0.2}}
        });
        let codex_settings = json!({
            "auth":{"OPENAI_API_KEY":"rotated-user-key"},
            "config":format!("model_provider='custom'\nmodel='GLM-5.2-FP8'\nmodel_catalog_json='cc-switch-model-catalog.json'\nmodel_reasoning_effort='high'\n[model_providers.custom]\nbase_url='{endpoint}'\nwire_api='responses'"),
            "modelCatalog":{"models":[
                {"model":"glm-5.2","inputModalities":["text","image"]},
                {"model":"user-model","inputModalities":["text"]}
            ]}
        });
        save_nexus(&db, "codex", "codex", codex_settings, shared_meta.clone());
        let claude_settings = json!({"env":{
                "ANTHROPIC_BASE_URL":endpoint, "ANTHROPIC_AUTH_TOKEN":"rotated-user-key",
                "ANTHROPIC_MODEL":"GLM-5.2-FP8[1m]"
        }});
        save_nexus(
            &db,
            "claude",
            "claude",
            claude_settings,
            shared_meta.clone(),
        );
        let mut desktop_meta = shared_meta;
        desktop_meta["claudeDesktopMode"] = json!("proxy");
        desktop_meta["claudeDesktopModelRoutes"] = json!({
            "claude-sonnet-5":{"model":"GLM-5.2-FP8","labelOverride":"keep"}
        });
        let desktop_settings = json!({"env":{"ANTHROPIC_BASE_URL":endpoint}});
        save_nexus(
            &db,
            "claude-desktop",
            "desktop",
            desktop_settings,
            desktop_meta,
        );

        for (app, id) in [
            (AppType::Codex, "codex"),
            (AppType::Claude, "claude"),
            (AppType::ClaudeDesktop, "desktop"),
        ] {
            assert!(db.migrate_managed_nexus_for_app(&app, Some(id)).unwrap());
        }
        let codex = nexus(&db, "codex", "codex");
        let codex_meta = codex.meta.unwrap();
        assert_eq!(codex_meta.managed_nexus_preset_version, Some(6));
        assert_eq!(codex_meta.custom_user_agent.as_deref(), Some("keep-agent"));
        let overrides = codex_meta.local_proxy_request_overrides.unwrap();
        assert_eq!(overrides.headers["x-keep"], "yes");
        assert_eq!(overrides.body.unwrap()["temperature"], 0.2);
        assert_eq!(
            codex.settings_config["auth"]["OPENAI_API_KEY"],
            "rotated-user-key"
        );
        let models = codex.settings_config["modelCatalog"]["models"]
            .as_array()
            .unwrap();
        assert_eq!(
            models
                .iter()
                .filter_map(|model| model["model"].as_str())
                .collect::<Vec<_>>(),
            [NEXUS_MODEL, "user-model"]
        );
        assert_eq!(models[0]["inputModalities"], json!(["text"]));
        let config = codex.settings_config["config"].as_str().unwrap();
        assert!(config.contains("model_auto_compact_token_limit = 252000"));
        assert!(!config.contains("model_catalog_json"));
        assert!(!config.contains("model_reasoning_effort"));
        let claude = nexus(&db, "claude", "claude");
        assert_eq!(claude.meta.unwrap().managed_nexus_preset_version, Some(6));
        assert_eq!(claude.settings_config["env"]["API_TIMEOUT_MS"], "3000000");
        let desktop = nexus(&db, "claude-desktop", "desktop");
        let route = &desktop.meta.unwrap().claude_desktop_model_routes["claude-sonnet-5"];
        assert_eq!(
            (route.model.as_str(), route.supports_1m),
            (NEXUS_MODEL, Some(true))
        );
        assert!(!db
            .migrate_managed_nexus_for_app(&AppType::Codex, Some("codex"))
            .unwrap());
    }

    #[test]
    fn migration_removes_only_owned_codex_catalog_pointer() {
        let config = |path| {
            json!({"config":format!(
                "model_catalog_json='{path}'\nmodel_provider='p'\n[model_providers.p]\nbase_url='{NEXUS_ENDPOINT}'"
            )})
        };
        let mut user = config("/Users/me/.codex/glm-catalog.json");
        assert!(!remove_managed_codex_catalog_pointer(&mut user).unwrap());
        assert!(user["config"].as_str().unwrap().contains("glm-catalog"));

        let mut owned = config("cc-switch-model-catalog.json");
        assert!(remove_managed_codex_catalog_pointer(&mut owned).unwrap());
        assert!(!owned["config"]
            .as_str()
            .unwrap()
            .contains("model_catalog_json"));
        assert!(!remove_managed_codex_catalog_pointer(&mut owned).unwrap());
    }

    #[test]
    fn migration_scrubs_exact_leak_from_unowned_current_rows() {
        let synthetic = "synthetic-shipped-key";
        let fingerprint = format!("{:x}", sha2::Sha256::digest(synthetic.as_bytes()));
        for (app, settings, pointer) in [
            (
                AppType::Codex,
                json!({
                    "auth":{"OPENAI_API_KEY":synthetic},
                    "config":"model_provider='custom'\nmodel='other'\n[model_providers.custom]\nbase_url='https://custom.example/v1'"
                }),
                "/auth/OPENAI_API_KEY",
            ),
            (
                AppType::Claude,
                json!({"env":{
                    "ANTHROPIC_BASE_URL":"https://custom.example/v1",
                    "ANTHROPIC_AUTH_TOKEN":synthetic
                }}),
                "/env/ANTHROPIC_AUTH_TOKEN",
            ),
            (
                AppType::ClaudeDesktop,
                json!({"env":{
                    "ANTHROPIC_BASE_URL":"https://custom.example/v1",
                    "ANTHROPIC_API_KEY":synthetic,
                    "ANTHROPIC_AUTH_TOKEN":"rotated-user-key"
                }}),
                "/env/ANTHROPIC_API_KEY",
            ),
        ] {
            let db = Database::memory().unwrap();
            let mut provider = Provider::with_id(
                "custom".into(),
                "Unrelated custom provider".into(),
                settings,
                None,
            );
            provider.meta = Some(Default::default());
            db.save_provider(app.as_str(), &provider).unwrap();

            assert!(db
                .scrub_nexus_credentials_for_app_with_fingerprints(
                    &app,
                    Some("custom"),
                    &[fingerprint.as_str()],
                )
                .unwrap());
            assert_eq!(
                nexus(&db, app.as_str(), "custom")
                    .settings_config
                    .pointer(pointer),
                Some(&json!(""))
            );
            if matches!(app, AppType::ClaudeDesktop) {
                assert_eq!(
                    nexus(&db, app.as_str(), "custom")
                        .settings_config
                        .pointer("/env/ANTHROPIC_AUTH_TOKEN"),
                    Some(&json!("rotated-user-key"))
                );
            }
            assert!(!db
                .scrub_nexus_credentials_for_app_with_fingerprints(
                    &app,
                    Some("custom"),
                    &[fingerprint.as_str()],
                )
                .unwrap());
        }
    }

    #[test]
    fn credential_scrub_survives_an_unrelated_managed_migration_error() {
        let db = Database::memory().unwrap();
        let leaked = "synthetic-shipped-key";
        let fingerprint = format!("{:x}", sha2::Sha256::digest(leaked.as_bytes()));
        save_nexus(
            &db,
            "claude",
            "leaked",
            json!({"env":{"ANTHROPIC_AUTH_TOKEN":leaked}}),
            json!({}),
        );
        save_nexus(
            &db,
            "claude",
            "broken",
            json!({
                "env":{"ANTHROPIC_BASE_URL":NEXUS_ENDPOINT,"ANTHROPIC_MODEL":NEXUS_MODEL},
                "modelCatalog":"invalid"
            }),
            json!({
                "providerType":"nexus",
                "managedNexusPresetVersion":NEXUS_VERSION - 1,
                "apiFormat":"openai_chat"
            }),
        );

        assert!(db
            .scrub_nexus_credentials_for_app_with_fingerprints(
                &AppType::Claude,
                Some("leaked"),
                &[fingerprint.as_str()],
            )
            .unwrap());
        assert!(db
            .migrate_managed_nexus_for_app(&AppType::Claude, Some("leaked"))
            .is_err());
        assert_eq!(
            nexus(&db, "claude", "leaked")
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN"),
            Some(&json!(""))
        );
    }

    #[test]
    fn implicit_local_debug_provider_is_not_claimed() {
        let local = json!({"env":{
            "ANTHROPIC_BASE_URL":"http://127.0.0.1:30001/v1",
            "ANTHROPIC_MODEL":"glm-5.2[1m]"
        }});
        let meta = json!({"apiFormat":"openai_chat"});
        assert!(!is_shipped_legacy_nexus(
            &AppType::Claude,
            "Nexus Local",
            &local,
            &meta
        ));
        assert!(has_nexus_signature(&AppType::Claude, &local, &meta));
        let shipped = json!({"env":{
            "ANTHROPIC_BASE_URL":LEGACY_NEXUS_ENDPOINT,
            "ANTHROPIC_MODEL":LEGACY_NEXUS_MODEL,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL":LEGACY_NEXUS_MODEL,
            "ANTHROPIC_DEFAULT_SONNET_MODEL":LEGACY_NEXUS_MODEL,
            "ANTHROPIC_DEFAULT_OPUS_MODEL":LEGACY_NEXUS_MODEL
        }});
        assert!(is_shipped_legacy_nexus(
            &AppType::Claude,
            "Nexus GLM-5.2",
            &shipped,
            &meta
        ));

        let db = Database::memory().unwrap();
        save_nexus(&db, "claude", "legacy", shipped, meta);
        assert!(db
            .migrate_managed_nexus_for_app(&AppType::Claude, Some("legacy"))
            .unwrap());
        let legacy = nexus(&db, "claude", "legacy");
        assert_eq!(
            legacy.meta.unwrap().managed_nexus_preset_version,
            Some(NEXUS_VERSION as u32)
        );
        assert_eq!(
            legacy.settings_config["env"]["ANTHROPIC_BASE_URL"],
            NEXUS_ENDPOINT
        );
    }

    #[test]
    fn migration_detaches_customized_managed_claude_desktop() {
        let db = Database::memory().unwrap();
        save_nexus(
            &db,
            "claude-desktop",
            "customized",
            json!({"env":{"ANTHROPIC_BASE_URL":"https://custom.example/v1"}}),
            json!({
                "providerType":"nexus",
                "managedNexusPresetVersion":NEXUS_VERSION - 1,
                "apiFormat":"openai_chat",
                "claudeDesktopMode":"proxy",
                "claudeDesktopModelRoutes":{"claude-sonnet-5":{"model":"custom-model"}}
            }),
        );

        assert!(db
            .migrate_managed_nexus_for_app(&AppType::ClaudeDesktop, Some("customized"))
            .unwrap());
        let meta = nexus(&db, "claude-desktop", "customized").meta.unwrap();
        assert!(meta.provider_type.is_none());
        assert!(meta.managed_nexus_preset_version.is_none());
        assert!(!db
            .migrate_managed_nexus_for_app(&AppType::ClaudeDesktop, Some("customized"))
            .unwrap());
    }
}
