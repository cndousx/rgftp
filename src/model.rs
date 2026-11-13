use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// A reasonably unique, sortable id without pulling in a UUID dependency.
pub fn new_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos:x}-{counter:x}")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuthMethod {
    Password { password: String },
    PrivateKey { path: String, passphrase: String },
}

impl Default for AuthMethod {
    fn default() -> Self {
        Self::Password {
            password: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionGroup {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub group_id: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    /// Empty means the login/home directory reported by the server.
    #[serde(default)]
    pub remote_dir: String,
}

impl ConnectionProfile {
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThemePreference {
    Light,
    Dark,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub groups: Vec<ConnectionGroup>,
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
    #[serde(default)]
    pub local_dir: Option<PathBuf>,
    /// `None` means no manual choice has been saved, so the system theme is used.
    #[serde(default)]
    pub theme: Option<ThemePreference>,
}

impl Settings {
    pub fn load() -> Self {
        let path = config_path();
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(mut settings) = serde_json::from_str::<Settings>(&text)
        {
            if settings.normalize_groups() {
                settings.save();
            }
            return settings;
        }

        Self {
            groups: vec![ConnectionGroup {
                id: new_id("group"),
                name: "我的连接".to_string(),
            }],
            connections: Vec::new(),
            local_dir: None,
            theme: None,
        }
    }

    /// Older versions allowed connections without a group. Every connection
    /// must now belong to an existing group, so adopt orphans into the first
    /// group (creating one when necessary). Returns whether anything changed.
    fn normalize_groups(&mut self) -> bool {
        let mut changed = false;
        if self.groups.is_empty() && !self.connections.is_empty() {
            self.groups.push(ConnectionGroup {
                id: new_id("group"),
                name: "我的连接".to_string(),
            });
            changed = true;
        }
        let Some(fallback) = self.groups.first().map(|group| group.id.clone()) else {
            return changed;
        };
        let group_ids: Vec<String> = self.groups.iter().map(|group| group.id.clone()).collect();
        for connection in &mut self.connections {
            let valid = connection
                .group_id
                .as_ref()
                .is_some_and(|id| group_ids.iter().any(|group_id| group_id == id));
            if !valid {
                connection.group_id = Some(fallback.clone());
                changed = true;
            }
        }
        changed
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn connection(&self, id: &str) -> Option<&ConnectionProfile> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn group(&self, id: &str) -> Option<&ConnectionGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// Picks the target side that actually changes the order. When the pointer
    /// is on the side where the item already sits, use the other side so
    /// dropping an item onto its neighbour still reorders the list.
    pub fn normalize_group_drop(&self, id: &str, target_id: &str, after: bool) -> bool {
        let Some(from) = self.groups.iter().position(|group| group.id == id) else {
            return after;
        };
        let Some(target) = self.groups.iter().position(|group| group.id == target_id) else {
            return after;
        };
        let no_op = if after {
            from == target + 1
        } else {
            from + 1 == target
        };
        if no_op { !after } else { after }
    }

    /// Same as [`Self::normalize_group_drop`], but compares positions inside
    /// the connection's group, which is the order the sidebar displays.
    pub fn normalize_connection_drop(&self, id: &str, target_id: &str, after: bool) -> bool {
        let Some(source) = self.connection(id) else {
            return after;
        };
        let Some(target) = self.connection(target_id) else {
            return after;
        };
        if source.group_id != target.group_id {
            return after;
        }

        let mut from = None;
        let mut to = None;
        let mut index = 0;
        for connection in &self.connections {
            if connection.group_id == source.group_id {
                if connection.id == id {
                    from = Some(index);
                }
                if connection.id == target_id {
                    to = Some(index);
                }
                index += 1;
            }
        }
        let (Some(from), Some(to)) = (from, to) else {
            return after;
        };
        let no_op = if after {
            from == to + 1
        } else {
            from + 1 == to
        };
        if no_op { !after } else { after }
    }

    /// Moves a saved group next to another group. Returns whether it was found.
    pub fn move_group(&mut self, id: &str, target_id: &str, after: bool) -> bool {
        if id == target_id {
            return false;
        }
        let after = self.normalize_group_drop(id, target_id, after);
        let Some(from) = self.groups.iter().position(|group| group.id == id) else {
            return false;
        };
        if !self.groups.iter().any(|group| group.id == target_id) {
            return false;
        }

        let group = self.groups.remove(from);
        let to = self
            .groups
            .iter()
            .position(|group| group.id == target_id)
            .expect("target group checked above");
        let to = if after { to + 1 } else { to };
        self.groups.insert(to, group);
        true
    }

    /// Moves a connection next to another connection and adopts the target's group.
    pub fn move_connection(&mut self, id: &str, target_id: &str, after: bool) -> bool {
        if id == target_id {
            return false;
        }
        let after = self.normalize_connection_drop(id, target_id, after);
        let Some(from) = self
            .connections
            .iter()
            .position(|connection| connection.id == id)
        else {
            return false;
        };
        let Some(target_group) = self
            .connections
            .iter()
            .find(|connection| connection.id == target_id)
            .map(|connection| connection.group_id.clone())
        else {
            return false;
        };

        let mut connection = self.connections.remove(from);
        connection.group_id = target_group;
        let to = self
            .connections
            .iter()
            .position(|connection| connection.id == target_id)
            .expect("target connection checked above");
        let to = if after { to + 1 } else { to };
        self.connections.insert(to, connection);
        true
    }

    /// Moves a connection to the end of an existing group.
    pub fn move_connection_to_group(&mut self, id: &str, group_id: &str) -> bool {
        if !self.groups.iter().any(|group| group.id == group_id) {
            return false;
        }
        let Some(from) = self
            .connections
            .iter()
            .position(|connection| connection.id == id)
        else {
            return false;
        };

        let mut connection = self.connections.remove(from);
        connection.group_id = Some(group_id.to_string());
        self.connections.push(connection);
        true
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rgftp")
        .join("profiles.json")
}

/// One row in either file pane.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix timestamp in seconds.
    pub modified: Option<i64>,
    /// Unix permission bits when the file system exposes them.
    pub permissions: Option<u32>,
    /// Hidden files (dotfiles, or Windows hidden/system attribute) are kept in
    /// `all_entries` but filtered out of the visible listing.
    pub hidden: bool,
}

pub fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_preference_round_trips_and_defaults_to_system() {
        let settings = Settings {
            theme: Some(ThemePreference::Dark),
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("\"theme\":\"Dark\""));

        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.theme, Some(ThemePreference::Dark));

        let legacy: Settings = serde_json::from_str(r#"{"groups":[],"connections":[]}"#).unwrap();
        assert_eq!(legacy.theme, None);
    }

    fn group(id: &str) -> ConnectionGroup {
        ConnectionGroup {
            id: id.to_string(),
            name: id.to_string(),
        }
    }

    fn connection(id: &str, group_id: Option<&str>) -> ConnectionProfile {
        ConnectionProfile {
            id: id.to_string(),
            name: id.to_string(),
            group_id: group_id.map(str::to_string),
            host: "example.test".to_string(),
            port: 22,
            username: "user".to_string(),
            auth: AuthMethod::default(),
            remote_dir: String::new(),
        }
    }

    fn group_ids(settings: &Settings) -> Vec<&str> {
        settings
            .groups
            .iter()
            .map(|group| group.id.as_str())
            .collect()
    }

    fn connection_ids(settings: &Settings) -> Vec<&str> {
        settings
            .connections
            .iter()
            .map(|connection| connection.id.as_str())
            .collect()
    }

    #[test]
    fn groups_and_connections_can_be_reordered() {
        let mut settings = Settings {
            groups: vec![group("a"), group("b"), group("c")],
            connections: vec![
                connection("1", Some("a")),
                connection("2", Some("a")),
                connection("3", Some("b")),
            ],
            ..Default::default()
        };

        assert!(settings.move_group("c", "a", false));
        assert_eq!(group_ids(&settings), ["c", "a", "b"]);
        assert!(settings.move_group("c", "b", true));
        assert_eq!(group_ids(&settings), ["a", "b", "c"]);
        assert!(!settings.move_group("a", "a", true));
        // Dropping onto the adjacent side that is already occupied must still
        // produce a move instead of doing nothing.
        assert!(!settings.normalize_group_drop("b", "a", true));
        assert!(settings.move_group("c", "b", true));
        assert_eq!(group_ids(&settings), ["a", "c", "b"]);

        assert!(settings.move_connection("3", "1", true));
        assert_eq!(connection_ids(&settings), ["1", "3", "2"]);
        assert_eq!(
            settings.connection("3").unwrap().group_id.as_deref(),
            Some("a")
        );

        assert!(settings.move_connection("3", "2", false));
        assert_eq!(connection_ids(&settings), ["1", "2", "3"]);
        assert!(settings.move_connection("3", "2", true));
        assert_eq!(connection_ids(&settings), ["1", "3", "2"]);
    }

    #[test]
    fn dropping_the_second_item_onto_the_first_reorders() {
        let mut settings = Settings {
            groups: vec![group("a"), group("b")],
            connections: vec![connection("1", Some("a")), connection("2", Some("a"))],
            ..Default::default()
        };

        // The pointer may be on the lower half of the first row/header, which
        // would normally mean "after the first item" and be a no-op.
        assert!(settings.move_group("b", "a", true));
        assert_eq!(group_ids(&settings), ["b", "a"]);
        assert!(settings.move_connection("2", "1", true));
        assert_eq!(connection_ids(&settings), ["2", "1"]);
    }

    #[test]
    fn connections_can_move_to_another_group() {
        let mut settings = Settings {
            groups: vec![group("a"), group("b")],
            connections: vec![connection("1", Some("a")), connection("2", Some("b"))],
            ..Default::default()
        };

        assert!(settings.move_connection_to_group("1", "b"));
        assert_eq!(
            settings.connection("1").unwrap().group_id.as_deref(),
            Some("b")
        );
        assert_eq!(connection_ids(&settings), ["2", "1"]);

        assert!(!settings.move_connection_to_group("1", "missing"));
        assert!(!settings.move_connection_to_group("missing", "a"));
    }

    #[test]
    fn ungrouped_connections_are_adopted_by_the_first_group() {
        let mut settings = Settings {
            connections: vec![connection("1", None)],
            ..Default::default()
        };
        assert!(settings.normalize_groups());
        assert_eq!(settings.groups.len(), 1);
        assert_eq!(
            settings.connection("1").unwrap().group_id.as_deref(),
            Some(settings.groups[0].id.as_str())
        );

        let mut settings = Settings {
            groups: vec![group("a")],
            connections: vec![
                connection("1", None),
                connection("2", Some("missing")),
                connection("3", Some("a")),
            ],
            ..Default::default()
        };
        assert!(settings.normalize_groups());
        assert_eq!(
            settings.connection("1").unwrap().group_id.as_deref(),
            Some("a")
        );
        assert_eq!(
            settings.connection("2").unwrap().group_id.as_deref(),
            Some("a")
        );
        assert_eq!(
            settings.connection("3").unwrap().group_id.as_deref(),
            Some("a")
        );
    }
}
