use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::app_config::AppType;
use crate::error::AppError;
use crate::store::AppState;

use super::remote::{resolve_ssh_target, run_ssh_command, shell_quote_single, SshConnectionTarget};

const AGENT_SCRIPT: &str = include_str!("../../resources/nexus_remote_agent.js");
const HISTORY_MAINTAINER: &str = include_str!("../../resources/nexus_remote_history_maintainer.py");
const AGENT_DIR: &str = "$HOME/.nexus-composer/remote-agent";
const AGENT_SCRIPT_PATH: &str = "$HOME/.nexus-composer/remote-agent/nexus-remote-agent.js";
const HISTORY_MAINTAINER_PATH: &str =
    "$HOME/.nexus-composer/remote-agent/nexus-remote-history-maintainer.py";
const AGENT_CONFIG_PATH: &str = "$HOME/.nexus-composer/remote-agent/agent-config.json";
const AGENT_LOG_PATH: &str = "$HOME/.nexus-composer/remote-agent/agent.log";
const AGENT_PID_PATH: &str = "$HOME/.nexus-composer/remote-agent/agent.pid";
const DEFAULT_PROXY_PORT: u16 = 18080;
const DEFAULT_CONTROL_PORT: u16 = 18090;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentStatus {
    pub host_alias: String,
    pub installed: bool,
    pub running: bool,
    pub node_version: Option<String>,
    pub pid: Option<u32>,
    pub status: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentDeployResult {
    pub host_alias: String,
    pub installed: bool,
    pub running: bool,
    pub written_files: Vec<String>,
    pub status: Option<Value>,
    pub history_preparation: Value,
    pub codex_reload: RemoteCodexReloadResult,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentSwitchResult {
    pub host_alias: String,
    pub provider_id: String,
    pub response: Value,
    pub history_preparation: Value,
    pub codex_reload: RemoteCodexReloadResult,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCodexReloadResult {
    pub attempted: bool,
    pub stopped_processes: usize,
    pub reconnect_triggered: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAgentProviderConfig {
    upstream_url: String,
    api_key: String,
    model: String,
    display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_catalog: Option<Value>,
    proxy_port: u16,
    control_port: u16,
    reasoning_effort: String,
}

pub struct RemoteAgentService;

impl RemoteAgentService {
    pub fn status(target: &SshConnectionTarget) -> Result<RemoteAgentStatus, AppError> {
        let target = resolve_ssh_target(target)?;
        let host_alias = target.label().to_string();
        let installed = remote_file_exists(&target, AGENT_SCRIPT_PATH)?;
        let node_version = remote_node_version(&target).ok();
        let status_result = remote_control_get(&target, "/status");
        let (running, status, error, pid) = match status_result {
            Ok(value) => {
                let pid = value
                    .get("agent")
                    .and_then(|agent| agent.get("pid"))
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok());
                (true, Some(value), None, pid)
            }
            Err(err) => {
                let pid = remote_agent_pid(&target).ok().flatten();
                (false, None, Some(err.to_string()), pid)
            }
        };

        Ok(RemoteAgentStatus {
            host_alias,
            installed,
            running,
            node_version,
            pid,
            status,
            error,
        })
    }

    pub fn deploy(
        state: &AppState,
        target: &SshConnectionTarget,
        provider_id: &str,
    ) -> Result<RemoteAgentDeployResult, AppError> {
        let target = resolve_ssh_target(target)?;
        let host_alias = target.label().to_string();
        let provider_config = provider_config_from_codex_provider(state, provider_id)?;
        ensure_remote_node_18(&target)?;

        stop_existing_remote_agent_for_redeploy(&target)?;
        run_ssh_command(
            &target,
            &format!("set -e; mkdir -p {AGENT_DIR}; chmod 700 {AGENT_DIR} 2>/dev/null || true"),
            None,
        )?;
        write_remote_agent_file(&target, AGENT_SCRIPT_PATH, AGENT_SCRIPT.as_bytes(), true)?;
        write_remote_agent_file(
            &target,
            HISTORY_MAINTAINER_PATH,
            HISTORY_MAINTAINER.as_bytes(),
            true,
        )?;
        write_remote_agent_file(
            &target,
            AGENT_CONFIG_PATH,
            serde_json::to_string_pretty(&provider_config)
                .map_err(|e| AppError::JsonSerialize { source: e })?
                .as_bytes(),
            false,
        )?;
        start_remote_agent(&target)?;
        let status = wait_remote_agent_status(&target);
        if status.is_none() {
            let _ = restore_remote_agent_without_control_api(&target);
            return Err(AppError::Message(
                "Remote Agent did not become healthy; the original Codex profile was restored"
                    .to_string(),
            ));
        }
        let history_preparation =
            match prepare_remote_history_for_agent(&target, &provider_config.model) {
                Ok(result) => result,
                Err(error) => {
                    let _ = remote_control_post(&target, "/stop", &json!({}));
                    return Err(error);
                }
            };
        let codex_reload = reload_remote_codex_app_server(&target);

        Ok(RemoteAgentDeployResult {
            host_alias,
            installed: true,
            running: status.is_some(),
            written_files: vec![
                AGENT_SCRIPT_PATH.to_string(),
                HISTORY_MAINTAINER_PATH.to_string(),
                AGENT_CONFIG_PATH.to_string(),
                AGENT_LOG_PATH.to_string(),
            ],
            status,
            history_preparation,
            codex_reload,
        })
    }

    pub fn switch_provider(
        state: &AppState,
        target: &SshConnectionTarget,
        provider_id: &str,
    ) -> Result<RemoteAgentSwitchResult, AppError> {
        let target = resolve_ssh_target(target)?;
        let host_alias = target.label().to_string();
        let provider_config = provider_config_from_codex_provider(state, provider_id)?;
        write_remote_agent_file(
            &target,
            HISTORY_MAINTAINER_PATH,
            HISTORY_MAINTAINER.as_bytes(),
            true,
        )?;
        let history_preparation =
            prepare_remote_history_for_agent(&target, &provider_config.model)?;
        let response = remote_control_post(&target, "/switch", &provider_config)?;
        let codex_reload = reload_remote_codex_app_server(&target);

        Ok(RemoteAgentSwitchResult {
            host_alias,
            provider_id: provider_id.to_string(),
            response,
            history_preparation,
            codex_reload,
        })
    }

    pub fn stop(target: &SshConnectionTarget) -> Result<Value, AppError> {
        let target = resolve_ssh_target(target)?;
        // Refresh this helper even for older already-running agents. Versions
        // before 0.1.7 restore config/auth but do not move their thread bucket,
        // so Nexus Composer performs the same idempotent maintenance first.
        write_remote_agent_file(
            &target,
            HISTORY_MAINTAINER_PATH,
            HISTORY_MAINTAINER.as_bytes(),
            true,
        )?;

        let agent_maintains_history = remote_control_get(&target, "/status")
            .ok()
            .as_ref()
            .and_then(|value| value.get("agent"))
            .and_then(|agent| agent.get("version"))
            .and_then(Value::as_str)
            .is_some_and(agent_version_maintains_history);
        if agent_maintains_history {
            let restored = remote_control_post(&target, "/stop", &json!({}))?;
            let codex_reload = reload_remote_codex_app_server(&target);
            return Ok(attach_codex_reload(restored, codex_reload));
        }

        let maintenance = run_remote_history_maintenance(&target)?;
        let restored = match remote_control_post(&target, "/stop", &json!({})) {
            Ok(value) => value,
            Err(_) => restore_remote_agent_without_control_api(&target)?,
        };
        let restored = attach_history_maintenance(restored, maintenance);
        let codex_reload = reload_remote_codex_app_server(&target);
        Ok(attach_codex_reload(restored, codex_reload))
    }

    pub fn logs(target: &SshConnectionTarget, lines: Option<u16>) -> Result<String, AppError> {
        let target = resolve_ssh_target(target)?;
        let lines = lines.unwrap_or(120).clamp(1, 500);
        run_ssh_command(
            &target,
            &format!("if [ -f {AGENT_LOG_PATH} ]; then tail -n {lines} {AGENT_LOG_PATH}; fi"),
            None,
        )
    }
}

fn agent_version_maintains_history(version: &str) -> bool {
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let parsed = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    parsed >= (0, 1, 7)
}

fn run_remote_history_maintenance(
    target: &super::remote::ResolvedSshTarget,
) -> Result<Value, AppError> {
    // The standard sqlite3 Python module provides online backup + transactional
    // updates without shipping an architecture-specific native dependency.
    // If Python is absent, stop is blocked only when affected history exists.
    let command = format!(
        "set -e; \
         python_bin=\"$(command -v python3 || command -v python || true)\"; \
         if [ -n \"$python_bin\" ]; then \
           exec \"$python_bin\" {HISTORY_MAINTAINER_PATH} --source-provider nexus-agent; \
         fi; \
         if grep -R -l -m 1 '\"model_provider\"[[:space:]]*:[[:space:]]*\"nexus-agent\"' \
              \"$HOME/.codex/sessions\" \"$HOME/.codex/archived_sessions\" 2>/dev/null | grep -q . \
            || grep -a -l -m 1 'nexus-agent' \"$HOME\"/.codex/state_*.sqlite 2>/dev/null | grep -q .; then \
           printf '%s\\n' 'Remote Agent found existing Codex threads but Python with sqlite3 is unavailable. Install Python 3, then stop again; the agent was left running and no profile was changed.' >&2; \
           exit 69; \
         fi; \
         printf '%s\\n' '{{\"ok\":true,\"skipped\":\"no_matching_threads_without_python\",\"jsonlFiles\":0,\"stateRows\":0}}'"
    );
    let output = run_ssh_command(target, &command, None)?;
    serde_json::from_str(output.trim()).map_err(|error| {
        AppError::Message(format!(
            "Failed to parse remote history maintenance result: {error}"
        ))
    })
}

fn prepare_remote_history_for_agent(
    target: &super::remote::ResolvedSshTarget,
    target_model: &str,
) -> Result<Value, AppError> {
    let command = format!(
        "set -e; \
         python_bin=\"$(command -v python3 || command -v python || true)\"; \
         if [ -z \"$python_bin\" ]; then \
           printf '%s\\n' 'Preparing existing Codex history for Remote Agent requires Python 3 with sqlite3.' >&2; \
           exit 127; \
         fi; \
         exec \"$python_bin\" {HISTORY_MAINTAINER_PATH} \
           --source-provider openai \
           --target-provider nexus-agent \
           --target-model {}",
        shell_quote_single(target_model)
    );
    let output = run_ssh_command(target, &command, None)?;
    let value: Value = serde_json::from_str(output.trim()).map_err(|error| {
        AppError::Message(format!(
            "Failed to parse remote history preparation result: {error}"
        ))
    })?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(AppError::Message(format!(
            "Remote history preparation failed: {}",
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        )));
    }
    Ok(value)
}

fn attach_history_maintenance(mut response: Value, maintenance: Value) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.insert("history_maintenance".to_string(), maintenance);
        response
    } else {
        json!({
            "ok": true,
            "restore": response,
            "history_maintenance": maintenance,
        })
    }
}

fn attach_codex_reload(mut response: Value, reload: RemoteCodexReloadResult) -> Value {
    let reload = serde_json::to_value(reload).unwrap_or_else(|_| {
        json!({
            "attempted": true,
            "stoppedProcesses": 0,
            "reconnectTriggered": false,
            "error": "Failed to serialize Codex reload result",
        })
    });
    if let Some(object) = response.as_object_mut() {
        object.insert("codex_reload".to_string(), reload);
        response
    } else {
        json!({
            "ok": true,
            "restore": response,
            "codex_reload": reload,
        })
    }
}

fn provider_config_from_codex_provider(
    state: &AppState,
    provider_id: &str,
) -> Result<RemoteAgentProviderConfig, AppError> {
    let provider = state
        .db
        .get_provider_by_id(provider_id, AppType::Codex.as_str())?
        .ok_or_else(|| AppError::Message(format!("Codex provider not found: {provider_id}")))?;
    if provider.category.as_deref() == Some("official") {
        return Err(AppError::Message(
            "OpenAI Official does not need the remote agent".to_string(),
        ));
    }

    let settings = provider.settings_config.as_object().ok_or_else(|| {
        AppError::Config("Codex provider config must be a JSON object".to_string())
    })?;
    let auth = settings.get("auth");
    let config_text = settings
        .get("config")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Config("Codex provider config missing config.toml".to_string()))?;
    let upstream_url = crate::codex_config::extract_codex_base_url(config_text)
        .ok_or_else(|| AppError::Message("Codex provider is missing base_url".to_string()))?;
    let api_key = crate::codex_config::extract_codex_api_key(auth, Some(config_text))
        .ok_or_else(|| AppError::Message("Codex provider is missing API key".to_string()))?;
    let model = crate::codex_config::codex_top_level_model(config_text)
        .unwrap_or_else(|| crate::codex_config::default_codex_official_model().to_string());
    let reasoning_effort = extract_top_level_toml_string(config_text, "model_reasoning_effort")
        .unwrap_or_else(|| "high".to_string());
    let model_catalog = crate::codex_config::codex_model_catalog_from_settings(
        &provider.settings_config,
        config_text,
        crate::codex_config::CodexCatalogToolProfile::ProxyChat,
    )?;

    Ok(RemoteAgentProviderConfig {
        upstream_url: normalize_chat_base_url(&upstream_url),
        api_key,
        model,
        display_name: provider.name,
        model_catalog,
        proxy_port: DEFAULT_PROXY_PORT,
        control_port: DEFAULT_CONTROL_PORT,
        reasoning_effort,
    })
}

fn normalize_chat_base_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/chat/completions")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

fn extract_top_level_toml_string(config_text: &str, key: &str) -> Option<String> {
    let parsed = config_text.parse::<toml::Value>().ok()?;
    parsed
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn ensure_remote_node_18(target: &super::remote::ResolvedSshTarget) -> Result<(), AppError> {
    let version = remote_node_version(target)?;
    let major = version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if major < 18 {
        return Err(AppError::Message(format!(
            "Remote agent requires Node.js 18+, found {version}"
        )));
    }
    Ok(())
}

fn remote_node_version(target: &super::remote::ResolvedSshTarget) -> Result<String, AppError> {
    let command = "PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/opt/homebrew/bin:$HOME/.local/bin:$HOME/bin\"; node_bin=\"$(command -v node || true)\"; if [ -z \"$node_bin\" ]; then printf '%s\\n' 'node not found' >&2; exit 127; fi; \"$node_bin\" --version";
    Ok(run_ssh_command(target, command, None)?.trim().to_string())
}

fn parse_codex_app_server_pids(output: &str) -> Vec<u32> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let command = fields.next()?;
            if command != "codex" {
                return None;
            }

            let args = fields.collect::<Vec<_>>();
            let is_app_server = args.windows(2).any(|pair| {
                pair[0] == "app-server" && (pair[1] == "proxy" || pair[1] == "--listen")
            });
            is_app_server.then_some(pid)
        })
        .collect()
}

fn reload_remote_codex_app_server(
    target: &super::remote::ResolvedSshTarget,
) -> RemoteCodexReloadResult {
    let processes = match run_ssh_command(target, "ps -u \"$(id -u)\" -o pid=,comm=,args=", None) {
        Ok(output) => output,
        Err(error) => {
            return RemoteCodexReloadResult {
                attempted: true,
                stopped_processes: 0,
                reconnect_triggered: false,
                error: Some(error.to_string()),
            };
        }
    };
    let pids = parse_codex_app_server_pids(&processes);
    if pids.is_empty() {
        return RemoteCodexReloadResult {
            attempted: true,
            stopped_processes: 0,
            reconnect_triggered: false,
            error: None,
        };
    }

    // Only native `codex` app-server children are selected. Their Node/shell
    // wrappers exit with them and Codex Desktop reconnects automatically.
    // Normal `codex exec`/TUI processes and the Remote Agent are untouched.
    let pid_list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!(
        "count=0; for pid in {pid_list}; do if kill -TERM \"$pid\" 2>/dev/null; then count=$((count + 1)); fi; done; printf '%s\\n' \"$count\""
    );
    match run_ssh_command(target, &command, None) {
        Ok(output) => {
            let stopped_processes = output.trim().parse::<usize>().unwrap_or(0);
            RemoteCodexReloadResult {
                attempted: true,
                stopped_processes,
                reconnect_triggered: stopped_processes > 0,
                error: None,
            }
        }
        Err(error) => RemoteCodexReloadResult {
            attempted: true,
            stopped_processes: 0,
            reconnect_triggered: false,
            error: Some(error.to_string()),
        },
    }
}

fn remote_file_exists(
    target: &super::remote::ResolvedSshTarget,
    path: &str,
) -> Result<bool, AppError> {
    let output = run_ssh_command(
        target,
        &format!("if [ -f {path} ]; then printf '1\\n'; else printf '0\\n'; fi"),
        None,
    )?;
    Ok(output.trim() == "1")
}

fn remote_agent_pid(target: &super::remote::ResolvedSshTarget) -> Result<Option<u32>, AppError> {
    let output = run_ssh_command(
        target,
        &format!("if [ -f {AGENT_PID_PATH} ]; then cat {AGENT_PID_PATH}; fi"),
        None,
    )?;
    Ok(output.trim().parse::<u32>().ok())
}

fn write_remote_agent_file(
    target: &super::remote::ResolvedSshTarget,
    remote_path: &str,
    content: &[u8],
    executable: bool,
) -> Result<(), AppError> {
    let mode = if executable { "700" } else { "600" };
    let command = format!(
        "set -e; umask 077; file=\"{remote_path}\"; dir=\"${{file%/*}}\"; mkdir -p \"$dir\"; tmp=\"$file.tmp.$$\"; cat > \"$tmp\"; mv \"$tmp\" \"$file\"; chmod {mode} \"$file\""
    );
    run_ssh_command(target, &command, Some(content))?;
    Ok(())
}

fn start_remote_agent(target: &super::remote::ResolvedSshTarget) -> Result<(), AppError> {
    let command = format!(
        "set -e; cd {AGENT_DIR}; PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/opt/homebrew/bin:$HOME/.local/bin:$HOME/bin\"; nohup node nexus-remote-agent.js >> agent.log 2>&1 < /dev/null & printf '%s\\n' $! > agent.pid"
    );
    run_ssh_command(target, &command, None)?;
    Ok(())
}

fn stop_existing_remote_agent_for_redeploy(
    target: &super::remote::ResolvedSshTarget,
) -> Result<(), AppError> {
    // A redeploy restarts the same provider route. Do not migrate its threads
    // to the preserved profile during this internal stop.
    let _ = remote_control_post(target, "/stop", &json!({ "maintainHistory": false }));
    let command = format!(
        "set -e; \
         pid=''; [ -f {AGENT_PID_PATH} ] && pid=\"$(cat {AGENT_PID_PATH} 2>/dev/null || true)\"; \
         if [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null; then kill \"$pid\" 2>/dev/null || true; fi; \
         i=0; while [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null && [ \"$i\" -lt 50 ]; do sleep 0.1; i=$((i + 1)); done; \
         if [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null; then kill -9 \"$pid\" 2>/dev/null || true; fi; \
         rm -f {AGENT_PID_PATH}"
    );
    run_ssh_command(target, &command, None)?;
    Ok(())
}

fn restore_remote_agent_without_control_api(
    target: &super::remote::ResolvedSshTarget,
) -> Result<Value, AppError> {
    let command = format!(
        "set -e; if [ -f {AGENT_PID_PATH} ]; then kill \"$(cat {AGENT_PID_PATH})\" 2>/dev/null || true; fi; PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/opt/homebrew/bin:$HOME/.local/bin:$HOME/bin\"; if [ -f {AGENT_SCRIPT_PATH} ]; then node {AGENT_SCRIPT_PATH} --restore; else printf '%s\\n' '{{\"ok\":false,\"restored_backup\":false,\"error\":\"agent not installed\"}}'; fi"
    );
    let output = run_ssh_command(target, &command, None)?;
    serde_json::from_str(output.trim()).map_err(|e| {
        AppError::Message(format!(
            "Failed to parse remote agent restore response: {e}"
        ))
    })
}

fn wait_remote_agent_status(target: &super::remote::ResolvedSshTarget) -> Option<Value> {
    for _ in 0..10 {
        if let Ok(status) = remote_control_get(target, "/status") {
            return Some(status);
        }
        let _ = run_ssh_command(target, "sleep 0.2", None);
    }
    None
}

fn remote_control_get(
    target: &super::remote::ResolvedSshTarget,
    path: &str,
) -> Result<Value, AppError> {
    let command = format!("curl -fsS --max-time 5 http://127.0.0.1:{DEFAULT_CONTROL_PORT}{path}");
    let output = run_ssh_command(target, &command, None)?;
    serde_json::from_str(output.trim())
        .map_err(|e| AppError::Message(format!("Failed to parse remote agent response: {e}")))
}

fn remote_control_post<T: Serialize>(
    target: &super::remote::ResolvedSshTarget,
    path: &str,
    payload: &T,
) -> Result<Value, AppError> {
    let body = serde_json::to_vec(payload).map_err(|e| AppError::JsonSerialize { source: e })?;
    let command = format!(
        "curl -fsS --max-time 90 -X POST -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:{DEFAULT_CONTROL_PORT}{path}"
    );
    let output = run_ssh_command(target, &command, Some(&body))?;
    serde_json::from_str(output.trim())
        .map_err(|e| AppError::Message(format!("Failed to parse remote agent response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_agents_with_built_in_history_maintenance() {
        assert!(!agent_version_maintains_history("0.1.6"));
        assert!(agent_version_maintains_history("0.1.7"));
        assert!(agent_version_maintains_history("v0.2.0"));
        assert!(!agent_version_maintains_history("invalid"));
    }

    #[test]
    fn normalizes_chat_completions_endpoint_to_provider_base_url() {
        assert_eq!(
            normalize_chat_base_url("http://example.test/v1/chat/completions"),
            "http://example.test/v1"
        );
        assert_eq!(
            normalize_chat_base_url("http://example.test/v1/"),
            "http://example.test/v1"
        );
    }

    #[test]
    fn attaches_history_maintenance_to_control_response() {
        let response = attach_history_maintenance(
            json!({ "ok": true, "restored_backup": true }),
            json!({ "jsonlFiles": 2, "stateRows": 2 }),
        );
        assert_eq!(response["history_maintenance"]["jsonlFiles"], 2);
        assert_eq!(response["history_maintenance"]["stateRows"], 2);
    }

    #[test]
    fn selects_only_native_codex_app_server_processes_for_reload() {
        let processes = r#"
1693999 node node /usr/local/bin/codex -c features.code_mode_host=true app-server --listen unix://
1694047 codex /opt/codex -c features.code_mode_host=true app-server --listen unix://
1696263 sh /bin/sh -c codex app-server proxy
1696277 node node /usr/local/bin/codex app-server proxy
1696300 codex /opt/codex app-server proxy
1696400 codex /opt/codex exec --json inspect repo
1696500 codex /opt/codex resume thread-id
"#;

        assert_eq!(
            parse_codex_app_server_pids(processes),
            vec![1_694_047, 1_696_300]
        );
    }

    #[test]
    fn attaches_codex_reload_to_stop_response() {
        let response = attach_codex_reload(
            json!({ "ok": true }),
            RemoteCodexReloadResult {
                attempted: true,
                stopped_processes: 2,
                reconnect_triggered: true,
                error: None,
            },
        );

        assert_eq!(response["codex_reload"]["stoppedProcesses"], 2);
        assert_eq!(response["codex_reload"]["reconnectTriggered"], true);
    }
}
