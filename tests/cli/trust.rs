// A project's own config files are used only once the folder is trusted.

use super::common::*;

#[test]
fn untrusted_project_files_are_ignored() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    std::fs::write(e.proj.join(".claude-jr.json"), r#"{"model":"project-model"}"#).unwrap();
    std::fs::write(e.proj.join(".mcp.json"), r#"{"mcpServers":{"evil":{"command":"/bin/false"}}}"#).unwrap();
    let o = e.run(&["-p", "hi", "-y"]);
    ok(&o);
    assert!(o.err.contains("ignoring .claude-jr.json and .mcp.json: this folder isn't trusted"), "{}", o.err);
    assert_eq!(e.requests()[0]["model"], "claude-opus-5-5");
    assert!(!o.err.contains("MCP server evil"), "{}", o.err);
}

#[test]
fn trusted_project_files_are_honoured() {
    let e = Env::with(&[("MOCK_ROUNDS", "0")]);
    std::fs::write(e.proj.join(".claude-jr.json"), r#"{"model":"project-model"}"#).unwrap();
    std::fs::write(e.proj.join(".mcp.json"), r#"{"mcpServers":{"broken":{"command":"/nonexistent/server"}}}"#).unwrap();
    e.trust_project();
    let o = e.run(&["-p", "hi", "-y"]);
    ok(&o);
    assert!(!o.err.contains("isn't trusted"), "{}", o.err);
    assert_eq!(e.requests()[0]["model"], "project-model");
    assert!(o.err.contains("MCP server broken"), "the project's .mcp.json was read: {}", o.err);
}
