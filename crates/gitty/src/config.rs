//! `~/.config/gitty/config.toml` (lenient: unknown keys and bad values warn and fall back to
//! defaults) and per-repo UI state under `~/.local/state/gitty/repos/`.

use std::path::Path;

use gitty_core::diff::ops::{DiffAlgorithm, WsMode};
use serde::{Deserialize, Serialize};

use crate::dates::DateMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Density {
    #[default]
    Compact,
    Comfortable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// A theme name, or "auto" (light or dark by the terminal background).
    pub theme: String,
    pub tab_size: u8,
    pub diff_algorithm: DiffAlgorithm,
    pub whitespace: WsMode,
    /// Terminal width at and above which split view turns on by itself.
    pub split_threshold: u16,
    pub date_mode: DateMode,
    pub density: Density,
    pub emph_alpha: Option<f32>,
    pub auto_fetch_minutes: u32,
    pub auto_tune: bool,
    /// The Files tab starts with ignored files listed (`i` toggles).
    pub files_show_ignored: bool,
    /// History starts with the commit graph shown (`L` toggles).
    pub history_graph: bool,
    pub difftool: Option<String>,
    /// `[keys]`: action name → key or keys (see `keymap`).
    pub keys: toml::Table,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            theme: "auto".into(),
            tab_size: 4,
            diff_algorithm: DiffAlgorithm::Myers,
            whitespace: WsMode::Show,
            split_threshold: 200,
            date_mode: DateMode::Relative,
            density: Density::Compact,
            emph_alpha: None,
            auto_fetch_minutes: 5,
            auto_tune: true,
            files_show_ignored: true,
            history_graph: true,
            difftool: None,
            keys: toml::Table::new(),
        }
    }
}

fn pick<T>(v: &toml::Value, key: &str, warnings: &mut Vec<String>, f: impl FnOnce(&toml::Value) -> Option<T>) -> Option<T> {
    let r = f(v);
    if r.is_none() {
        warnings.push(format!("config: invalid value for `{key}`: {v}"));
    }
    r
}

impl Config {
    pub fn load(path: &Path) -> (Config, Vec<String>) {
        let mut c = Config::default();
        let mut w = Vec::new();
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (c, w),
            Err(e) => {
                w.push(format!("config: {}: {e}", path.display()));
                return (c, w);
            }
        };
        let table: toml::Table = match toml::from_str(&text) {
            Ok(t) => t,
            Err(e) => {
                w.push(format!("config: {}: {e}", path.display()));
                return (c, w);
            }
        };
        let int = |v: &toml::Value| v.as_integer();
        for (k, v) in &table {
            let w = &mut w;
            match k.as_str() {
                "theme" => c.theme = pick(v, k, w, |v| v.as_str().map(String::from)).unwrap_or(c.theme),
                "tab_size" => c.tab_size = pick(v, k, w, int).map_or(c.tab_size, |n| n.clamp(1, 16) as u8),
                "diff_algorithm" => {
                    c.diff_algorithm = pick(v, k, w, |v| match v.as_str()? {
                        "myers" => Some(DiffAlgorithm::Myers),
                        "histogram" => Some(DiffAlgorithm::Histogram),
                        _ => None,
                    })
                    .unwrap_or(c.diff_algorithm)
                }
                "whitespace" => {
                    c.whitespace = pick(v, k, w, |v| match v.as_str()? {
                        "show" => Some(WsMode::Show),
                        "ignore-all" => Some(WsMode::IgnoreAll),
                        "ignore-amount" => Some(WsMode::IgnoreAmount),
                        _ => None,
                    })
                    .unwrap_or(c.whitespace)
                }
                "split_threshold" => c.split_threshold = pick(v, k, w, int).map_or(c.split_threshold, |n| n.clamp(0, 10_000) as u16),
                "date_mode" => {
                    c.date_mode = pick(v, k, w, |v| match v.as_str()? {
                        "relative" => Some(DateMode::Relative),
                        "absolute" => Some(DateMode::Absolute),
                        "both" => Some(DateMode::Both),
                        _ => None,
                    })
                    .unwrap_or(c.date_mode)
                }
                "density" => {
                    c.density = pick(v, k, w, |v| match v.as_str()? {
                        "compact" => Some(Density::Compact),
                        "comfortable" => Some(Density::Comfortable),
                        _ => None,
                    })
                    .unwrap_or(c.density)
                }
                "emph_alpha" => {
                    c.emph_alpha = pick(v, k, w, |v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
                        .map(|f| (f as f32).clamp(0.0, 1.0))
                        .or(c.emph_alpha)
                }
                "auto_fetch_minutes" => c.auto_fetch_minutes = pick(v, k, w, int).map_or(c.auto_fetch_minutes, |n| n.clamp(0, 1440) as u32),
                "auto_tune" => c.auto_tune = pick(v, k, w, toml::Value::as_bool).unwrap_or(c.auto_tune),
                "files_show_ignored" => c.files_show_ignored = pick(v, k, w, toml::Value::as_bool).unwrap_or(c.files_show_ignored),
                "history_graph" => c.history_graph = pick(v, k, w, toml::Value::as_bool).unwrap_or(c.history_graph),
                "difftool" => c.difftool = pick(v, k, w, |v| v.as_str().map(String::from)).or(c.difftool.take()),
                "keys" => match v.as_table() {
                    Some(t) => c.keys = t.clone(),
                    None => w.push("config: `keys` must be a table: [keys]".into()),
                },
                _ => w.push(format!("config: unknown key `{k}`")),
            }
        }
        (c, w)
    }

    /// Persists `theme = "<name>"`, keeping the rest of the file byte-for-byte.
    pub fn save_theme(path: &Path, name: &str) -> std::io::Result<()> {
        // only a missing file counts as empty; never overwrite a config we could not read
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let value = toml::Value::String(name.to_string()).to_string();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, set_top_level_key(&text, "theme", &value))
    }
}

/// Sets `key = value_toml` at the top level of a TOML document: replaces the first top-level
/// assignment of `key`, or inserts one after the leading comment block. Other bytes are kept.
pub fn set_top_level_key(text: &str, key: &str, value_toml: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let new_line = format!("{key} = {value_toml}\n");
    let is_key = |l: &str| {
        let t = l.trim_start();
        t.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    let first_table = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    let mut out = String::with_capacity(text.len() + new_line.len());
    if let Some(i) = lines[..first_table].iter().position(|l| is_key(l)) {
        for (j, l) in lines.iter().enumerate() {
            out.push_str(if i == j { &new_line } else { l });
        }
        return out;
    }
    let at = lines[..first_table].iter().take_while(|l| l.trim_start().starts_with('#')).count();
    for (j, l) in lines.iter().enumerate() {
        if j == at {
            out.push_str(&new_line);
        }
        out.push_str(l);
        if j + 1 == lines.len() && !l.ends_with('\n') && at > j {
            out.push('\n');
        }
    }
    if at >= lines.len() {
        out.push_str(&new_line);
    }
    out
}

/// Per-repo UI state; never written into the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub history_width: Option<u16>,
    pub files_width: Option<u16>,
    pub files_height: Option<u16>,
    pub scope_all: bool,
    /// Width of the Changes tab's left column (file list and commit box).
    pub changes_width: Option<u16>,
    /// History file list as a directory tree (`t`).
    pub tree_view: bool,
}

impl UiState {
    pub fn load(path: &Path) -> UiState {
        std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }
}

pub mod paths {
    use std::path::{Path, PathBuf};

    fn base(env: &impl Fn(&str) -> Option<String>, xdg: &str, home_rel: &str) -> PathBuf {
        match env(xdg).filter(|s| !s.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(env("HOME").unwrap_or_else(|| ".".into())).join(home_rel),
        }
    }
    pub fn config_dir(env: impl Fn(&str) -> Option<String>) -> PathBuf {
        base(&env, "XDG_CONFIG_HOME", ".config").join("gitty")
    }
    pub fn state_dir(env: impl Fn(&str) -> Option<String>) -> PathBuf {
        base(&env, "XDG_STATE_HOME", ".local/state").join("gitty")
    }
    /// `<state>/repos/<fnv64 of the repo path>.toml`.
    pub fn repo_state_file(state_dir: &Path, repo: &Path) -> PathBuf {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in repo.as_os_str().as_encoded_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        state_dir.join("repos").join(format!("{h:016x}.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use gitty_core::diff::ops::{DiffAlgorithm, WsMode};

    fn load_str(s: &str) -> (Config, Vec<String>) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        std::fs::write(&p, s).unwrap();
        Config::load(&p)
    }

    #[test]
    fn defaults_when_missing() {
        let (c, w) = Config::load(Path::new("/nonexistent/gitty/config.toml"));
        assert!(w.is_empty());
        assert_eq!(c.theme, "auto");
        assert_eq!(c.tab_size, 4);
        assert_eq!(c.split_threshold, 200);
        assert_eq!(c.date_mode, DateMode::Relative);
        assert_eq!(c.density, Density::Compact);
        assert_eq!(c.whitespace, WsMode::Show);
        assert_eq!(c.diff_algorithm, DiffAlgorithm::Myers);
        assert!(c.auto_tune);
        assert!(c.files_show_ignored);
        assert!(c.history_graph);
        assert_eq!(c.emph_alpha, None);
    }

    #[test]
    fn parses_all_keys() {
        let (c, w) = load_str(
            "theme = \"dracula\"\ntab_size = 8\ndiff_algorithm = \"histogram\"\nwhitespace = \"ignore-all\"\n\
             split_threshold = 180\ndate_mode = \"both\"\ndensity = \"comfortable\"\nemph_alpha = 0.4\n\
             auto_fetch_minutes = 0\nauto_tune = false\nfiles_show_ignored = false\nhistory_graph = false\ndifftool = \"code --diff\"\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.theme, "dracula");
        assert_eq!(c.tab_size, 8);
        assert_eq!(c.diff_algorithm, DiffAlgorithm::Histogram);
        assert_eq!(c.whitespace, WsMode::IgnoreAll);
        assert_eq!(c.split_threshold, 180);
        assert_eq!(c.date_mode, DateMode::Both);
        assert_eq!(c.density, Density::Comfortable);
        assert_eq!(c.emph_alpha, Some(0.4));
        assert_eq!(c.auto_fetch_minutes, 0);
        assert!(!c.auto_tune);
        assert!(!c.files_show_ignored);
        assert!(!c.history_graph);
        assert_eq!(c.difftool.as_deref(), Some("code --diff"));
    }

    #[test]
    fn a_non_bool_files_show_ignored_warns_and_keeps_the_default() {
        let (c, w) = load_str("files_show_ignored = \"no\"\n");
        assert!(c.files_show_ignored);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("files_show_ignored"), "{w:?}");
    }

    #[test]
    fn a_non_bool_history_graph_warns_and_keeps_the_default() {
        let (c, w) = load_str("history_graph = 0\n");
        assert!(c.history_graph);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("history_graph"), "{w:?}");
    }

    #[test]
    fn unknown_key_warns() {
        let (c, w) = load_str("colour = \"red\"\ntheme = \"dracula\"\n");
        assert_eq!(c.theme, "dracula");
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("colour"));
    }

    #[test]
    fn bad_value_warns_and_defaults() {
        let (c, w) = load_str("tab_size = \"wide\"\nwhitespace = \"sometimes\"\n");
        assert_eq!(c.tab_size, 4);
        assert_eq!(c.whitespace, WsMode::Show);
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn unparsable_file_warns_and_defaults() {
        let (c, w) = load_str("theme = [");
        assert_eq!(c.theme, "auto");
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn tab_size_clamped() {
        assert_eq!(load_str("tab_size = 0").0.tab_size, 1);
        assert_eq!(load_str("tab_size = 99").0.tab_size, 16);
    }

    #[test]
    fn set_key_replaces_existing() {
        let t = "# my config\ntheme = \"dracula\" # fav\ntab_size = 2\n";
        assert_eq!(set_top_level_key(t, "theme", "\"rose-pine\""), "# my config\ntheme = \"rose-pine\"\ntab_size = 2\n");
    }

    #[test]
    fn set_key_into_empty() {
        assert_eq!(set_top_level_key("", "theme", "\"x\""), "theme = \"x\"\n");
    }

    #[test]
    fn set_key_inserts_after_leading_comments() {
        let t = "# header\n# more\n\ntab_size = 2\n";
        assert_eq!(set_top_level_key(t, "theme", "\"x\""), "# header\n# more\ntheme = \"x\"\n\ntab_size = 2\n");
    }

    #[test]
    fn set_key_with_only_tables_goes_before_them() {
        let t = "[keys]\ntheme = \"not-this\"\n";
        assert_eq!(set_top_level_key(t, "theme", "\"x\""), "theme = \"x\"\n[keys]\ntheme = \"not-this\"\n");
    }

    #[test]
    fn set_key_does_not_match_prefix_keys() {
        let t = "themes_dir = \"a\"\n";
        assert_eq!(set_top_level_key(t, "theme", "\"x\""), "theme = \"x\"\nthemes_dir = \"a\"\n");
    }

    #[test]
    fn save_theme_creates_dirs_and_keeps_rest() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a/b/config.toml");
        Config::save_theme(&p, "dracula").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "theme = \"dracula\"\n");
        std::fs::write(&p, "tab_size = 2\n").unwrap();
        Config::save_theme(&p, "rose-pine").unwrap();
        let (c, w) = Config::load(&p);
        assert!(w.is_empty());
        assert_eq!((c.theme.as_str(), c.tab_size), ("rose-pine", 2));
    }

    #[test]
    fn save_theme_never_clobbers_an_unreadable_config() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        let original = b"tab_size = 2\n# \xff\xfe not utf-8\n".to_vec();
        std::fs::write(&p, &original).unwrap();
        assert!(Config::save_theme(&p, "dracula").is_err());
        assert_eq!(std::fs::read(&p).unwrap(), original);
        let (_, w) = Config::load(&p);
        assert_eq!(w.len(), 1, "an unreadable config is reported");
    }

    #[test]
    fn ui_state_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("repos/x.toml");
        assert_eq!(UiState::load(&p), UiState::default());
        let s = UiState { history_width: Some(70), files_width: None, files_height: Some(9), changes_width: Some(50), scope_all: true, tree_view: true };
        s.save(&p).unwrap();
        assert_eq!(UiState::load(&p), s);
        std::fs::write(&p, "garbage = [").unwrap();
        assert_eq!(UiState::load(&p), UiState::default());
    }

    #[test]
    fn state_file_is_stable_per_repo() {
        let a = paths::repo_state_file(Path::new("/s"), Path::new("/work/a"));
        assert_eq!(a, paths::repo_state_file(Path::new("/s"), Path::new("/work/a")));
        assert_ne!(a, paths::repo_state_file(Path::new("/s"), Path::new("/work/b")));
        assert!(a.starts_with("/s/repos"));
    }

    #[test]
    fn xdg_dirs() {
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/x/cfg".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(paths::config_dir(env), PathBuf::from("/x/cfg/gitty"));
        assert_eq!(paths::state_dir(env), PathBuf::from("/home/u/.local/state/gitty"));
        let env2 = |k: &str| if k == "XDG_CONFIG_HOME" { Some(String::new()) } else if k == "HOME" { Some("/h".into()) } else { None };
        assert_eq!(paths::config_dir(env2), PathBuf::from("/h/.config/gitty"));
    }
}
