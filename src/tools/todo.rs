// todo: a short task list the model keeps for multi-step work.

use super::{Output, Tools, def};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::sync::LazyLock;

pub static TODO: LazyLock<Box<RawValue>> = LazyLock::new(|| {
    def(json!({
        "name": "todo",
        "description": "Keep a task list for work with several steps, so you and the user can see progress. \
            Send the whole list each time. Keep exactly one item in_progress while working, and mark items \
            completed as soon as they're done. Skip it for small tasks.",
        "input_schema": {
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string" },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        }
    }))
});

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    InProgress,
    Completed,
}

#[derive(Deserialize)]
pub struct Todo {
    content: String,
    status: Status,
}

pub fn update(t: &Tools, input: &Value) -> Output {
    let todos: Vec<Todo> = serde_json::from_value(input["todos"].clone()).map_err(|e| format!("invalid todos: {e}"))?;
    let text = render(&todos);
    *t.todos.lock().unwrap() = todos;
    Ok(text)
}

fn render(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "(no tasks)".into();
    }
    let done = todos.iter().filter(|t| t.status == Status::Completed).count();
    let mut s = format!("{done}/{} done\n", todos.len());
    for t in todos {
        let mark = match t.status {
            Status::Pending => "□",
            Status::InProgress => "▸",
            Status::Completed => "✓",
        };
        s.push_str(&format!("{mark} {}\n", t.content));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::super::test_tools;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn renders_list() {
        let (t, _) = test_tools("todo");
        let input = json!({"todos": [
            {"content": "read code", "status": "completed"},
            {"content": "fix bug", "status": "in_progress"},
            {"content": "run tests", "status": "pending"}
        ]});
        let out = t.run("todo", &input, &AtomicBool::new(false)).unwrap();
        assert_eq!(out, "1/3 done\n✓ read code\n▸ fix bug\n□ run tests\n");
        assert!(t.run("todo", &json!({"todos": [{"content": 1}]}), &AtomicBool::new(false)).is_err());
    }
}
