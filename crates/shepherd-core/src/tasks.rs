//! Task creation, persistence, directory layout, and listing.

use crate::{CreatedTask, NewTask, TaskDetailJson, TaskListEntryJson, TaskSectionJson, TaskStatus};
use std::path::Path;
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

/// Read a task status JSON file produced by either the Go or Rust implementation.
pub fn load_task_status(
    path: impl AsRef<Path>,
) -> Result<TaskStatus, Box<dyn std::error::Error + Send + Sync>> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Atomically write task status using the same temp-file-and-rename strategy as Go.
pub fn save_task_status(
    path: impl AsRef<Path>,
    status: &TaskStatus,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = path.as_ref();
    let mut bytes = serde_json::to_vec_pretty(status)?;
    bytes.push(b'\n');
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

pub fn touch_task(task: &CreatedTask) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut status = task.status.clone();
    status.updated = now_rfc3339();
    save_task_status(task.directory.join("status.json"), &status)
}

pub fn list_tasks(
    root: impl AsRef<Path>,
    archived: bool,
) -> Result<Vec<CreatedTask>, Box<dyn std::error::Error + Send + Sync>> {
    let mut directory = root.as_ref().join(".shepherd").join("tasks");
    if archived {
        directory.push("_archive");
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with('_') {
            continue;
        }
        let path = entry.path();
        if let Ok(status) = load_task_status(path.join("status.json")) {
            tasks.push(CreatedTask {
                status,
                directory: path,
            });
        }
    }
    tasks.sort_by_key(|task| task.status.id);
    Ok(tasks)
}

pub fn find_task(
    root: impl AsRef<Path>,
    reference: &str,
) -> Result<CreatedTask, Box<dyn std::error::Error + Send + Sync>> {
    let tasks = list_tasks(root, false)?;
    let mut prefix_matches = Vec::new();
    for task in tasks {
        let name = format!("{}-{}", task.status.id, task.status.slug);
        if task.status.id.to_string() == reference || name == reference {
            return Ok(task);
        }
        if name.starts_with(reference) {
            prefix_matches.push(task);
        }
    }
    match prefix_matches.len() {
        1 => Ok(prefix_matches.remove(0)),
        0 => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no task {reference:?}"),
        )
        .into()),
        n => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("task {reference:?} is ambiguous ({n} matches)"),
        )
        .into()),
    }
}

const TASK_SECTIONS: [&str; 5] = ["issue", "spec", "acceptance", "links", "plan"];

pub fn task_section_path(
    task: &CreatedTask,
    section: &str,
) -> Result<std::path::PathBuf, std::io::Error> {
    if !TASK_SECTIONS.contains(&section) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "unknown section {section:?} (have: {})",
                TASK_SECTIONS.join(", ")
            ),
        ));
    }
    Ok(task.directory.join(format!("{section}.md")))
}

pub fn read_task_section(
    task: &CreatedTask,
    section: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let path = task_section_path(task, section)?;
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn write_task_section(
    task: &CreatedTask,
    section: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = task_section_path(task, section)?;
    atomic_write(&path, content.as_bytes())?;
    Ok(())
}

pub fn task_context(task: &CreatedTask, limit_bytes: usize) -> String {
    let mut full = format!("# Task {}-{}\n\n", task.status.id, task.status.slug);
    let mut index = format!(
        "# Task {}-{}\n\nRead these files (or call get_task_context(section)):\n\n",
        task.status.id, task.status.slug
    );
    for section in TASK_SECTIONS {
        let Ok(content) = read_task_section(task, section) else {
            continue;
        };
        if content.trim().is_empty() {
            continue;
        }
        full.push_str(content.trim());
        full.push_str("\n\n");
        if let Ok(path) = task_section_path(task, section) {
            index.push_str(&format!(
                "- {section} ({} bytes): {}\n",
                content.len(),
                path.display()
            ));
        }
    }
    index.push_str(&format!(
        "- attachments: {}\n",
        task.directory.join("attachments").display()
    ));
    if limit_bytes > 0 && full.len() > limit_bytes {
        index
    } else {
        full
    }
}

pub fn archive_task(
    root: impl AsRef<Path>,
    reference: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let task = find_task(root.as_ref(), reference)?;
    let archive_dir = root
        .as_ref()
        .join(".shepherd")
        .join("tasks")
        .join("_archive");
    std::fs::create_dir_all(&archive_dir)?;
    let destination = archive_dir.join(task.directory.file_name().unwrap_or_default());
    if destination.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", destination.display()),
        )
        .into());
    }
    std::fs::rename(task.directory, &destination)?;
    Ok(destination)
}

pub fn task_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for ch in title.nfd().flat_map(char::to_lowercase) {
        if is_combining_mark(ch) {
            continue;
        }
        if ch.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(ch);
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.len() > 40 {
        slug.truncate(40);
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "task".into()
    } else {
        slug
    }
}

pub fn acceptance_markdown(criteria: &[String]) -> String {
    let mut output = String::from("# Acceptance criteria\n\n");
    let mut count = 0;
    for criterion in criteria.iter().map(|c| c.trim()).filter(|c| !c.is_empty()) {
        output.push_str("- [ ] ");
        output.push_str(criterion);
        output.push('\n');
        count += 1;
    }
    if count == 0 {
        output.push_str("- [ ] (add checkable criteria; the reviewer verifies each one)\n");
    }
    output
}

/// Create a task directory and files using the same layout as Go's task.Create.
pub fn create_task(
    root: impl AsRef<Path>,
    mut input: NewTask,
) -> Result<CreatedTask, Box<dyn std::error::Error + Send + Sync>> {
    if input.title.trim().is_empty() {
        return Err(
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "a task needs a title").into(),
        );
    }
    let tasks_dir = root.as_ref().join(".shepherd").join("tasks");
    if input.id == 0 {
        input.id = next_task_id(&tasks_dir);
    }
    if let Some(existing) = list_tasks(root.as_ref(), false)?
        .into_iter()
        .find(|t| t.status.id == input.id)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "task {} already exists ({})",
                input.id,
                existing.directory.display()
            ),
        )
        .into());
    }
    let slug = task_slug(&input.title);
    let directory = tasks_dir.join(format!("{}-{}", input.id, slug));
    std::fs::create_dir_all(&tasks_dir)?;
    std::fs::create_dir(&directory)?;
    std::fs::create_dir(directory.join("attachments"))?;

    let issue = input
        .issue_markdown
        .unwrap_or_else(|| format!("# {}\n\n{}\n", input.title, input.description.trim()));
    let mut links = String::from("# Links\n\n");
    if let Some(issue_ref) = &input.issue {
        links.push_str(&format!("- Issue: {}\n", issue_ref.url));
    }
    for link in input.links {
        links.push_str(&format!("- {}\n", link));
    }
    for (name, content) in [
        ("issue.md", issue),
        ("spec.md", "# Spec\n\nDesign decisions, constraints, interfaces. Agents read this before coding.\n".to_owned()),
        ("acceptance.md", acceptance_markdown(&input.criteria)),
        ("links.md", links),
    ] {
        atomic_write(&directory.join(name), content.as_bytes())?;
    }

    let now = now_rfc3339();
    let status = TaskStatus {
        id: input.id,
        slug,
        title: input.title,
        state: "new".into(),
        issue: input.issue,
        branch: String::new(),
        pr: 0,
        subtasks: Vec::new(),
        waiting: String::new(),
        note: String::new(),
        agents: Vec::new(),
        created: now.clone(),
        updated: now,
    };
    save_task_status(directory.join("status.json"), &status)?;
    Ok(CreatedTask { status, directory })
}

fn next_task_id(tasks_dir: &Path) -> u32 {
    let mut max_id = 0;
    for dir in [tasks_dir.to_path_buf(), tasks_dir.join("_archive")] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Some((id, _)) = entry.file_name().to_str().and_then(parse_task_name) {
                max_id = max_id.max(id);
            }
        }
    }
    max_id + 1
}

fn parse_task_name(name: &str) -> Option<(u32, &str)> {
    let (number, slug) = name.split_once('-')?;
    if number.is_empty() || slug.is_empty() {
        return None;
    }
    Some((number.parse().ok()?, slug))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

pub(crate) fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86_400) as i64;
    let day_seconds = seconds % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = day_seconds / 3600;
    let minute = (day_seconds % 3600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

pub fn task_list_json(tasks: &[CreatedTask]) -> Result<String, serde_json::Error> {
    let entries: Vec<TaskListEntryJson> = tasks
        .iter()
        .map(|task| TaskListEntryJson {
            status: task.status.clone(),
            directory: task.directory.display().to_string(),
        })
        .collect();
    serde_json::to_string_pretty(&entries)
}

/// Serializes either a full task view (`section == None`) or a single task section (`section == Some(...)`) as JSON.
pub fn task_show_json(
    task: &CreatedTask,
    section: Option<&str>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(section_name) = section {
        let path = task_section_path(task, section_name)?;
        let content = read_task_section(task, section_name)?;
        let payload = TaskSectionJson {
            id: task.status.id,
            slug: task.status.slug.clone(),
            section: section_name.to_owned(),
            path: path.display().to_string(),
            content,
        };
        return Ok(serde_json::to_string_pretty(&payload)?);
    }
    let mut sections = std::collections::BTreeMap::new();
    for name in TASK_SECTIONS {
        if let Ok(path) = task_section_path(task, name) {
            if path.exists() {
                if let Ok(content) = read_task_section(task, name) {
                    sections.insert(name.to_owned(), content);
                }
            }
        }
    }
    let payload = TaskDetailJson {
        status: task.status.clone(),
        directory: task.directory.display().to_string(),
        sections,
        context: task_context(task, 0),
    };
    Ok(serde_json::to_string_pretty(&payload)?)
}
