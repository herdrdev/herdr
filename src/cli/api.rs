const API_SCHEMA_JSON: &str = include_str!("../../docs/next/api/herdr-api.schema.json");

use crate::api::schema::{EmptyParams, Method, Request};

pub(super) fn run_api_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_api_help();
        return Ok(2);
    };

    match subcommand {
        "schema" => api_schema(&args[1..]),
        "snapshot" => api_snapshot(&args[1..]),
        "help" | "--help" | "-h" => {
            print_api_help();
            Ok(0)
        }
        _ => {
            print_api_help();
            Ok(2)
        }
    }
}

fn api_schema(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => {
            print!("{}", schema_summary_text()?);
        }
        [flag] if flag == "--json" => {
            print!("{API_SCHEMA_JSON}");
        }
        [flag, path] if flag == "--output" => {
            write_schema_file(std::path::Path::new(path))?;
            println!("esquema da API gravado em {path}");
        }
        [flag] if flag == "--output" => {
            eprintln!("falta o valor de --output");
            return Ok(2);
        }
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            print_api_schema_help();
        }
        [other] if other.starts_with('-') => {
            eprintln!("opção desconhecida: {other}");
            return Ok(2);
        }
        _ => {
            print_api_schema_help();
            return Ok(2);
        }
    }
    Ok(0)
}

fn api_snapshot(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("Uso: herdr api snapshot");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:api:snapshot".into(),
        method: Method::SessionSnapshot(EmptyParams::default()),
    })?)
}

fn write_schema_file(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, API_SCHEMA_JSON)
}

fn schema_summary_text() -> std::io::Result<String> {
    let value: serde_json::Value = serde_json::from_str(API_SCHEMA_JSON)?;
    let protocol = value
        .get("protocol")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| std::io::Error::other("o esquema da API não contém protocol"))?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| std::io::Error::other("o esquema da API não contém schema_version"))?;
    let mut schemas: Vec<&str> = value
        .get("schemas")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("o esquema da API não contém schemas"))?
        .keys()
        .map(String::as_str)
        .collect();
    schemas.sort();

    Ok(format!(
        "Esquema da API Herdr\nprotocol: {}\nschema_version: {}\nschemas: {}\n\nUse `herdr api schema --json` para exibir o esquema completo.\nUse `herdr api schema --output PATH` para gravá-lo em arquivo.\n",
        protocol,
        schema_version,
        schemas.join(", ")
    ))
}

fn print_api_help() {
    eprintln!("Comandos de herdr api:");
    eprintln!("  herdr api snapshot");
    eprintln!("  herdr api schema [--json | --output PATH]");
}

fn print_api_schema_help() {
    eprintln!("Uso: herdr api schema [--json | --output PATH]");
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema_summary_text_stays_human_sized() {
        let text = super::schema_summary_text().unwrap();
        assert!(text.contains("Esquema da API Herdr"));
        assert!(text.contains("Use `herdr api schema --json`"));
        assert!(text.len() < 400);
    }
}
