//! What the TUI remembers between runs, in `tui.json` next to the node's data: the grouping each space was
//! last shown with, and the space last looked at. The filter is not remembered (board spec BRD-22).

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use han_node::api::Group;
use serde::{Deserialize, Serialize};

/// The persisted choices of one user on this device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The grouping chosen for each space.
    #[serde(default)]
    pub groups: BTreeMap<String, Group>,
    /// The space shown when none is named.
    #[serde(default)]
    pub last_space: Option<String>,
    /// Recovery notice saved before a setup operation; contains no invite or home ticket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_recovery: Option<String>,
}

impl Config {
    /// Reads the file; a missing or unreadable file is the default (a bad config never stops the TUI).
    pub fn load(path: &Path) -> Config {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Writes the file through a temporary file, so a crash cannot leave half of it.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        std::fs::rename(&tmp, path)
    }

    /// The grouping of a space.
    pub fn group_of(&self, space: &str) -> Group {
        self.groups.get(space).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_survives_garbage() {
        let dir = std::env::temp_dir().join(format!("han-tui-cfg-{}", std::process::id()));
        let path = dir.join("tui.json");
        let mut c = Config::default();
        c.groups.insert("sp".into(), Group::Person);
        c.last_space = Some("sp".into());
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path), c);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Config::load(&path), Config::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
