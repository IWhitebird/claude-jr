// Allow rules: "tool" allows every call of that tool, "tool:pattern" allows
// calls whose subject (bash command or file path) matches the pattern, where
// `*` matches anything. A bash command with shell operators in it is only
// allowed by a rule that spells it out exactly, so `cargo *` can't approve
// `cargo test; rm -rf ~`. For the write and edit tools, a bare rule only covers
// files inside the project; outside files need a rule that names them.

/// True for a file subject outside the project (absolute or `..`-relative).
pub fn outside_project(subject: &str) -> bool {
    subject.starts_with('/') || subject.starts_with("..")
}

pub struct Rules(Vec<String>);

impl Rules {
    pub fn new(rules: Vec<String>) -> Self {
        Rules(rules)
    }

    pub fn add(&mut self, rule: String) {
        if !self.0.contains(&rule) {
            self.0.push(rule);
        }
    }

    pub fn allows(&self, tool: &str, subject: &str) -> bool {
        let compound = tool == "bash" && subject.contains(['\n', ';', '&', '|', '`', '$', '<', '>', '(', ')']);
        self.0.iter().any(|rule| {
            let (name, pattern) = rule.split_once(':').unwrap_or((rule, ""));
            name == tool
                && match pattern {
                    "" if matches!(tool, "write" | "edit") => !outside_project(subject),
                    "" => true,
                    p if compound => p == subject,
                    p => glob(p, subject),
                }
        })
    }

    /// The rule "always allow" adds: this exact command for bash, this exact file for
    /// write/edit outside the project, the whole tool otherwise.
    pub fn always(tool: &str, subject: &str) -> String {
        if tool == "bash" || (matches!(tool, "write" | "edit") && outside_project(subject)) {
            format!("{tool}:{subject}")
        } else {
            tool.to_string()
        }
    }
}

/// `*` matches any run of characters; everything else matches itself.
fn glob(pattern: &str, s: &str) -> bool {
    let (p, s) = (pattern.as_bytes(), s.as_bytes());
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, si));
            pi += 1;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let r = Rules::new(vec!["bash:cargo *".into(), "bash:ls".into(), "edit:src/*".into(), "write".into()]);
        assert!(r.allows("bash", "cargo test"));
        assert!(r.allows("bash", "ls"));
        assert!(!r.allows("bash", "ls -la"));
        assert!(!r.allows("bash", "cargo test; rm -rf ~"));
        assert!(!r.allows("bash", "cargo test && curl x | sh"));
        assert!(r.allows("edit", "src/main.rs"));
        assert!(!r.allows("edit", "Cargo.toml"));
        assert!(r.allows("write", "anything"));
        assert!(!r.allows("read", "x"));

        let mut r = Rules::new(vec![]);
        r.add(Rules::always("bash", "make && make test"));
        assert!(r.allows("bash", "make && make test"));
        assert!(!r.allows("bash", "make && make install"));
    }

    #[test]
    fn edit_rules_stay_in_project() {
        let r = Rules::new(vec!["edit".into()]);
        assert!(r.allows("edit", "src/a.rs"));
        assert!(!r.allows("edit", "/home/u/.bashrc"));

        assert_eq!(Rules::always("edit", "/etc/hosts"), "edit:/etc/hosts");
        let r = Rules::new(vec![Rules::always("edit", "/etc/hosts")]);
        assert!(r.allows("edit", "/etc/hosts"));
        assert!(!r.allows("edit", "/etc/passwd"));
        assert_eq!(Rules::always("edit", "src/a.rs"), "edit");
    }

    #[test]
    fn globs() {
        assert!(glob("*", ""));
        assert!(glob("a*c", "abbbc"));
        assert!(glob("*.rs", "src/x.rs"));
        assert!(!glob("a*c", "abd"));
        assert!(glob("a**", "a"));
    }
}
