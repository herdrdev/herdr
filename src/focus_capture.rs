//! Local, atomic Markdown storage for captured focus items.
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_DIR: &str = "~/workspaces/obsidian/focus";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemHeader {
    pub fields: Vec<(String, String)>,
}

impl ItemHeader {
    pub(crate) fn order(&self) -> Option<u64> {
        self.fields
            .iter()
            .find(|(key, _)| key == "order")
            .and_then(|(_, value)| value.split('#').next()?.trim().parse().ok())
    }

    pub(crate) fn sort_key(&self) -> (bool, u64) {
        self.order()
            .map_or((true, u64::MAX), |order| (false, order))
    }
}

pub(crate) fn parse_header(raw: &str) -> Option<ItemHeader> {
    let raw = raw.replace("\r\n", "\n").replace('\r', "\n");
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let fields = rest[..end]
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let value = value.trim().to_string();
            let value = unquote_yaml_string(&value).unwrap_or(value);
            Some((key.trim().to_string(), value))
        })
        .collect();
    Some(ItemHeader { fields })
}

fn unquote_yaml_string(value: &str) -> Option<String> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut unescaped = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => unescaped.push('\\'),
                Some('"') => unescaped.push('"'),
                Some(other) => {
                    unescaped.push('\\');
                    unescaped.push(other);
                }
                None => unescaped.push('\\'),
            }
        } else {
            unescaped.push(c);
        }
    }
    Some(unescaped)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedItem {
    pub path: PathBuf,
    pub slug: String,
    pub order: u64,
    pub title: String,
    pub research: bool,
}

#[derive(Debug)]
pub(crate) enum AddError {
    Empty,
    Io(std::io::Error),
}

pub(crate) fn split_capture(text: &str) -> (String, String) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim();
    let first_line_end = trimmed.find('\n').unwrap_or(trimmed.len());
    let sentence_end = trimmed
        .char_indices()
        .find_map(|(i, c)| {
            matches!(c, '.' | '?' | '!')
                .then(|| {
                    let end = i + c.len_utf8();
                    (trimmed[end..]
                        .chars()
                        .next()
                        .is_none_or(char::is_whitespace))
                    .then_some(end)
                })
                .flatten()
        })
        .unwrap_or(trimmed.len());
    let title_end = sentence_end.min(first_line_end);
    let raw_title = trimmed[..title_end].trim();
    let title = raw_title.strip_suffix('.').unwrap_or(raw_title);
    let title: String = title
        .chars()
        .take(80)
        .collect::<String>()
        .trim_end()
        .to_string();
    let body = if raw_title.chars().count() > 80 {
        trimmed.to_string()
    } else {
        trimmed[title_end..].trim().to_string()
    };
    (title, body)
}

pub(crate) fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            if slug.len() < 60 {
                slug.push(c);
            }
        } else {
            separator = true;
        }
        if slug.len() >= 60 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "item".into()
    } else {
        slug
    }
}

fn next_order(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            (!name.starts_with('.') && entry.path().extension().is_some_and(|ext| ext == "md"))
                .then(|| std::fs::read_to_string(entry.path()).ok())
                .flatten()
                .and_then(|raw| parse_header(&raw).map(|header| header.sort_key()))
        })
        .filter_map(|(missing, order)| (!missing).then_some(order))
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

pub(crate) fn add_item(dir: &Path, text: &str, research: bool) -> Result<SavedItem, AddError> {
    if text.trim().is_empty() {
        return Err(AddError::Empty);
    }
    let (title, body) = split_capture(text);
    let base = slugify(&title);
    let order = next_order(dir);
    crate::platform::create_private_dir_all(dir).map_err(AddError::Io)?;
    let now = crate::platform::local_datetime().unwrap_or_else(|| {
        let dt = time::OffsetDateTime::now_utc();
        time::PrimitiveDateTime::new(dt.date(), dt.time())
    });
    let format =
        time::format_description::parse_borrowed::<2>("[year]-[month]-[day]T[hour]:[minute]")
            .expect("constant datetime format");
    let created = now
        .format(&format)
        .unwrap_or_else(|_| "1970-01-01T00:00".into());
    let content = markdown_item(&title, &body, order, research, &created);
    let mut temp = None;
    for attempt in 0..16 {
        let path = dir.join(format!(".focus-{}-{attempt}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let written = (|| -> std::io::Result<()> {
                    use std::io::Write;
                    file.write_all(content.as_bytes())?;
                    file.sync_all()
                })();
                if let Err(error) = written {
                    let _ = std::fs::remove_file(&path);
                    return Err(AddError::Io(error));
                }
                temp = Some(path);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(AddError::Io(e)),
        }
    }
    let temp = temp.ok_or_else(|| {
        AddError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not reserve temporary file",
        ))
    })?;
    let result = (1..=u64::MAX)
        .find_map(|suffix| {
            let slug = if suffix == 1 {
                base.clone()
            } else {
                let tail = format!("-{suffix}");
                let mut prefix: String = base
                    .chars()
                    .take(60_usize.saturating_sub(tail.chars().count()))
                    .collect();
                while prefix.ends_with('-') {
                    prefix.pop();
                }
                format!("{prefix}{tail}")
            };
            let path = dir.join(format!("{slug}.md"));
            match std::fs::hard_link(&temp, &path) {
                Ok(()) => Some(Ok((slug, path))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(e) => Some(Err(e)),
            }
        })
        .unwrap_or_else(|| {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "no available item slug",
            ))
        });
    let cleanup = std::fs::remove_file(&temp);
    let (slug, path) = result.map_err(AddError::Io)?;
    cleanup.map_err(AddError::Io)?;
    let absolute = std::fs::canonicalize(&path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir().unwrap_or_default().join(&path)
        }
    });
    Ok(SavedItem {
        path: absolute,
        slug,
        order,
        title,
        research,
    })
}

fn markdown_item(title: &str, body: &str, order: u64, research: bool, created: &str) -> String {
    let tags = if research { "[research]" } else { "[]" };
    let mut escaped_title = String::with_capacity(title.len());
    for c in title.chars().filter(|c| !c.is_control()) {
        if matches!(c, '\\' | '"') {
            escaped_title.push('\\');
        }
        escaped_title.push(c);
    }
    format!("---\ntitle: \"{escaped_title}\"\ntags: {tags}\nstage: dumped\norder: {order}\nresearch: {research}\ncreated: {created}\n---\n\n# {title}{}", if body.is_empty() { String::new() } else { format!("\n\n{body}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "herdr-focus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn title_body_slug_and_header_are_tolerant() {
        assert_eq!(
            split_capture("Hello. More here"),
            ("Hello".into(), "More here".into())
        );
        assert_eq!(
            split_capture("Really? More"),
            ("Really?".into(), "More".into())
        );
        assert_eq!(split_capture("Wait! More"), ("Wait!".into(), "More".into()));
        assert_eq!(
            split_capture("colon: kept. body"),
            ("colon: kept".into(), "body".into())
        );
        assert_eq!(
            split_capture("First line\nsecond"),
            ("First line".into(), "second".into())
        );
        assert_eq!(
            split_capture("first\r\nsecond\rthird"),
            ("first".into(), "second\nthird".into())
        );
        let no_space_capture = format!("{}. tail", "a".repeat(90));
        let (title, body) = split_capture(&no_space_capture);
        assert_eq!(title, "a".repeat(80));
        assert_eq!(body, no_space_capture);
        let space_at_cut_capture = format!("{} tail", "a".repeat(79));
        let (title, body) = split_capture(&space_at_cut_capture);
        assert_eq!(title, "a".repeat(79));
        assert_eq!(body, space_at_cut_capture);
        assert_eq!(slugify(" Héllo / World! "), "h-llo-world");
        assert_eq!(slugify("!!!"), "item");
        assert_eq!(slugify(&"a".repeat(80)).len(), 60);
        let h = parse_header("---\nunknown: kept\norder: 8 # comment\n---\n").unwrap();
        assert_eq!(h.order(), Some(8));
        assert_eq!(h.fields[0], ("unknown".into(), "kept".into()));
        assert_eq!(h.sort_key(), (false, 8));
        assert_eq!(
            parse_header("---\ntitle: no order\n---")
                .unwrap()
                .sort_key(),
            (true, u64::MAX)
        );
    }

    #[test]
    fn markdown_bytes_are_exact_for_plain_and_research_items() {
        assert_eq!(markdown_item("Plan", "body", 7, false, "2026-10-11T19:02"), "---\ntitle: \"Plan\"\ntags: []\nstage: dumped\norder: 7\nresearch: false\ncreated: 2026-10-11T19:02\n---\n\n# Plan\n\nbody");
        assert_eq!(markdown_item("Read", "", 1, true, "2026-10-11T19:02"), "---\ntitle: \"Read\"\ntags: [research]\nstage: dumped\norder: 1\nresearch: true\ncreated: 2026-10-11T19:02\n---\n\n# Read");
    }

    #[test]
    fn markdown_titles_are_quoted_and_round_trip() {
        let titles = [
            "herdr sidebar: break and done state broken",
            "fix #42 now",
            "say \"hi\" \\ bye",
            "- dash first",
            "[bracket] first",
            "&anchor first",
        ];
        for title in titles {
            let raw = markdown_item(title, "", 1, false, "2026-10-11T19:02");
            let frontmatter_title = format!(
                "title: \"{}\"",
                title.replace('\\', "\\\\").replace('"', "\\\"")
            );
            assert_eq!(raw.lines().nth(1), Some(frontmatter_title.as_str()));
            assert_eq!(
                parse_header(&raw).unwrap().fields[0],
                ("title".into(), title.into())
            );
        }
    }

    #[test]
    fn atomic_add_collision_order_research_and_private_directory() {
        let root = temp();
        let dir = root.join("new");
        let empty_dir = root.join("empty");
        assert!(matches!(
            add_item(&empty_dir, " \n ", false),
            Err(AddError::Empty)
        ));
        assert!(!empty_dir.exists());
        let first = add_item(&dir, "Do this. Body", false).unwrap();
        let second = add_item(&dir, "Do this. Again", true).unwrap();
        let third = add_item(&dir, "Do this. Third", false).unwrap();
        assert_eq!(
            (
                first.slug.as_str(),
                second.slug.as_str(),
                third.slug.as_str()
            ),
            ("do-this", "do-this-2", "do-this-3")
        );
        assert_eq!((first.order, second.order, third.order), (1, 2, 3));
        assert!(!first.research && second.research);
        let raw = std::fs::read_to_string(first.path).unwrap();
        assert!(raw.starts_with(
            "---\ntitle: \"Do this\"\ntags: []\nstage: dumped\norder: 1\nresearch: false\ncreated: "
        ));
        assert!(raw.ends_with("---\n\n# Do this\n\nBody"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert!(matches!(
            add_item(&dir, " \n ", false),
            Err(AddError::Empty)
        ));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn order_ignores_hidden_and_unreadable_files_and_accepts_comments() {
        let root = temp();
        std::fs::write(root.join("one.md"), "---\norder: 8 # imported\n---\n").unwrap();
        std::fs::write(root.join("broken.md"), [0xff, 0xfe]).unwrap();
        std::fs::write(root.join(".hidden.md"), "---\norder: 1000\n---\n").unwrap();
        assert_eq!(add_item(&root, "next", false).unwrap().order, 9);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collision_suffix_keeps_slug_within_sixty_characters() {
        let root = temp();
        let title = "x".repeat(80);
        let first = add_item(&root, &title, false).unwrap();
        let second = add_item(&root, &title, false).unwrap();
        assert_eq!(first.slug.len(), 60);
        assert_eq!(second.slug.len(), 60);
        assert!(second.slug.ends_with("-2"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
