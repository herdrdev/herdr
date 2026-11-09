//! Pattern listing, showing, new, validation, editing, and deletion CLI commands.

use crate::{config::load_config, shepherd::ShepherdClient};
use serde_json::Value;
use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;

pub(crate) fn patterns_list_command(
    root: PathBuf,
    _args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(client) = server {
        let data = client.shepherd_call(
            "patterns",
            serde_json::json!({
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_patterns_from_value(writer, &data)?;
        }
        return Ok(());
    }
    let list = crate::patterns::list_patterns(&root, None)?;
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&list)?)?;
    } else {
        print_human_patterns_list(writer, &list)?;
    }
    Ok(())
}

fn print_human_patterns_list(
    writer: &mut dyn std::io::Write,
    list: &[crate::patterns::PatternSummary],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if list.is_empty() {
        writeln!(writer, "no patterns found")?;
        return Ok(());
    }
    writeln!(
        writer,
        "NAME\tKIND\tSOURCE\tSHADOWED\tAGENTS\tTOPICS\tDESCRIPTION"
    )?;
    for p in list {
        let shadowed_str = if p.shadowed { "yes" } else { "no" };
        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            p.name, p.kind, p.source, shadowed_str, p.agents_count, p.topics_count, p.description
        )?;
    }
    Ok(())
}

fn print_human_patterns_from_value(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let patterns = data.as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    if patterns.is_empty() {
        writeln!(writer, "no patterns found")?;
        return Ok(());
    }
    writeln!(
        writer,
        "NAME\tKIND\tSOURCE\tSHADOWED\tAGENTS\tTOPICS\tDESCRIPTION"
    )?;
    for p in patterns {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("-");
        let kind = p.get("kind").and_then(Value::as_str).unwrap_or("-");
        let source = p.get("source").and_then(Value::as_str).unwrap_or("-");
        let shadowed = p
            .get("shadowed")
            .and_then(Value::as_bool)
            .map(|b| if b { "yes" } else { "no" })
            .unwrap_or("no");
        let agents = p.get("agents_count").and_then(Value::as_u64).unwrap_or(0);
        let topics = p.get("topics_count").and_then(Value::as_u64).unwrap_or(0);
        let desc = p.get("description").and_then(Value::as_str).unwrap_or("");
        writeln!(
            writer,
            "{name}\t{kind}\t{source}\t{shadowed}\t{agents}\t{topics}\t{desc}"
        )?;
    }
    Ok(())
}

pub(crate) fn pattern_show_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = args.first().ok_or("missing pattern name for 'show'")?;
    if let Some(client) = server {
        let data = client.shepherd_call(
            "pattern_get",
            serde_json::json!({
                "name": name,
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            print_human_pattern_detail(writer, &data)?;
        }
        return Ok(());
    }
    let (pattern, source, path) = crate::patterns::load_pattern(name, &root, None)?;
    let config = load_config(&root).unwrap_or_default();
    let problems = crate::patterns::validate(&pattern, &config);
    let detail = crate::patterns::PatternDetail {
        pattern,
        source,
        path: path.map(|p| p.display().to_string()),
        problems,
    };
    if json {
        writeln!(writer, "{}", serde_json::to_string_pretty(&detail)?)?;
    } else {
        let val = serde_json::to_value(&detail)?;
        print_human_pattern_detail(writer, &val)?;
    }
    Ok(())
}

fn print_human_pattern_detail(
    writer: &mut dyn std::io::Write,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = data.get("name").and_then(Value::as_str).unwrap_or("-");
    let kind = data.get("kind").and_then(Value::as_str).unwrap_or("-");
    let source = data.get("source").and_then(Value::as_str).unwrap_or("-");
    let path = data
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("(built-in template)");
    let desc = data
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");

    writeln!(writer, "Pattern: {name} ({kind})")?;
    writeln!(writer, "Source:  {source} ({path})")?;
    writeln!(writer, "Purpose: {desc}")?;
    writeln!(writer)?;

    if let Some(topics) = data.get("topics").and_then(Value::as_array) {
        writeln!(writer, "Topics ({}):", topics.len())?;
        for t in topics {
            let tname = t.get("name").and_then(Value::as_str).unwrap_or("-");
            let partitions = t.get("partitions").and_then(Value::as_u64).unwrap_or(1);
            let mut meta = vec![format!("partitions: {partitions}")];
            if let Some(k) = t.get("key").and_then(Value::as_str) {
                meta.push(format!("key: {k}"));
            }
            if let Some(f) = t.get("filter").and_then(Value::as_str) {
                meta.push(format!("filter: {f}"));
            }
            writeln!(writer, "  • {tname:<20} ({})", meta.join(", "))?;
        }
        writeln!(writer)?;
    }

    if let Some(agents) = data.get("agents").and_then(Value::as_array) {
        writeln!(writer, "Agents ({}):", agents.len())?;
        for a in agents {
            let aid = a.get("id").and_then(Value::as_str).unwrap_or("-");
            let role = a.get("role").and_then(Value::as_str).unwrap_or("-");
            let count = a.get("count").and_then(Value::as_u64).unwrap_or(1);
            let mut badges = vec![format!("role: {role}"), format!("count: {count}")];
            if let Some(grp) = a.get("group").and_then(Value::as_str) {
                badges.push(format!("group: {grp}"));
            }
            if let Some(lvl) = a.get("level").and_then(Value::as_u64) {
                badges.push(format!("level: {lvl}"));
            }
            if let Some(vendor) = a.get("vendor").and_then(Value::as_str) {
                badges.push(format!("vendor: {vendor}"));
            }
            if let Some(model) = a.get("model").and_then(Value::as_str) {
                badges.push(format!("model: {model}"));
            }
            writeln!(writer, "  • {aid:<20} [{}]", badges.join(" | "))?;

            let pub_list: Vec<&str> = a
                .get("publishes")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let con_list: Vec<&str> = a
                .get("consumes")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();

            if !pub_list.is_empty() {
                writeln!(writer, "      publishes -> {}", pub_list.join(", "))?;
            }
            if !con_list.is_empty() {
                writeln!(writer, "      consumes  <- {}", con_list.join(", "))?;
            }
            if let Some(prompt) = a.get("prompt").and_then(Value::as_str) {
                let preview = if prompt.len() > 120 {
                    format!("{}...", &prompt[..117])
                } else {
                    prompt.to_string()
                };
                writeln!(writer, "      persona: \"{preview}\"")?;
            }
        }
        writeln!(writer)?;
    }

    if let Some(problems) = data.get("problems").and_then(Value::as_array) {
        if !problems.is_empty() {
            writeln!(writer, "Validation Diagnostics:")?;
            for prob in problems {
                let sev = prob
                    .get("severity")
                    .and_then(Value::as_str)
                    .unwrap_or("warning");
                let path = prob.get("path").and_then(Value::as_str).unwrap_or("-");
                let msg = prob.get("message").and_then(Value::as_str).unwrap_or("");
                writeln!(writer, "  [{sev}] {path}: {msg}")?;
            }
        }
    }
    Ok(())
}

pub fn pattern_new_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut name = None;
    let mut from = None;
    let mut is_global = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" | "-f" => {
                i += 1;
                if i < args.len() {
                    from = Some(&args[i]);
                }
            }
            "--global" | "-g" => {
                is_global = true;
            }
            other if !other.starts_with('-') && name.is_none() => {
                name = Some(other);
            }
            _ => {}
        }
        i += 1;
    }

    let name = name.ok_or("missing pattern name for 'new'")?;
    let from = from.ok_or("missing '--from <template|name>' for 'new'")?;

    if let Some(client) = server {
        let get_data = client.shepherd_call(
            "pattern_get",
            serde_json::json!({
                "name": from,
                "root": root.display().to_string(),
            }),
        )?;
        let mut pattern_val = get_data;
        pattern_val["name"] = serde_json::json!(name);
        let save_res = client.shepherd_call(
            "pattern_save",
            serde_json::json!({
                "pattern": pattern_val,
                "global": is_global,
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&save_res)?)?;
        } else {
            let path_str = save_res.get("path").and_then(Value::as_str).unwrap_or("");
            writeln!(writer, "created pattern '{name}' at {path_str}")?;
        }
        return Ok(());
    }

    let (pattern, path) = crate::patterns::duplicate_pattern(name, from, is_global, &root, None)?;
    let config = load_config(&root).unwrap_or_default();
    let problems = crate::patterns::validate(&pattern, &config);
    if json {
        let out = serde_json::json!({
            "ok": true,
            "name": name,
            "path": path.display().to_string(),
            "source": if is_global { "global" } else { "project" },
            "problems": problems,
        });
        writeln!(writer, "{}", serde_json::to_string_pretty(&out)?)?;
    } else {
        writeln!(writer, "created pattern '{name}' at {}", path.display())?;
    }
    Ok(())
}

pub(crate) fn pattern_validate_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut file_path = None;
    let mut name = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                i += 1;
                if i < args.len() {
                    file_path = Some(PathBuf::from(&args[i]));
                }
            }
            other if !other.starts_with('-') && name.is_none() => {
                name = Some(other);
            }
            _ => {}
        }
        i += 1;
    }

    if file_path.is_none() && name.is_none() {
        return Err("missing pattern name or '--file <path>' to validate".into());
    }

    let (pattern, pattern_label) = if let Some(path) = file_path {
        let pat = crate::patterns::load_pattern_from_file(&path)?;
        let label = path.display().to_string();
        (pat, label)
    } else {
        let n = name.unwrap();
        if let Some(client) = server {
            let res = client.shepherd_call(
                "pattern_validate",
                serde_json::json!({
                    "name": n,
                    "root": root.display().to_string(),
                }),
            )?;
            if json {
                writeln!(writer, "{}", serde_json::to_string_pretty(&res)?)?;
                return Ok(());
            }
            let valid = res.get("valid").and_then(Value::as_bool).unwrap_or(false);
            print_human_validation_problems(writer, n, &res)?;
            if !valid {
                return Err("pattern validation failed".into());
            }
            return Ok(());
        }
        let (pat, _, _) = crate::patterns::load_pattern(n, &root, None)?;
        (pat, n.to_string())
    };

    let config = load_config(&root).unwrap_or_default();
    let problems = crate::patterns::validate(&pattern, &config);
    let valid = !problems
        .iter()
        .any(|p| p.severity == crate::patterns::Severity::Error);

    if json {
        let out = serde_json::json!({
            "valid": valid,
            "problems": problems,
        });
        writeln!(writer, "{}", serde_json::to_string_pretty(&out)?)?;
        return Ok(());
    }

    let val = serde_json::json!({ "valid": valid, "problems": problems });
    print_human_validation_problems(writer, &pattern_label, &val)?;

    if !valid {
        return Err("pattern validation failed".into());
    }
    Ok(())
}

fn print_human_validation_problems(
    writer: &mut dyn std::io::Write,
    label: &str,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let valid = data.get("valid").and_then(Value::as_bool).unwrap_or(false);
    let problems = data
        .get("problems")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    if valid && problems.is_empty() {
        writeln!(
            writer,
            "OK: pattern '{label}' is valid (0 errors, 0 warnings)"
        )?;
        return Ok(());
    }

    if valid {
        writeln!(writer, "OK: pattern '{label}' is valid (with warnings):")?;
    } else {
        writeln!(writer, "FAILED: pattern '{label}' has validation errors:")?;
    }

    for p in problems {
        let sev = p
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("warning");
        let path = p.get("path").and_then(Value::as_str).unwrap_or("-");
        let msg = p.get("message").and_then(Value::as_str).unwrap_or("");
        writeln!(writer, "  [{sev}] {path}: {msg}")?;
    }
    Ok(())
}

pub(crate) fn pattern_delete_command(
    root: PathBuf,
    args: Vec<String>,
    server: Option<&ShepherdClient>,
    json: bool,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = args.first().ok_or("missing pattern name for 'delete'")?;

    if let Some(client) = server {
        let data = client.shepherd_call(
            "pattern_delete",
            serde_json::json!({
                "name": name,
                "root": root.display().to_string(),
            }),
        )?;
        if json {
            writeln!(writer, "{}", serde_json::to_string_pretty(&data)?)?;
        } else {
            let src = data
                .get("deleted_source")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            writeln!(writer, "deleted pattern '{name}' from {src}")?;
        }
        return Ok(());
    }

    let source = crate::patterns::delete_pattern(name, &root, None)?;
    if json {
        let out = serde_json::json!({
            "ok": true,
            "name": name,
            "deleted_source": source.as_str(),
        });
        writeln!(writer, "{}", serde_json::to_string_pretty(&out)?)?;
    } else {
        writeln!(writer, "deleted pattern '{name}' from {source}")?;
    }
    Ok(())
}

pub fn pattern_edit_command(
    root: PathBuf,
    args: Vec<String>,
    writer: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = args.first().ok_or("missing pattern name for 'edit'")?;
    let (_, _source, path) = crate::patterns::load_pattern(name, &root, None)?;
    let Some(path) = path else {
        return Err(format!(
            "cannot edit built-in pattern template '{name}' in place; duplicate it first with `shepherd pattern new <name> --from {name}`"
        )
        .into());
    };
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        writeln!(writer, "{}", path.display())?;
        return Ok(());
    }
    let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(editor).arg(&path).status()?;
    if !status.success() {
        return Err(format!("editor exited with {status}").into());
    }
    Ok(())
}
