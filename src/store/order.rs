// Keeps a conversation read back from its file in an order the API accepts.
//
// A session file is only ever added to, and a cut-off last line is dealt
// with where the file is opened. But a line can also go missing from the
// middle (a disk that was full, a file someone edited, damage), and the
// messages left then break the API's rules: a conversation starts with the
// user, no message is empty, and a tool call is answered by the message
// right after it and by nothing else. One such message would have every
// later request refused, so the session could never be continued. As the
// file is replayed, what no longer fits is left out.

use super::session::Msg;
use serde::Deserialize;

/// What a message line says about its place in the conversation.
#[derive(Default)]
pub struct Shape {
    pub user: bool,
    /// It has content at all.
    some: bool,
    /// It has text blocks with nothing in them, which have to come out
    /// before it is sent (see `without_blank_text`).
    pub blank: bool,
    /// The tool calls it makes, and those it answers.
    calls: Vec<String>,
    results: Vec<String>,
}

/// Reads a line's shape without keeping its text. None if it is no message.
pub fn shape(line: &str) -> Option<Shape> {
    #[derive(Deserialize)]
    struct Line {
        role: String,
        #[serde(default)]
        content: Shape,
    }
    let Line { role, content } = serde_json::from_str(line).ok()?;
    matches!(role.as_str(), "user" | "assistant").then_some(Shape { user: role == "user", ..content })
}

/// A message's content: plain text, or a list of blocks of which only the
/// tool calls and results matter here.
impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Block {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            id: Option<String>,
            #[serde(default)]
            tool_use_id: Option<String>,
            #[serde(default)]
            text: Option<Blank>,
        }
        /// Whether a text is blank, found out without keeping the text.
        struct Blank(bool);
        impl<'de> Deserialize<'de> for Blank {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct Text;
                impl serde::de::Visitor<'_> for Text {
                    type Value = Blank;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("text")
                    }
                    fn visit_str<E>(self, text: &str) -> Result<Blank, E> {
                        Ok(Blank(text.trim().is_empty()))
                    }
                }
                d.deserialize_str(Text)
            }
        }
        struct Content;
        impl<'de> serde::de::Visitor<'de> for Content {
            type Value = Shape;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("text or a list of content blocks")
            }
            fn visit_str<E>(self, text: &str) -> Result<Shape, E> {
                Ok(Shape { some: !text.is_empty(), ..Shape::default() })
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut blocks: A) -> Result<Shape, A::Error> {
                let mut shape = Shape::default();
                while let Some(block) = blocks.next_element::<Block>()? {
                    // The API takes no text block that is blank.
                    if block.kind == "text" && block.text.is_none_or(|Blank(blank)| blank) {
                        shape.blank = true;
                        continue;
                    }
                    shape.some = true;
                    match (block.kind.as_str(), block.id, block.tool_use_id) {
                        ("tool_use", Some(id), _) => shape.calls.push(id),
                        ("tool_result", _, Some(id)) => shape.results.push(id),
                        _ => {}
                    }
                }
                Ok(shape)
            }
        }
        d.deserialize_any(Content)
    }
}

/// A message line without its blank text blocks. Versions before 0.3.1 kept
/// them as the model sent them.
pub fn without_blank_text(line: &str) -> Option<String> {
    let mut message: serde_json::Value = serde_json::from_str(line).ok()?;
    let blocks = message.get_mut("content")?.as_array_mut()?;
    blocks.retain(|b| b["type"] != "text" || b["text"].as_str().is_some_and(|t| !t.trim().is_empty()));
    Some(message.to_string())
}

/// The state of a replay: which calls are waiting for their results.
#[derive(Default)]
pub struct Order {
    /// Calls made by the newest assistant message and not answered yet, and
    /// where in the conversation that message is.
    open: Vec<String>,
    open_from: usize,
    /// How many messages were left out.
    pub dropped: usize,
}

impl Order {
    /// Adds the next message of the file to the conversation, if it fits.
    pub fn take(&mut self, messages: &mut Vec<Msg>, msg: Msg, shape: Shape) {
        if !shape.some {
            return self.dropped += 1;
        }
        if !msg.user {
            // A conversation starts with the user.
            if messages.is_empty() {
                return self.dropped += 1;
            }
            self.unanswered(messages);
            self.open_from = messages.len();
            self.open = shape.calls;
        } else if !shape.results.is_empty() {
            // Results answer the calls made just before: all of them (they
            // are written together), and no others.
            let all = self.open.iter().all(|id| shape.results.contains(id));
            if !all || !shape.results.iter().all(|id| self.open.contains(id)) {
                return self.dropped += 1;
            }
            self.open.clear();
        } else {
            self.unanswered(messages);
        }
        messages.push(msg);
    }

    /// Calls, and then something other than their results: that line is
    /// gone, and the message that made the calls can't stay without it.
    fn unanswered(&mut self, messages: &mut Vec<Msg>) {
        if !self.open.is_empty() {
            self.dropped += messages.len() - self.open_from;
            messages.truncate(self.open_from);
            self.open.clear();
        }
    }

    /// The conversation was cut back (/rewind): what is open is whatever its
    /// last message left open.
    pub fn cut(&mut self, messages: &[Msg]) {
        self.open.clear();
        if let Some(last) = messages.last().filter(|m| !m.user) {
            self.open_from = messages.len() - 1;
            let calls = last.content().into_iter().filter(|b| b["type"] == "tool_use");
            self.open = calls.filter_map(|b| b["id"].as_str().map(String::from)).collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// Replays these messages and returns the roles and first words of what is kept.
    fn replay(lines: &[Value]) -> (Vec<String>, usize) {
        let (mut order, mut kept) = (Order::default(), Vec::new());
        for line in lines {
            let text = line.to_string();
            match shape(&text) {
                Some(shape) => {
                    order.take(&mut kept, Msg::new(line["role"].as_str().unwrap(), line["content"].clone()), shape)
                }
                None => order.dropped += 1,
            }
        }
        let name = |m: &Msg| {
            let content = m.content();
            let what = content.first().map(|b| b["text"].as_str().or(b["id"].as_str()).or(b["tool_use_id"].as_str()));
            format!("{}:{}", if m.user { "u" } else { "a" }, what.flatten().unwrap_or("?"))
        };
        (kept.iter().map(name).collect(), order.dropped)
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": [{ "type": "text", "text": text }] })
    }
    fn says(text: &str) -> Value {
        json!({ "role": "assistant", "content": [{ "type": "text", "text": text }] })
    }
    fn calls(ids: &[&str]) -> Value {
        let blocks: Vec<Value> =
            ids.iter().map(|id| json!({ "type": "tool_use", "id": id, "name": "bash", "input": {} })).collect();
        json!({ "role": "assistant", "content": blocks })
    }
    fn results(ids: &[&str]) -> Value {
        let blocks: Vec<Value> =
            ids.iter().map(|id| json!({ "type": "tool_result", "tool_use_id": id, "content": "ok" })).collect();
        json!({ "role": "user", "content": blocks })
    }

    #[test]
    fn a_whole_conversation_is_kept_as_it_is() {
        let whole = [
            user("hi"),
            calls(&["c1", "c2"]),
            results(&["c1", "c2"]),
            says("done"),
            // Two from one side in a row are fine: the API joins them.
            user("more"),
            user("and this"),
            says("one"),
            calls(&["c3"]),
            results(&["c3"]),
            // Calls still open at the end are answered when the session is continued.
            calls(&["c4"]),
        ];
        let (kept, dropped) = replay(&whole);
        assert_eq!(kept, ["u:hi", "a:c1", "u:c1", "a:done", "u:more", "u:and this", "a:one", "a:c3", "u:c3", "a:c4"]);
        assert_eq!(dropped, 0);
        // Plain text as content, as some files have it.
        let (kept, dropped) = replay(&[json!({ "role": "user", "content": "plain" }), says("ok")]);
        assert_eq!((kept.len(), dropped), (2, 0));
    }

    #[test]
    fn what_a_missing_line_leaves_behind_is_left_out() {
        // The first message is gone: the conversation can't start with the assistant.
        assert_eq!(replay(&[says("hello"), user("hi"), says("yes")]), (vec!["u:hi".into(), "a:yes".into()], 1));
        // The results are gone: the message with the calls goes too. What
        // was said before it, and what the user said next, stay.
        let lost_results = [user("hi"), says("looking"), calls(&["c1"]), user("next"), says("ok")];
        assert_eq!(replay(&lost_results).0, ["u:hi", "a:looking", "u:next", "a:ok"]);
        assert_eq!(replay(&lost_results).1, 1);
        // The calls are gone: results for nothing are left out.
        let lost_calls = [user("hi"), results(&["ghost"]), says("done")];
        assert_eq!(replay(&lost_calls), (vec!["u:hi".into(), "a:done".into()], 1));
        // The results are gone and the model's next message follows directly.
        let then_more = [user("hi"), calls(&["c1"]), calls(&["c2"]), results(&["c2"]), says("ok")];
        assert_eq!(replay(&then_more), (vec!["u:hi".into(), "a:c2".into(), "u:c2".into(), "a:ok".into()], 1));
        // Results for only some of the calls: left out, and then the calls with them.
        let some = [user("hi"), calls(&["c1", "c2"]), results(&["c1"]), user("next"), says("ok")];
        assert_eq!(replay(&some), (vec!["u:hi".into(), "u:next".into(), "a:ok".into()], 2));
        // Results for other calls than the ones just made.
        let wrong = [user("hi"), calls(&["c1"]), results(&["c9"]), user("next"), says("ok")];
        assert_eq!(replay(&wrong).0, ["u:hi", "u:next", "a:ok"]);
        // Empty messages, and lines that are no message.
        let no_type = json!({ "role": "user", "content": [{ "text": "hi" }] });
        let blank = json!({ "role": "user", "content": [{ "type": "text", "text": " \n" }] });
        assert_eq!(replay(&[no_type, blank, user("hi"), says("yes")]), (vec!["u:hi".into(), "a:yes".into()], 2));
        // A blank text block beside something else: the message stays, to be sent without it.
        let beside = json!({ "role": "assistant", "content": [
            { "type": "text", "text": "" }, { "type": "tool_use", "id": "c1", "name": "bash", "input": {} }] });
        let seen = shape(&beside.to_string()).unwrap();
        assert!(seen.blank && seen.some && seen.calls == ["c1"]);
        let cleaned: Value = serde_json::from_str(&without_blank_text(&beside.to_string()).unwrap()).unwrap();
        assert_eq!(cleaned["content"].as_array().unwrap().len(), 1);
        assert_eq!((cleaned["content"][0]["id"].as_str(), cleaned["role"].as_str()), (Some("c1"), Some("assistant")));
        let odd = [
            json!({ "role": "user", "content": [] }),
            json!({ "role": "user", "content": "" }),
            json!([1, 2]),
            json!({ "role": 5 }),
            json!({ "role": "system", "content": "x" }),
            json!({ "role": "user", "content": [1] }),
            user("hi"),
            says("yes"),
        ];
        assert_eq!(replay(&odd), (vec!["u:hi".into(), "a:yes".into()], 6));
    }

    #[test]
    fn a_rewind_resets_what_is_open() {
        let mut order = Order::default();
        let mut kept = Vec::new();
        for line in [user("hi"), calls(&["c1"]), results(&["c1"]), calls(&["c2"])] {
            order.take(
                &mut kept,
                Msg::new(line["role"].as_str().unwrap(), line["content"].clone()),
                shape(&line.to_string()).unwrap(),
            );
        }
        // Back to before the second call's message: nothing is open, so a prompt just follows.
        kept.truncate(3);
        order.cut(&kept);
        let next = user("again");
        order.take(&mut kept, Msg::new("user", next["content"].clone()), shape(&next.to_string()).unwrap());
        assert_eq!((kept.len(), order.dropped), (4, 0));
        // Back to right after a call: it is open again, and its results fit.
        kept.truncate(2);
        order.cut(&kept);
        let answer = results(&["c1"]);
        order.take(&mut kept, Msg::new("user", answer["content"].clone()), shape(&answer.to_string()).unwrap());
        assert_eq!((kept.len(), order.dropped), (3, 0));
    }
}
