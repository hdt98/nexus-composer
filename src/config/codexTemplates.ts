/**
 * Codex configuration template used as the default for new custom providers.
 */

export interface CodexTemplate {
  auth: Record<string, any>;
  config: string;
}

/**
 * Gets the custom Codex template.
 * @returns Codex template configuration.
 */
export function getCodexCustomTemplate(): CodexTemplate {
  const config = `model_provider = "custom"
model = "gpt-5.5"
model_reasoning_effort = "high"
disable_response_storage = true

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true`;

  return {
    auth: { OPENAI_API_KEY: "" },
    config,
  };
}
