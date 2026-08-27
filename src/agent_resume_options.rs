pub(crate) fn filter(args: &[String], flags: &[String], options: &[String]) -> Vec<String> {
    let mut filtered = Vec::new();
    let mut index = 0;

    while let Some(arg) = args.get(index) {
        if arg == "--" {
            break;
        }
        if flags.iter().any(|flag| flag == arg) {
            filtered.push(arg.clone());
            index += 1;
            continue;
        }
        if let Some((name, value)) = arg.split_once('=') {
            if options.iter().any(|option| option == name) && !value.is_empty() {
                filtered.push(arg.clone());
            }
            index += 1;
            continue;
        }
        if options.iter().any(|option| option == arg) {
            let Some(value) = args.get(index + 1) else {
                break;
            };
            if value == "--" || value.starts_with('-') {
                index += 1;
                continue;
            }
            filtered.push(arg.clone());
            filtered.push(value.clone());
            index += 2;
            continue;
        }
        index += 1;
    }

    filtered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn resume_options_filter_keeps_only_declared_flags_and_option_values() {
        let args = strings(&[
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "--unknown",
            "unknown-value",
            "--permission-mode=bypassPermissions",
            "fix the bug",
            "--model",
            "sonnet",
        ]);

        assert_eq!(
            filter(
                &args,
                &strings(&["--dangerously-skip-permissions"]),
                &strings(&["--model", "--permission-mode"]),
            ),
            strings(&[
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "--permission-mode=bypassPermissions",
                "--model",
                "sonnet",
            ])
        );
    }

    #[test]
    fn resume_options_filter_drops_missing_values_and_stops_at_argument_separator() {
        let args = strings(&["--model", "--", "--dangerously-skip-permissions", "prompt"]);

        assert!(filter(
            &args,
            &strings(&["--dangerously-skip-permissions"]),
            &strings(&["--model"]),
        )
        .is_empty());
    }
}
