// Claude through a cloud's own door: Amazon Bedrock ("bedrock/MODEL") and
// Google Cloud's Vertex AI ("vertex/MODEL"). Both take Anthropic's request
// and answer with Anthropic's stream. What differs is the address, the login
// (bedrock.rs, vertex.rs) and a few things neither offers: the refusal
// fallback, the web tools and remote MCP servers are left out of a request.

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cloud {
    Bedrock,
    Vertex,
}

/// Reads a setting. The environment, except in tests.
pub type Env<'a> = &'a (dyn Fn(&str) -> Option<String> + Send + Sync);

/// The same, owned by the client.
pub(super) type Settings = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A setting that is there and not blank.
pub(super) fn set(env: Env, name: &str) -> Option<String> {
    env(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// The cloud a model id names, and the id that cloud knows the model by.
pub fn of(model: &str) -> Option<(Cloud, &str)> {
    let (door, id) = model.split_once('/')?;
    let cloud = match door {
        "bedrock" => Cloud::Bedrock,
        "vertex" => Cloud::Vertex,
        _ => return None,
    };
    (!id.is_empty()).then_some((cloud, id))
}

/// Claude's own name for a model, whichever door it is asked through:
/// "bedrock/anthropic.claude-opus-5-5" and "vertex/claude-opus-5-5" are both
/// "claude-opus-5-5". Prices and capabilities are looked up by this.
pub fn claude(model: &str) -> &str {
    match of(model) {
        Some((Cloud::Bedrock, id)) => id.rsplit_once("anthropic.").map_or(id, |(_, name)| name),
        Some((Cloud::Vertex, id)) => id.split('@').next().unwrap_or(id),
        None => model,
    }
}

/// `pick`, asked through the same door as `current`: someone using Bedrock
/// who picks another Claude model from a list means that model on Bedrock.
pub fn same_door(current: &str, pick: &str) -> String {
    match (of(current), current.split_once('/')) {
        (Some(_), Some((door, _))) if of(pick).is_none() => format!("{door}/{pick}"),
        _ => pick.to_string(),
    }
}

/// Why `model`'s cloud can't be used yet, if it can't. None for any other model.
pub fn missing(model: &str, env: Env) -> Option<String> {
    match of(model)?.0 {
        Cloud::Bedrock => super::bedrock::login(env).err().map(|e| e.to_string()),
        Cloud::Vertex => super::vertex::project(env).err().map(|e| e.to_string()),
    }
}

/// The real environment.
pub fn environment(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cloud_is_named_in_front_and_the_model_keeps_claudes_name() {
        assert_eq!(of("bedrock/claude-opus-5-5"), Some((Cloud::Bedrock, "claude-opus-5-5")));
        assert_eq!(of("vertex/claude-haiku-4-5@20251001"), Some((Cloud::Vertex, "claude-haiku-4-5@20251001")));
        assert_eq!(of("claude-opus-5-5"), None);
        assert_eq!(of("openai/gpt-5"), None);
        assert_eq!(of("vertex/"), None);

        assert_eq!(claude("bedrock/claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(claude("bedrock/anthropic.claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(claude("bedrock/us.anthropic.claude-sonnet-5"), "claude-sonnet-5");
        assert_eq!(claude("vertex/claude-haiku-4-5@20251001"), "claude-haiku-4-5");
        assert_eq!(claude("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(claude("ollama/qwen3"), "ollama/qwen3");
    }

    #[test]
    fn a_pick_from_a_list_stays_with_the_cloud_in_use() {
        assert_eq!(same_door("bedrock/claude-opus-5-5", "claude-haiku-5-5"), "bedrock/claude-haiku-5-5");
        assert_eq!(same_door("vertex/claude-opus-4-5@20251101", "claude-haiku-5-5"), "vertex/claude-haiku-5-5");
        assert_eq!(same_door("claude-opus-5-5", "claude-haiku-5-5"), "claude-haiku-5-5");
        assert_eq!(same_door("ollama/qwen3", "claude-haiku-5-5"), "claude-haiku-5-5");
        assert_eq!(same_door("bedrock/claude-opus-5-5", "vertex/claude-haiku-5-5"), "vertex/claude-haiku-5-5");
    }

    #[test]
    fn what_is_missing_is_said_before_anything_is_sent() {
        let none = |_: &str| None;
        assert!(missing("claude-opus-5-5", &none).is_none());
        assert!(missing("bedrock/claude-opus-5-5", &none).unwrap().contains("AWS_BEARER_TOKEN_BEDROCK"));
        assert!(missing("vertex/claude-opus-5-5", &none).unwrap().contains("ANTHROPIC_VERTEX_PROJECT_ID"));
        let set = |name: &str| matches!(name, "AWS_BEARER_TOKEN_BEDROCK" | "GOOGLE_CLOUD_PROJECT").then(|| "x".into());
        assert!(missing("bedrock/claude-opus-5-5", &set).is_none());
        assert!(missing("vertex/claude-opus-5-5", &set).is_none());
    }
}
