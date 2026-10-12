use std::io::{self, Read};

pub(super) fn run_focus_command(args: &[String]) -> io::Result<i32> {
    if args
        .first()
        .is_none_or(|arg| arg == "--help" || arg == "-h")
    {
        println!("usage: herdr focus add <text…> [--research] [--json]");
        return Ok(if !args.is_empty() { 0 } else { 2 });
    }
    if args.first().is_some_and(|arg| arg == "add") {
        return add(&args[1..]);
    }
    eprintln!("usage: herdr focus add <text…> [--research] [--json]");
    Ok(2)
}

fn parse_add(args: &[String]) -> Result<(String, bool, bool), String> {
    let mut research = false;
    let mut json = false;
    let mut words = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--research" => research = true,
            "--json" => json = true,
            value if value.starts_with('-') && value != "-" => {
                return Err(format!("unknown option: {value}"))
            }
            value => words.push(value.to_string()),
        }
    }
    if words.is_empty() {
        return Err("missing capture text".into());
    }
    Ok((words.join(" "), research, json))
}

fn read_capture(text: &str, mut stdin: impl Read) -> io::Result<String> {
    if text == "-" {
        let mut capture = String::new();
        stdin.read_to_string(&mut capture)?;
        Ok(capture)
    } else {
        Ok(text.to_string())
    }
}

fn add(args: &[String]) -> io::Result<i32> {
    let (text, research, json) = match parse_add(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let text = read_capture(&text, io::stdin().lock())?;
    let config = crate::config::Config::load().config;
    let dir = crate::worktree::expand_tilde_path(&config.focus.dir);
    match save_at(&dir, &text, research) {
        Ok(item) if json => println!("{}", json_line(&item)),
        Ok(item) => println!("{}", item.path.display()),
        Err((code, message)) => {
            eprintln!("{message}");
            return Ok(code);
        }
    }
    Ok(0)
}

fn save_at(
    dir: &std::path::Path,
    text: &str,
    research: bool,
) -> Result<crate::focus_capture::SavedItem, (i32, String)> {
    crate::focus_capture::add_item(dir, text, research).map_err(|error| match error {
        crate::focus_capture::AddError::Empty => (1, "capture is empty".into()),
        crate::focus_capture::AddError::Io(error) => {
            (1, format!("could not save focus item: {error}"))
        }
    })
}
fn json_line(item: &crate::focus_capture::SavedItem) -> String {
    serde_json::json!({"path": item.path, "slug": item.slug, "order": item.order}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn joins_args_and_reads_stdin_marker() {
        let (text, research, json) =
            parse_add(&["a".into(), "b".into(), "--research".into(), "--json".into()]).unwrap();
        assert_eq!(text, "a b");
        assert!(research && json);
        assert_eq!(
            read_capture("-", Cursor::new("from stdin")).unwrap(),
            "from stdin"
        );
    }

    #[test]
    fn missing_text_is_usage_error() {
        assert!(parse_add(&[]).is_err());
        assert_eq!(run_focus_command(&["add".into()]).unwrap(), 2);
    }

    #[test]
    fn json_shape_and_write_failure_status() {
        let root = std::env::temp_dir().join(format!("focus-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let saved = save_at(&root, "Read paper", true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_line(&saved)).unwrap();
        assert_eq!(
            value
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["order", "path", "slug"]
        );
        assert_eq!(value["path"], saved.path.to_string_lossy().as_ref());
        let file = root.join("not-a-directory");
        std::fs::write(&file, "file").unwrap();
        assert_eq!(save_at(&file, "fails", false).unwrap_err().0, 1);
        assert_eq!(save_at(&root, " \n ", false).unwrap_err().0, 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
