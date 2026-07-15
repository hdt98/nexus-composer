use crate::app_config::AppType;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::provider::Provider;
use rusqlite::params;
use serde_json::{json, Map, Value};
use toml_edit::{value, DocumentMut};

pub(crate) const ENDPOINT: &str = "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
pub(crate) const MODEL: &str = "GLM-5.2-FP8";
pub(crate) const CLAUDE_MODEL: &str = "GLM-5.2-FP8[1m]";
pub(crate) const VERSION: u32 = 3;
const NAME: &str = "Nexus GLM-5.2";
const CONTEXT: i64 = 1_048_576;
const COMPACT: i64 = 252_000;
const MAX_TOKENS: u64 = 65_536;
const STREAM_IDLE_MS: i64 = 900_000;
const LOCAL_ENDPOINT: &str = "http://127.0.0.1:30000/v1";
const DRAFT_ENDPOINT: &str = "http://127.0.0.1:30001/v1";
const HOSTED_ENDPOINT: &str = "https://glm-test-glm52-tp4.onenexus-do.cloud/v1";
const LOCAL_MODEL: &str = "GLM-5.2-SGLang";
const LEGACY_MODEL: &str = "glm-5.2";
const LEGACY_CLAUDE_MODEL: &str = "glm-5.2[1m]";
const LEGACY_NAMES: [&str; 4] = ["Nexus", "Nexus Local", "Nexus GLM-5.2 Hosted", NAME];
const MANAGED_MODELS: [&str; 5] = [
    LOCAL_MODEL,
    LEGACY_MODEL,
    LEGACY_CLAUDE_MODEL,
    MODEL,
    CLAUDE_MODEL,
];
const SHIPPED_KEYS: [&str; 4] = [
    "nexus-local",
    "dummy",
    "onenx_4f0133292760f767_4NJH8994xJRPhxRGJzIahdDRtnRLkds8UD6FLrwA6ZQ",
    "onenx_77c730bc912a8f08_e6pVlx7XLCcIugi-JwxWP7gPbzCugk6vxmbU-YEXpWc",
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
    pub(crate) current_codex_changed: bool,
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
        let managed = entry.get("role").is_none()
            && entry
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| MANAGED_MODELS.contains(&model));
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
) {
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
            entry.get("role").is_none()
                && entry
                    .get("model")
                    .and_then(Value::as_str)
                    .is_some_and(|model| MANAGED_MODELS.contains(&model))
                && inherited.contains(entry)
        });
    }
}

fn scrub(object: &mut Map<String, Value>, key: &str) {
    if object
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| SHIPPED_KEYS.contains(&value))
    {
        object.remove(key);
    }
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
                    scrub(env, "ANTHROPIC_AUTH_TOKEN");
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
                    scrub(env, "ANTHROPIC_AUTH_TOKEN");
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
                    scrub(auth, "OPENAI_API_KEY");
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
        current_codex_id: Option<&str>,
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
            let (Ok(mut settings), Ok(mut meta)) = (
                serde_json::from_str::<Value>(&settings),
                serde_json::from_str::<Value>(&meta),
            ) else {
                continue;
            };
            match classify(&app, &name, &settings, &meta) {
                Ownership::Current | Ownership::Unrelated => continue,
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
            outcome.current_codex_changed |=
                app == "codex" && current_codex_id == Some(id.as_str());
        }
        tx.commit()?;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let future = json!({"providerType":"nexus","managedNexusPresetVersion":4});
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
    fn migration_preserves_request_values_and_detaches_custom_current_codex() {
        let db = Database::memory().unwrap();
        save(
            &db,
            "claude",
            "claude",
            "Nexus",
            json!({"env": {
                "ANTHROPIC_BASE_URL": HOSTED_ENDPOINT,
                "ANTHROPIC_AUTH_TOKEN": SHIPPED_KEYS[2],
                "ANTHROPIC_MODEL": LEGACY_MODEL,
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": LEGACY_MODEL,
                "ANTHROPIC_DEFAULT_SONNET_MODEL": LEGACY_MODEL,
                "ANTHROPIC_DEFAULT_OPUS_MODEL": LEGACY_MODEL
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

        let outcome = db.migrate_legacy_nexus_providers(Some("current")).unwrap();
        assert_eq!(
            outcome,
            NexusMigrationOutcome {
                migrated: 2,
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
        let codex = db.get_provider_by_id("current", "codex").unwrap().unwrap();
        let meta = codex.meta.unwrap();
        assert!(meta.provider_type.is_none() && meta.codex_chat_reasoning.is_none());
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
        assert_eq!(db.migrate_legacy_nexus_providers(None).unwrap().migrated, 1);
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
}
