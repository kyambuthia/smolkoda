use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    OpenAi,
    Anthropic,
}

impl Protocol {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Auth {
    Bearer,
    ApiKey,
    XApiKey,
    None,
}

impl Auth {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "bearer" => Some(Self::Bearer),
            "api-key" | "apikey" => Some(Self::ApiKey),
            "x-api-key" | "xapikey" => Some(Self::XApiKey),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::ApiKey => "api-key",
            Self::XApiKey => "x-api-key",
            Self::None => "none",
        }
    }
}

pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub protocol: Protocol,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub env: &'static [&'static str],
    pub auth: Auth,
    pub models: &'static [&'static str],
    pub headers: &'static [(&'static str, &'static str)],
    pub max_tokens_param: &'static str,
    pub note: &'static str,
}

pub static PRESETS: &[Preset] = &[
    Preset {
        id: "anthropic",
        label: "Anthropic",
        protocol: Protocol::Anthropic,
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-sonnet-4-5",
        env: &["ANTHROPIC_API_KEY"],
        auth: Auth::XApiKey,
        models: &["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "openai",
        label: "OpenAI",
        protocol: Protocol::OpenAi,
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-5.2",
        env: &["OPENAI_API_KEY"],
        auth: Auth::Bearer,
        models: &["gpt-5.2", "gpt-5.1", "gpt-5-mini"],
        headers: &[],
        max_tokens_param: "max_completion_tokens",
        note: "",
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        protocol: Protocol::OpenAi,
        base_url: "https://openrouter.ai/api/v1",
        default_model: "anthropic/claude-sonnet-4.5",
        env: &["OPENROUTER_API_KEY"],
        auth: Auth::Bearer,
        models: &[
            "anthropic/claude-sonnet-4.5",
            "openai/gpt-5.2",
            "google/gemini-2.5-pro",
        ],
        headers: &[("X-Title", "smolkoda")],
        max_tokens_param: "max_tokens",
        note: "One key for most hosted models.",
    },
    Preset {
        id: "runinfra",
        label: "RunInfra",
        protocol: Protocol::OpenAi,
        base_url: "https://api.runinfra.ai/v1",
        default_model: "glm-5-3-flash",
        env: &["RUNINFRA_GATEWAY_KEY", "RUNINFRA_API_KEY"],
        auth: Auth::Bearer,
        models: &[
            "glm-5-3-flash",
            "glm-5-3",
            "deepseek-v4-1-flash",
            "deepseek-v4-1-flash-1017",
            "nemotron-3-5-lightning-30b",
            "qwen3-8-27b",
            "ornith-1-5-122b",
            "ornith-1-5-35b",
        ],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "OpenAI-compatible; Anthropic Messages is also available at /v1/messages.",
    },
    Preset {
        id: "google",
        label: "Google Gemini",
        protocol: Protocol::OpenAi,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: "gemini-2.5-pro",
        env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        auth: Auth::Bearer,
        models: &["gemini-2.5-pro", "gemini-2.5-flash"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "Uses Gemini's OpenAI-compatible endpoint.",
    },
    Preset {
        id: "deepseek",
        label: "DeepSeek",
        protocol: Protocol::OpenAi,
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        env: &["DEEPSEEK_API_KEY"],
        auth: Auth::Bearer,
        models: &["deepseek-chat", "deepseek-reasoner"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "xai",
        label: "xAI",
        protocol: Protocol::OpenAi,
        base_url: "https://api.x.ai/v1",
        default_model: "grok-4",
        env: &["XAI_API_KEY"],
        auth: Auth::Bearer,
        models: &["grok-4", "grok-3"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "groq",
        label: "Groq",
        protocol: Protocol::OpenAi,
        base_url: "https://api.groq.com/openai/v1",
        default_model: "moonshotai/kimi-k2-instruct",
        env: &["GROQ_API_KEY"],
        auth: Auth::Bearer,
        models: &["moonshotai/kimi-k2-instruct", "llama-3.3-70b-versatile"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "mistral",
        label: "Mistral",
        protocol: Protocol::OpenAi,
        base_url: "https://api.mistral.ai/v1",
        default_model: "mistral-large-latest",
        env: &["MISTRAL_API_KEY"],
        auth: Auth::Bearer,
        models: &["mistral-large-latest", "mistral-medium-latest"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "cohere",
        label: "Cohere",
        protocol: Protocol::OpenAi,
        base_url: "https://api.cohere.ai/compatibility/v1",
        default_model: "command-a-03-2025",
        env: &["COHERE_API_KEY"],
        auth: Auth::Bearer,
        models: &["command-a-03-2025"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "together",
        label: "Together AI",
        protocol: Protocol::OpenAi,
        base_url: "https://api.together.xyz/v1",
        default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        env: &["TOGETHER_API_KEY"],
        auth: Auth::Bearer,
        models: &["meta-llama/Llama-3.3-70B-Instruct-Turbo"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "fireworks",
        label: "Fireworks AI",
        protocol: Protocol::OpenAi,
        base_url: "https://api.fireworks.ai/inference/v1",
        default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct",
        env: &["FIREWORKS_API_KEY"],
        auth: Auth::Bearer,
        models: &["accounts/fireworks/models/llama-v3p3-70b-instruct"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "cerebras",
        label: "Cerebras",
        protocol: Protocol::OpenAi,
        base_url: "https://api.cerebras.ai/v1",
        default_model: "llama-3.3-70b",
        env: &["CEREBRAS_API_KEY"],
        auth: Auth::Bearer,
        models: &["llama-3.3-70b"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "moonshot",
        label: "Moonshot / Kimi",
        protocol: Protocol::OpenAi,
        base_url: "https://api.moonshot.ai/v1",
        default_model: "kimi-k2-0905-preview",
        env: &["MOONSHOT_API_KEY"],
        auth: Auth::Bearer,
        models: &["kimi-k2-0905-preview"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "github",
        label: "GitHub Models",
        protocol: Protocol::OpenAi,
        base_url: "https://models.inference.ai.azure.com",
        default_model: "openai/gpt-4.1",
        env: &["GITHUB_TOKEN", "GH_TOKEN"],
        auth: Auth::Bearer,
        models: &["openai/gpt-4.1", "openai/gpt-4o"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "Uses a GitHub token with Models access.",
    },
    Preset {
        id: "ollama",
        label: "Ollama (local)",
        protocol: Protocol::OpenAi,
        base_url: "http://localhost:11434/v1",
        default_model: "qwen3:8b",
        env: &[],
        auth: Auth::None,
        models: &["qwen3:8b", "qwen3:32b", "llama3.3"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "Start Ollama first; tool support depends on the model.",
    },
    Preset {
        id: "lmstudio",
        label: "LM Studio (local)",
        protocol: Protocol::OpenAi,
        base_url: "http://localhost:1234/v1",
        default_model: "qwen3-8b",
        env: &[],
        auth: Auth::None,
        models: &["qwen3-8b"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "vllm",
        label: "vLLM (local)",
        protocol: Protocol::OpenAi,
        base_url: "http://localhost:8000/v1",
        default_model: "Qwen/Qwen3-32B",
        env: &[],
        auth: Auth::None,
        models: &["Qwen/Qwen3-32B"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "llamacpp",
        label: "llama.cpp (local)",
        protocol: Protocol::OpenAi,
        base_url: "http://localhost:8080/v1",
        default_model: "local-model",
        env: &[],
        auth: Auth::None,
        models: &["local-model"],
        headers: &[],
        max_tokens_param: "max_tokens",
        note: "",
    },
    Preset {
        id: "azure",
        label: "Azure OpenAI",
        protocol: Protocol::OpenAi,
        base_url: "https://YOUR-RESOURCE.openai.azure.com/openai/deployments/YOUR-DEPLOYMENT/chat/completions?api-version=2024-10-21",
        default_model: "gpt-4o",
        env: &["AZURE_OPENAI_API_KEY", "AZURE_API_KEY"],
        auth: Auth::ApiKey,
        models: &["gpt-4o", "gpt-4.1"],
        headers: &[],
        max_tokens_param: "max_completion_tokens",
        note: "Replace YOUR-RESOURCE and YOUR-DEPLOYMENT in the URL.",
    },
];

pub static AUTO_ORDER: &[&str] = &[
    "anthropic",
    "openai",
    "openrouter",
    "runinfra",
    "google",
    "deepseek",
    "xai",
    "groq",
    "mistral",
];

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

pub fn default_key_ref(preset: Option<&Preset>) -> Option<String> {
    preset.and_then(|preset| {
        preset
            .env
            .iter()
            .find(|name| std::env::var_os(name).is_some())
            .map(|name| format!("env:{name}"))
    })
}
