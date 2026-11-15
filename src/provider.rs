// Which API a model is asked through. A model id with a provider's name in
// front ("ollama/qwen3", "openai/gpt-5", "openrouter/x/y") goes to that
// provider; a plain id goes to Anthropic, as it always did.
//
// There are two wire formats. "anthropic" is the Messages API, also spoken by
// several other vendors and gateways. "openai" is Chat Completions, which
// nearly everything else speaks, local servers included. Conversations are
// kept in the Anthropic format and translated on the way out and back (see
// openai.rs), so nothing else in the program knows the difference.
//
// More can be added in the config file:
//
//   "providers": {
//     "work": { "base_url": "https://llm.example.com/v1", "api": "openai", "key_env": "WORK_LLM_KEY" }
//   }
//
// and then used as `-m work/some-model`.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Anthropic,
    OpenAi,
}

/// A provider from the config file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub base_url: String,
    #[serde(default = "openai")]
    pub api: Shape,
    /// The environment variable that holds the key. None: no key is sent.
    pub key_env: Option<String>,
    /// The model's context size in tokens, for the footer.
    pub context: Option<u64>,
    /// The most tokens a reply may have. None: the provider's own default.
    pub max_tokens: Option<u32>,
}

fn openai() -> Shape {
    Shape::OpenAi
}

/// Where to send a request, resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Provider {
    pub name: String,
    pub shape: Shape,
    /// Without a trailing slash.
    pub base_url: String,
    pub key_env: Option<String>,
    pub context: Option<u64>,
    pub max_tokens: Option<u32>,
}

impl Provider {
    /// The key to send, if the provider takes one and it is set.
    pub fn key(&self) -> Option<String> {
        let key = std::env::var(self.key_env.as_deref()?).ok()?;
        (!key.trim().is_empty()).then(|| key.trim().to_string())
    }

    /// Why this provider can't be used yet, if it can't.
    pub fn missing_key(&self) -> Option<String> {
        let env = self.key_env.as_deref()?;
        self.key().is_none().then(|| format!("{} needs an API key: set {env}", self.name))
    }
}

/// (name, format, base URL, the key's environment variable).
const BUILT_IN: &[(&str, Shape, &str, Option<&str>)] = &[
    ("openai", Shape::OpenAi, "https://api.openai.com/v1", Some("OPENAI_API_KEY")),
    ("openrouter", Shape::OpenAi, "https://openrouter.ai/api/v1", Some("OPENROUTER_API_KEY")),
    ("gemini", Shape::OpenAi, "https://generativelanguage.googleapis.com/v1beta/openai", Some("GEMINI_API_KEY")),
    ("groq", Shape::OpenAi, "https://api.groq.com/openai/v1", Some("GROQ_API_KEY")),
    ("deepseek", Shape::OpenAi, "https://api.deepseek.com/v1", Some("DEEPSEEK_API_KEY")),
    ("mistral", Shape::OpenAi, "https://api.mistral.ai/v1", Some("MISTRAL_API_KEY")),
    ("xai", Shape::OpenAi, "https://api.x.ai/v1", Some("XAI_API_KEY")),
    // Local servers, on the ports they use by default. No key.
    ("ollama", Shape::OpenAi, "http://localhost:11434/v1", None),
    ("lmstudio", Shape::OpenAi, "http://localhost:1234/v1", None),
    ("llamacpp", Shape::OpenAi, "http://localhost:8080/v1", None),
];

pub type Providers = BTreeMap<String, ProviderConfig>;

/// The provider a model id names and the id the provider knows the model by.
/// None for a plain id: that is Anthropic's own API.
pub fn resolve<'a>(model: &'a str, custom: &Providers) -> Option<(Provider, &'a str)> {
    let (name, id) = model.split_once('/')?;
    if id.is_empty() {
        return None;
    }
    if let Some(p) = custom.get(name) {
        let provider = Provider {
            name: name.to_string(),
            shape: p.api,
            base_url: p.base_url.trim_end_matches('/').to_string(),
            key_env: p.key_env.clone(),
            context: p.context,
            max_tokens: p.max_tokens,
        };
        return Some((provider, id));
    }
    let &(_, shape, base, key_env) = BUILT_IN.iter().find(|p| p.0 == name)?;
    // A local server that is somewhere else: OLLAMA_HOST=host:port, as ollama's own tools read it.
    let base_url = match (name, std::env::var("OLLAMA_HOST")) {
        ("ollama", Ok(host)) if !host.trim().is_empty() => {
            let host = host.trim().trim_end_matches('/');
            let host = if host.contains("://") { host.to_string() } else { format!("http://{host}") };
            format!("{host}/v1")
        }
        _ => base.to_string(),
    };
    let provider = Provider {
        name: name.to_string(),
        shape,
        base_url,
        key_env: key_env.map(String::from),
        context: None,
        max_tokens: None,
    };
    Some((provider, id))
}

/// The names that can go in front of a model id, for help and error messages.
pub fn names(custom: &Providers) -> Vec<String> {
    let mut names: Vec<String> = custom.keys().cloned().collect();
    names.extend(BUILT_IN.iter().map(|p| p.0.to_string()).filter(|n| !custom.contains_key(n)));
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_names_the_provider_and_plain_ids_are_anthropics() {
        let none = Providers::new();
        assert!(resolve("claude-opus-5-5", &none).is_none());
        assert!(resolve("some/unknown-prefix", &none).is_none(), "not a provider: left alone");
        assert!(resolve("openai/", &none).is_none());

        let (p, id) = resolve("openai/gpt-5.2", &none).unwrap();
        assert_eq!((p.name.as_str(), p.shape, id), ("openai", Shape::OpenAi, "gpt-5.2"));
        assert_eq!(p.base_url, "https://api.openai.com/v1");
        assert_eq!(p.key_env.as_deref(), Some("OPENAI_API_KEY"));
        // Only the first part is the provider; the rest is the model's own name.
        let (p, id) = resolve("openrouter/anthropic/claude-sonnet-5", &none).unwrap();
        assert_eq!((p.name.as_str(), id), ("openrouter", "anthropic/claude-sonnet-5"));
        let (p, id) = resolve("ollama/qwen3:8b", &none).unwrap();
        assert_eq!((p.key_env.clone(), id), (None, "qwen3:8b"));
        assert!(p.missing_key().is_none(), "a local server needs no key");
    }

    #[test]
    fn the_config_adds_providers_and_can_replace_built_in_ones() {
        let custom: Providers = serde_json::from_str(
            r#"{
                "work": { "base_url": "https://llm.example.com/v1/", "key_env": "CJR_TEST_NO_SUCH_KEY", "context": 32000 },
                "kimi": { "base_url": "https://api.example.ai/anthropic", "api": "anthropic", "key_env": "CJR_TEST_NO_SUCH_KEY" },
                "ollama": { "base_url": "http://gpu-box:11434/v1" }
            }"#,
        )
        .unwrap();
        let (work, id) = resolve("work/big-model", &custom).unwrap();
        assert_eq!(
            (work.shape, work.base_url.as_str(), id),
            (Shape::OpenAi, "https://llm.example.com/v1", "big-model")
        );
        assert_eq!(work.context, Some(32000));
        assert_eq!(work.missing_key().as_deref(), Some("work needs an API key: set CJR_TEST_NO_SUCH_KEY"));
        assert_eq!(resolve("kimi/k2", &custom).unwrap().0.shape, Shape::Anthropic);
        assert_eq!(resolve("ollama/x", &custom).unwrap().0.base_url, "http://gpu-box:11434/v1");
        let names = names(&custom);
        assert!(names.contains(&"work".to_string()) && names.contains(&"groq".to_string()));
        assert_eq!(names.iter().filter(|n| *n == "ollama").count(), 1);
        assert!(serde_json::from_str::<Providers>(r#"{"x": {"base_url": "u", "api": "soap"}}"#).is_err());
        assert!(serde_json::from_str::<Providers>(r#"{"x": {"base_url": "u", "typo": 1}}"#).is_err());
    }
}
