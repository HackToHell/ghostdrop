//! Ghostdrop's own configuration (`~/.config/ghostdrop/config`), Ghostty syntax.
//! Keys Ghostdrop doesn't know are passed straight to libghostty.
use crate::keys::KeyCombo;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum Action {
    NewTab(Option<String>),
    CloseTab,
    NextTab,
    PreviousTab,
    LastTab,
    GotoTab(i32),
    MoveTab(i32),
    Toggle,
    Hide,
    RenameTab,
    ReloadConfig,
    OpenConfig,
    Quit,
    Unbind,
    Ghostty(String),
}

impl Action {
    pub fn parse(raw: &str) -> Option<Action> {
        let s = raw.trim();
        let (name, arg) = match s.split_once(':') {
            Some((n, a)) => (n, Some(a.to_string())),
            None => (s, None),
        };
        Some(match name {
            "new_tab" | "new_window" => Action::NewTab(arg.filter(|a| !a.is_empty())),
            "close_tab" | "close_surface" => Action::CloseTab,
            "next_tab" => Action::NextTab,
            "previous_tab" | "prev_tab" => Action::PreviousTab,
            "last_tab" => Action::LastTab,
            "goto_tab" => Action::GotoTab(arg?.parse().ok()?),
            "move_tab" => Action::MoveTab(arg?.parse().ok()?),
            "toggle" | "toggle_quick_terminal" | "toggle_visibility" => Action::Toggle,
            "hide" => Action::Hide,
            "rename_tab" | "prompt_surface_title" | "prompt_tab_title" => Action::RenameTab,
            "reload_config" => Action::ReloadConfig,
            "open_config" => Action::OpenConfig,
            "quit" => Action::Quit,
            "unbind" => Action::Unbind,
            "ghostty" => Action::Ghostty(arg?),
            _ => return None,
        })
    }

    pub fn describe(&self) -> String {
        match self {
            Action::NewTab(Some(p)) => format!("new_tab:{p}"),
            Action::NewTab(None) => "new_tab".into(),
            Action::CloseTab => "close_tab".into(),
            Action::NextTab => "next_tab".into(),
            Action::PreviousTab => "previous_tab".into(),
            Action::LastTab => "last_tab".into(),
            Action::GotoTab(n) => format!("goto_tab:{n}"),
            Action::MoveTab(n) => format!("move_tab:{n}"),
            Action::Toggle => "toggle".into(),
            Action::Hide => "hide".into(),
            Action::RenameTab => "rename_tab".into(),
            Action::ReloadConfig => "reload_config".into(),
            Action::OpenConfig => "open_config".into(),
            Action::Quit => "quit".into(),
            Action::Unbind => "unbind".into(),
            Action::Ghostty(a) => format!("ghostty:{a}"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub name: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub title: Option<String>,
    pub term: Option<String>,
    pub wait_after_command: bool,
}

impl Profile {
    pub fn new(name: &str) -> Profile {
        Profile { name: name.into(), ..Default::default() }
    }

    /// The command handed to libghostty (run via /bin/sh -c).
    pub fn effective_command(&self) -> Option<String> {
        let cmd = self.command.as_ref().filter(|c| !c.is_empty())?;
        Some(match &self.term {
            Some(t) => format!("/usr/bin/env TERM={t} {cmd}"),
            None => cmd.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarMode {
    Auto,
    Always,
    Never,
}

pub const BUILTIN_PROFILE: &str = "shell";

pub struct Settings {
    pub hotkey: Option<KeyCombo>,
    pub profiles: HashMap<String, Profile>,
    pub profile_order: Vec<String>,
    pub default_profile: String,
    pub bindings: Vec<(KeyCombo, Action)>,
    pub tab_bar: TabBarMode,
    pub tab_bar_top: bool,
    pub inherit_ghostty_config: bool,
    pub prespawn: bool,
    pub control_commands: bool,
    pub ghostty_lines: Vec<String>,
    /// Line in the Ghostdrop config each `ghostty_lines` entry came from.
    pub ghostty_line_src: Vec<usize>,
    pub errors: Vec<String>,
    pub created_template: bool,
}

/// Bindings Ghostty doesn't already provide on Linux. (Its defaults already
/// cover new/close/next/previous/move/goto tab, copy/paste and search.)
const DEFAULT_BINDINGS: &[(&str, &str)] = &[];

pub fn config_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x);
        }
    }
    home().join(".config")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest).to_string_lossy().into_owned()
    } else if p == "~" {
        home().to_string_lossy().into_owned()
    } else {
        p.to_string()
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        match std::env::var("GHOSTDROP_CONFIG") {
            Ok(p) if !p.is_empty() => PathBuf::from(expand_tilde(&p)),
            _ => config_dir().join("ghostdrop/config"),
        }
    }

    fn empty() -> Settings {
        let mut profiles = HashMap::new();
        profiles.insert(BUILTIN_PROFILE.to_string(), Profile::new(BUILTIN_PROFILE));
        Settings {
            hotkey: KeyCombo::parse("ctrl+grave_accent"),
            profiles,
            profile_order: vec![BUILTIN_PROFILE.to_string()],
            default_profile: BUILTIN_PROFILE.to_string(),
            bindings: Vec::new(),
            tab_bar: TabBarMode::Auto,
            tab_bar_top: true,
            inherit_ghostty_config: true,
            prespawn: true,
            control_commands: false,
            ghostty_lines: Vec::new(),
            ghostty_line_src: Vec::new(),
            errors: Vec::new(),
            created_template: false,
        }
    }

    pub fn load() -> Settings {
        let mut s = Settings::empty();
        for (k, a) in DEFAULT_BINDINGS {
            if let (Some(c), Some(act)) = (KeyCombo::parse(k), Action::parse(a)) {
                s.bindings.push((c, act));
            }
        }
        let path = Settings::path();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if std::fs::write(&path, TEMPLATE).is_ok() {
                s.created_template = true;
            }
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            s.parse(&text);
        }
        if !s.profiles.contains_key(&s.default_profile) {
            s.errors.push(format!(
                "default-profile '{}' is not defined; using '{}'",
                s.default_profile, BUILTIN_PROFILE
            ));
            s.default_profile = BUILTIN_PROFILE.into();
        }
        s
    }

    pub fn profile(&self, name: Option<&str>) -> Profile {
        let name = name.unwrap_or(&self.default_profile);
        self.profiles
            .get(name)
            .or_else(|| self.profiles.get(&self.default_profile))
            .cloned()
            .unwrap_or_else(|| Profile::new(BUILTIN_PROFILE))
    }

    fn profile_mut(&mut self, name: &str) -> &mut Profile {
        if !self.profiles.contains_key(name) {
            self.profiles.insert(name.to_string(), Profile::new(name));
            self.profile_order.push(name.to_string());
        }
        self.profiles.get_mut(name).unwrap()
    }

    pub fn parse(&mut self, text: &str) {
        for (n, raw) in text.lines().enumerate() {
            let ln = n + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                self.errors.push(format!("line {ln}: expected key = value"));
                continue;
            };
            let key = key.trim();
            let mut value = value.trim().to_string();
            if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                value = value[1..value.len() - 1].to_string();
            }

            if let Some(rest) = key.strip_prefix("profile.") {
                let Some((name, field)) = rest.rsplit_once('.') else {
                    self.errors.push(format!("line {ln}: expected profile.<name>.<field>"));
                    continue;
                };
                let name = name.to_string();
                match field {
                    "command" => self.profile_mut(&name).command = Some(value).filter(|v| !v.is_empty()),
                    "cwd" | "working-directory" => {
                        self.profile_mut(&name).cwd = Some(value).filter(|v| !v.is_empty()).map(|v| expand_tilde(&v))
                    }
                    "title" => self.profile_mut(&name).title = Some(value).filter(|v| !v.is_empty()),
                    "term" => {
                        if value.is_empty() {
                            self.profile_mut(&name).term = None;
                        } else if value.chars().all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c)) {
                            self.profile_mut(&name).term = Some(value);
                        } else {
                            self.errors.push(format!("line {ln}: invalid term '{value}'"));
                        }
                    }
                    "wait-after-command" => self.profile_mut(&name).wait_after_command = parse_bool(&value),
                    "env" => match value.split_once('=') {
                        Some((k, v)) => self.profile_mut(&name).env.push((k.to_string(), v.to_string())),
                        None => self.errors.push(format!("line {ln}: env must be KEY=VALUE")),
                    },
                    _ => self.errors.push(format!("line {ln}: unknown profile field '{field}'")),
                }
                continue;
            }

            match key {
                "hotkey" => {
                    if value.is_empty() || value == "none" {
                        self.hotkey = None;
                    } else if let Some(c) = KeyCombo::parse(&value) {
                        self.hotkey = Some(c);
                    } else {
                        self.errors.push(format!("line {ln}: invalid hotkey '{value}'"));
                    }
                }
                "default-profile" => self.default_profile = value,
                "bind" => self.parse_bind(&value, ln),
                "tab-bar" => match value.as_str() {
                    "auto" => self.tab_bar = TabBarMode::Auto,
                    "always" => self.tab_bar = TabBarMode::Always,
                    "never" => self.tab_bar = TabBarMode::Never,
                    _ => self.errors.push(format!("line {ln}: tab-bar must be auto|always|never")),
                },
                "tab-bar-position" => match value.as_str() {
                    "top" => self.tab_bar_top = true,
                    "bottom" => self.tab_bar_top = false,
                    _ => self.errors.push(format!("line {ln}: tab-bar-position must be top|bottom")),
                },
                "inherit-ghostty-config" => self.inherit_ghostty_config = parse_bool(&value),
                "prespawn" => self.prespawn = parse_bool(&value),
                "control-commands" => self.control_commands = parse_bool(&value),
                // macOS-only options: accepted so shared configs don't error.
                "status-item" | "dock-icon" => {}
                _ => {
                    self.ghostty_lines.push(line.to_string());
                    self.ghostty_line_src.push(ln);
                }
            }
        }
    }

    fn parse_bind(&mut self, value: &str, ln: usize) {
        if value == "clear" {
            self.bindings.clear();
            return;
        }
        let chars: Vec<char> = value.chars().collect();
        let split = (0..chars.len()).find(|&i| chars[i] == '=' && i > 0 && chars[i - 1] != '+');
        let Some(split) = split else {
            self.errors.push(format!("line {ln}: bind must be trigger=action"));
            return;
        };
        let trig: String = chars[..split].iter().collect();
        let act: String = chars[split + 1..].iter().collect();
        let Some(combo) = KeyCombo::parse(&trig) else {
            self.errors.push(format!("line {ln}: invalid trigger '{trig}'"));
            return;
        };
        let Some(action) = Action::parse(&act) else {
            self.errors.push(format!("line {ln}: unknown action '{act}'"));
            return;
        };
        self.bindings.retain(|(c, _)| !(c.mods == combo.mods && c.key == combo.key));
        if !matches!(action, Action::Unbind) {
            self.bindings.push((combo, action));
        }
    }
}

pub fn parse_bool(s: &str) -> bool {
    matches!(s.to_lowercase().as_str(), "true" | "yes" | "1" | "on")
}

pub const TEMPLATE: &str = r#"# Ghostdrop configuration.
#
# Ghostdrop reads your normal Ghostty config first (fonts, theme, keybinds...),
# then this file. Any key that isn't a Ghostdrop key below is passed straight to
# libghostty, so you can override Ghostty settings for the drop-down only.
# Comments must be on their own line. Reload: `ghostdrop ctl reload`.
#
# Want an AI agent to set this up for you? Point it at AGENTS.md in this folder.

# Show/hide hotkey. On X11 Ghostdrop grabs it globally. Wayland compositors don't
# let apps grab keys, so bind it in your compositor instead, e.g. for sway:
#   bindsym ctrl+grave exec ghostdrop
# hotkey = ctrl+grave_accent

# Geometry & behaviour use Ghostty's quick-terminal options:
# (top | bottom | left | right | center)
# quick-terminal-position = top
# (or 50%,80% / 600px)
# quick-terminal-size = 50%
# (seconds; ignored on Linux for now)
# quick-terminal-animation-duration = 0
# quick-terminal-autohide = true

# Profiles: what a new tab runs. The built-in "shell" profile is your login shell.
# profile.work.command = ssh my-host
# ($TERM override for hosts without ghostty's terminfo)
# profile.work.term = xterm-256color
# profile.work.env = FOO=bar
# profile.work.cwd = ~/src
# profile.work.title = work
# default-profile = shell

# App-level key bindings (handled before the terminal sees the key). Ghostty's
# own keybinds (ctrl+shift+t, ctrl+page_up/down, alt+1..9, ...) keep working too.
# bind = ctrl+shift+n=new_tab:work
# bind = ctrl+shift+w=close_tab
# bind = ctrl+shift+t=unbind

# (auto = hidden with one tab | always | never)
# tab-bar = auto
# tab-bar-position = top
# (start the first tab at launch)
# prespawn = true
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_profiles_and_binds() {
        let mut s = Settings::empty();
        s.parse("profile.dev.command = ssh dev\nprofile.dev.term = xterm-256color\ndefault-profile = dev\nbind = ctrl+shift+n=new_tab:dev\nfont-size = 12\nbind = cmd+==ghostty:increase_font_size:1\n");
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        assert_eq!(s.profile(None).effective_command().unwrap(), "/usr/bin/env TERM=xterm-256color ssh dev");
        assert_eq!(s.ghostty_lines, vec!["font-size = 12"]);
        assert_eq!(s.bindings.len(), 2);
    }
}
