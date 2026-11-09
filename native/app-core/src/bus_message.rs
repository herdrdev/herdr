//! How a bus message reads in the monitor.

use shepherd_core::bus::Message;

/// `(header, body)`: who said it to whom (or on which topic) and when, and the text (trimmed for the screen).
pub fn line(message: &Message) -> (String, String) {
    let to = if !message.to.is_empty() {
        format!(" → {}", message.to)
    } else if !message.topic.is_empty() {
        format!(" → #{}", message.topic)
    } else {
        String::new()
    };
    let time = message
        .ts
        .replace('T', " ")
        .chars()
        .skip(11)
        .take(8)
        .collect::<String>();
    let body = if message.content.chars().count() > 600 {
        format!("{}…", message.content.chars().take(600).collect::<String>())
    } else {
        message.content.clone()
    };
    (
        format!("{time}  {}{to}  {}", message.from, message.kind),
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_reads_as_who_to_whom() {
        let mut m = Message::directed("brain-1", "coder-2", "msg", "haz esto");
        m.ts = "2026-11-07T10:11:12Z".into();
        assert_eq!(
            line(&m),
            (
                "10:11:12  brain-1 → coder-2  msg".to_owned(),
                "haz esto".to_owned()
            )
        );
        let mut t = Message::directed("a-1", "", "msg", "x");
        t.to = String::new();
        t.topic = "orders".into();
        t.ts = "2026-11-07T10:11:12Z".into();
        assert!(line(&t).0.contains("→ #orders"));
    }
}
