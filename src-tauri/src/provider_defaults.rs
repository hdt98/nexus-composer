use once_cell::sync::Lazy;
use std::collections::HashMap;

/// Provider icon metadata.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProviderIcon {
    pub name: &'static str,
    pub color: &'static str,
}

/// Default mapping from provider names to icons.
#[allow(dead_code)]
pub static DEFAULT_PROVIDER_ICONS: Lazy<HashMap<&'static str, ProviderIcon>> = Lazy::new(|| {
    let mut m = HashMap::new();

    // AI providers.
    m.insert(
        "openai",
        ProviderIcon {
            name: "openai",
            color: "#00A67E",
        },
    );
    m.insert(
        "anthropic",
        ProviderIcon {
            name: "anthropic",
            color: "#D4915D",
        },
    );
    m.insert(
        "claude",
        ProviderIcon {
            name: "claude",
            color: "#D4915D",
        },
    );
    m.insert(
        "google",
        ProviderIcon {
            name: "google",
            color: "#4285F4",
        },
    );
    m.insert(
        "gemini",
        ProviderIcon {
            name: "gemini",
            color: "#4285F4",
        },
    );
    m.insert(
        "deepseek",
        ProviderIcon {
            name: "deepseek",
            color: "#1E88E5",
        },
    );
    m.insert(
        "kimi",
        ProviderIcon {
            name: "kimi",
            color: "#6366F1",
        },
    );
    m.insert(
        "moonshot",
        ProviderIcon {
            name: "moonshot",
            color: "#6366F1",
        },
    );
    m.insert(
        "zhipu",
        ProviderIcon {
            name: "zhipu",
            color: "#0F62FE",
        },
    );
    m.insert(
        "minimax",
        ProviderIcon {
            name: "minimax",
            color: "#FF6B6B",
        },
    );
    m.insert(
        "baidu",
        ProviderIcon {
            name: "baidu",
            color: "#2932E1",
        },
    );
    m.insert(
        "alibaba",
        ProviderIcon {
            name: "alibaba",
            color: "#FF6A00",
        },
    );
    m.insert(
        "tencent",
        ProviderIcon {
            name: "tencent",
            color: "#00A4FF",
        },
    );
    m.insert(
        "meta",
        ProviderIcon {
            name: "meta",
            color: "#0081FB",
        },
    );
    m.insert(
        "microsoft",
        ProviderIcon {
            name: "microsoft",
            color: "#00A4EF",
        },
    );
    m.insert(
        "cohere",
        ProviderIcon {
            name: "cohere",
            color: "#39594D",
        },
    );
    m.insert(
        "perplexity",
        ProviderIcon {
            name: "perplexity",
            color: "#20808D",
        },
    );
    m.insert(
        "mistral",
        ProviderIcon {
            name: "mistral",
            color: "#FF7000",
        },
    );
    m.insert(
        "huggingface",
        ProviderIcon {
            name: "huggingface",
            color: "#FFD21E",
        },
    );

    // Cloud platforms.
    m.insert(
        "aws",
        ProviderIcon {
            name: "aws",
            color: "#FF9900",
        },
    );
    m.insert(
        "azure",
        ProviderIcon {
            name: "azure",
            color: "#0078D4",
        },
    );
    m.insert(
        "huawei",
        ProviderIcon {
            name: "huawei",
            color: "#FF0000",
        },
    );
    m.insert(
        "cloudflare",
        ProviderIcon {
            name: "cloudflare",
            color: "#F38020",
        },
    );

    m
});

/// Infers an icon from a provider name.
#[allow(dead_code)]
pub fn infer_provider_icon(provider_name: &str) -> Option<ProviderIcon> {
    let name_lower = provider_name.to_lowercase();

    // Exact match.
    if let Some(icon) = DEFAULT_PROVIDER_ICONS.get(name_lower.as_str()) {
        return Some(icon.clone());
    }

    // Partial match containing a known keyword.
    for (key, icon) in DEFAULT_PROVIDER_ICONS.iter() {
        if name_lower.contains(key) {
            return Some(icon.clone());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let icon = infer_provider_icon("openai");
        assert!(icon.is_some());
        let icon = icon.unwrap();
        assert_eq!(icon.name, "openai");
        assert_eq!(icon.color, "#00A67E");
    }

    #[test]
    fn test_fuzzy_match() {
        let icon = infer_provider_icon("OpenAI Official");
        assert!(icon.is_some());
        let icon = icon.unwrap();
        assert_eq!(icon.name, "openai");
    }

    #[test]
    fn test_case_insensitive() {
        let icon = infer_provider_icon("ANTHROPIC");
        assert!(icon.is_some());
        assert_eq!(icon.unwrap().name, "anthropic");
    }

    #[test]
    fn test_no_match() {
        let icon = infer_provider_icon("unknown provider");
        assert!(icon.is_none());
    }
}
