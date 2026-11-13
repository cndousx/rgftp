use std::path::{Path, PathBuf};

pub fn join_remote(base: &str, name: &str) -> String {
    if name.is_empty() {
        return base.to_string();
    }
    if base.is_empty() || base == "/" {
        format!("/{name}")
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

pub fn remote_parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => trimmed[..index].to_string(),
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

pub fn format_time(timestamp: Option<i64>) -> String {
    let Some(seconds) = timestamp else {
        return "—".to_string();
    };
    let Some(utc) = chrono::DateTime::from_timestamp(seconds, 0) else {
        return "—".to_string();
    };
    utc.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

pub fn format_progress(transferred: u64, total: u64) -> String {
    if total == 0 {
        format_size(transferred)
    } else {
        let pct = (transferred as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
        format!(
            "{} / {}  {:.0}%",
            format_size(transferred),
            format_size(total),
            pct
        )
    }
}

/// Case-insensitive subsequence match used by the file pane search boxes.
///
/// `"rgt"` matches `"rgftp-config.json"` but `"gtr"` does not.
pub fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let mut needle_chars = needle.chars();
    let mut wanted = needle_chars.next();

    for hay in haystack.to_lowercase().chars() {
        match wanted {
            Some(expected) if hay == expected => wanted = needle_chars.next(),
            _ => {}
        }
        if wanted.is_none() {
            return true;
        }
    }
    wanted.is_none()
}

/// Formats unix permission bits as octal digits, e.g. `0o644` becomes `644`.
pub fn format_mode(mode: u32) -> String {
    format!("{:03o}", mode & 0o777)
}

/// Formats unix permission bits in the familiar `rwxr-xr-x` form.
pub fn format_mode_symbolic(mode: u32) -> String {
    let bit = |mask: u32, yes: char| if mode & mask != 0 { yes } else { '-' };
    [
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        bit(0o100, 'x'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        bit(0o010, 'x'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        bit(0o001, 'x'),
    ]
    .into_iter()
    .collect()
}

pub fn format_speed(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        return "0 B/s".to_string();
    }
    format!("{}/s", format_size(bytes_per_second.round() as u64))
}

pub fn expand_tilde(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(rest);
    }
    PathBuf::from(path)
}

pub fn path_file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_remote_paths() {
        assert_eq!(join_remote("/", "a"), "/a");
        assert_eq!(join_remote("/home/me", "a"), "/home/me/a");
        assert_eq!(join_remote("/home/me/", "a"), "/home/me/a");
    }

    #[test]
    fn finds_remote_parents() {
        assert_eq!(remote_parent("/"), "/");
        assert_eq!(remote_parent("/home"), "/");
        assert_eq!(remote_parent("/home/me"), "/home");
        assert_eq!(remote_parent("/home/me/"), "/home");
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_size(1536), "1.50 KB");
    }

    #[test]
    fn fuzzy_matching_is_case_insensitive_and_ordered() {
        assert!(fuzzy_match("rgftp-config.json", ""));
        assert!(fuzzy_match("rgftp-config.json", "rgt"));
        assert!(fuzzy_match("RGFTP-config.json", "rgt"));
        assert!(fuzzy_match("rgftp-config.json", "CONFIG"));
        assert!(!fuzzy_match("rgftp-config.json", "gtr"));
        assert!(!fuzzy_match("abc", "abcd"));
    }

    #[test]
    fn formats_speeds() {
        assert_eq!(format_speed(0.0), "0 B/s");
        assert_eq!(format_speed(2048.0), "2.00 KB/s");
        assert_eq!(format_speed(f64::NAN), "0 B/s");
    }

    #[test]
    fn formats_permission_modes() {
        assert_eq!(format_mode(0o644), "644");
        assert_eq!(format_mode(0o100755 & 0o777), "755");
        assert_eq!(format_mode_symbolic(0o644), "rw-r--r--");
        assert_eq!(format_mode_symbolic(0o755), "rwxr-xr-x");
        assert_eq!(format_mode_symbolic(0), "---------");
    }
}
