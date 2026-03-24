//! Read/write AMI-style nvram dumps (what SCEWIN / SCEHUB export gives you).

use encoding_rs::WINDOWS_1252;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct BiosSetting {
    pub setup_question: String,
    pub help_string: String,
    pub token: String,
    pub offset: String,
    pub width: String,
    pub bios_default: Option<String>,
    pub options: Vec<String>,
    /// Which radio option is *'d, if any
    pub active_option: Option<usize>,
    /// Plain Value=… when there are no Options=
    pub value: Option<String>,
    pub content: Vec<String>,
}

impl BiosSetting {
    pub fn key(&self) -> Option<(String, String, String)> {
        let sq = self.setup_question.trim();
        let tk = self.token.trim();
        let off = self.offset.trim();
        if sq.is_empty() || tk.is_empty() || off.is_empty() {
            return None;
        }
        Some((sq.to_string(), tk.to_string(), off.to_string()))
    }

    pub fn display_current(&self) -> String {
        if let Some(i) = self.active_option {
            if i < self.options.len() {
                return self.options[i].clone();
            }
        }
        if let Some(ref v) = self.value {
            return v.clone();
        }
        if self.options.len() == 1 {
            return self.options[0].clone();
        }
        if self.options.is_empty() {
            return String::new();
        }
        self.options.join(", ")
    }
}

/// `[00]Foo` → `[00] Foo` so the list isn't cramped.
pub fn option_pretty(opt: &str) -> String {
    if let Some(close) = opt.find(']') {
        if opt.starts_with('[') && close + 1 < opt.len() {
            let left = &opt[..=close];
            let right = opt[close + 1..].trim_start();
            return format!("{left} {right}");
        }
    }
    opt.to_string()
}

pub struct LoadedNvram {
    pub settings: Vec<BiosSetting>,
    pub original_lines: Vec<String>,
    pub path: Option<std::path::PathBuf>,
}

fn strip_comment(line: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\s*)(//.*)?$").unwrap());
    let caps = re.captures(line);
    caps.map(|c| c.get(0).map(|m| &line[..m.start()]).unwrap_or(line))
        .unwrap_or(line)
        .to_string()
}

/// Picks apart Options= and continuation lines that start with *[..].
fn parse_options_line(line: &str, setting: &mut BiosSetting) {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\s*(\*?)(\[[0-9A-Fa-f]+\][^\[]*)").expect("option regex")
    });
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let mut matched = false;
    for caps in re.captures_iter(line) {
        matched = true;
        let star = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let body = caps
            .get(2)
            .map(|m| m.as_str().trim())
            .unwrap_or("")
            .trim_start_matches('*')
            .trim_start();
        if body.is_empty() {
            continue;
        }
        let index = setting.options.len();
        setting.options.push(body.to_string());
        if star == "*" {
            setting.active_option = Some(index);
        }
    }
    if matched {
        return;
    }
    let star_in_front = line.trim_start().starts_with('*');
    let clean_line = line.trim_start_matches('*').trim();
    if !clean_line.is_empty() {
        let index = setting.options.len();
        setting.options.push(clean_line.to_string());
        if star_in_front {
            setting.active_option = Some(index);
        }
    }
}

fn leading_indent_before_bracket(raw_line: &str) -> String {
    let s = strip_comment(raw_line);
    let Some(pos) = s.find('[') else {
        return String::new();
    };
    // Continuation lines: spaces then *[01]Label. That * is "this option is active", not indent.
    // If we leave * on the indent string, save doubles the star and both lines look selected.
    let mut prefix = s[..pos].to_string();
    while prefix.ends_with('*') {
        prefix.pop();
    }
    prefix
}

pub fn load_nvram(path: &Path) -> io::Result<LoadedNvram> {
    let bytes = fs::read(path)?;
    let (cow, _, _) = WINDOWS_1252.decode(&bytes);
    let text = cow.into_owned();

    let original_lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();

    static RE_SETUP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_HELP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_TOKEN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_OFFSET: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_WIDTH: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_BIOS_DEF: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_OPTIONS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_VALUE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_BRACKET: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

    let re_setup = RE_SETUP.get_or_init(|| {
        Regex::new(r"(?i)^Setup\s+Question\s*=\s*(.*)").unwrap()
    });
    let re_help = RE_HELP.get_or_init(|| Regex::new(r"(?i)^Help\s+String\s*=\s*(.*)").unwrap());
    let re_token = RE_TOKEN.get_or_init(|| Regex::new(r"(?i)^Token\s*=\s*(.*)").unwrap());
    let re_offset = RE_OFFSET.get_or_init(|| Regex::new(r"(?i)^Offset\s*=\s*(.*)").unwrap());
    let re_width = RE_WIDTH.get_or_init(|| Regex::new(r"(?i)^Width\s*=\s*(.*)").unwrap());
    let re_bios_def =
        RE_BIOS_DEF.get_or_init(|| Regex::new(r"(?i)^BIOS\s+Default\s*=\s*(.*)").unwrap());
    let re_options = RE_OPTIONS.get_or_init(|| Regex::new(r"(?i)^Options\s*=\s*(.*)").unwrap());
    let re_value = RE_VALUE.get_or_init(|| Regex::new(r"(?i)^Value\s*=\s*(.*)").unwrap());
    let re_bracket = RE_BRACKET.get_or_init(|| Regex::new(r"^\s*\*?\[.*?\]").unwrap());

    let mut settings: Vec<BiosSetting> = Vec::new();
    let mut current: Option<BiosSetting> = None;

    let mut finalize = |s: Option<BiosSetting>| {
        if let Some(mut s) = s {
            if s.options.is_empty() && s.active_option.is_none() && s.value.is_none() {
                // weird block but keep it
            } else if s.options.len() == 1 && s.active_option.is_none() {
                s.value = Some(s.options[0].clone());
                s.options.clear();
            }
            settings.push(s);
        }
    };

    for raw_line in &original_lines {
        let line = strip_comment(raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }

        if let Some(caps) = re_setup.captures(&line) {
            finalize(current.take());
            let q = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            current = Some(BiosSetting {
                setup_question: q,
                help_string: String::new(),
                token: String::new(),
                offset: String::new(),
                width: String::new(),
                bios_default: None,
                options: Vec::new(),
                active_option: None,
                value: None,
                content: Vec::new(),
            });
            continue;
        }

        let Some(ref mut cur) = current else {
            continue;
        };

        if let Some(caps) = re_help.captures(&line) {
            cur.help_string = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            continue;
        }
        if let Some(caps) = re_token.captures(&line) {
            cur.token = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            continue;
        }
        if let Some(caps) = re_offset.captures(&line) {
            cur.offset = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            continue;
        }
        if let Some(caps) = re_width.captures(&line) {
            cur.width = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            continue;
        }
        if let Some(caps) = re_bios_def.captures(&line) {
            cur.bios_default = caps.get(1).map(|m| m.as_str().trim().to_string());
            continue;
        }
        if let Some(caps) = re_options.captures(&line) {
            let remainder = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            parse_options_line(remainder, cur);
            continue;
        }
        if let Some(caps) = re_value.captures(&line) {
            let val = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
            cur.value = Some(val);
            continue;
        }
        if re_bracket.is_match(&line) {
            parse_options_line(&line, cur);
            continue;
        }
        cur.content.push(line);
    }

    finalize(current);

    Ok(LoadedNvram {
        settings,
        original_lines,
        path: Some(path.to_path_buf()),
    })
}

/// Rewrite the file in place: same layout as `original_lines`, patched stars + Value=.
#[allow(unused_assignments)]
pub fn save_nvram(
    path: &Path,
    original_lines: &[String],
    settings: &[BiosSetting],
) -> io::Result<()> {
    let mut qmap: HashMap<(String, String, String), usize> = HashMap::new();
    for (i, s) in settings.iter().enumerate() {
        if let Some(k) = s.key() {
            qmap.insert(k, i);
        }
    }

    static RE_SETUP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_TOKEN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_OFFSET: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_OPTIONS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_VALUE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static RE_BRACKET: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

    let re_setup = RE_SETUP.get_or_init(|| {
        Regex::new(r"(?i)^Setup\s+Question\s*=\s*(.*)").unwrap()
    });
    let re_token = RE_TOKEN.get_or_init(|| Regex::new(r"(?i)^Token\s*=\s*(.*)").unwrap());
    let re_offset = RE_OFFSET.get_or_init(|| Regex::new(r"(?i)^Offset\s*=\s*(.*)").unwrap());
    let re_options = RE_OPTIONS.get_or_init(|| Regex::new(r"(?i)^Options\s*=").unwrap());
    let re_value = RE_VALUE.get_or_init(|| Regex::new(r"(?i)^Value\s*=").unwrap());
    let re_bracket = RE_BRACKET.get_or_init(|| Regex::new(r"^\s*\*?\[.*?\]").unwrap());

    let re_comment = Regex::new(r"(//.*)$").unwrap();
    let re_leading_ws = Regex::new(r"^\s*").unwrap();

    let mut new_lines: Vec<String> = original_lines.to_vec();
    let mut current_sq: Option<String> = None;
    let mut current_token: Option<String> = None;
    let mut current_offset: Option<String> = None;
    let mut current_setting_idx: Option<usize> = None;

    let mut i = 0usize;
    while i < new_lines.len() {
        let raw_line = &new_lines[i];
        let line_stripped = strip_comment(raw_line).trim().to_string();
        if line_stripped.is_empty() {
            i += 1;
            continue;
        }

        if let Some(caps) = re_setup.captures(&line_stripped) {
            current_sq = caps.get(1).map(|m| m.as_str().trim().to_string());
            current_token = None;
            current_offset = None;
            current_setting_idx = None;
            i += 1;
            continue;
        }
        if let Some(caps) = re_token.captures(&line_stripped) {
            current_token = caps.get(1).map(|m| m.as_str().trim().to_string());
            i += 1;
            continue;
        }
        if let Some(caps) = re_offset.captures(&line_stripped) {
            current_offset = caps.get(1).map(|m| m.as_str().trim().to_string());
            if let (Some(ref sq), Some(ref tk), Some(ref off)) =
                (&current_sq, &current_token, &current_offset)
            {
                current_setting_idx = qmap.get(&(sq.clone(), tk.clone(), off.clone())).copied();
            } else {
                current_setting_idx = None;
            }
            i += 1;
            continue;
        }

        let Some(idx) = current_setting_idx else {
            i += 1;
            continue;
        };
        let setting = &settings[idx];

        if re_options.is_match(&line_stripped) {
            let mut option_block_end = i + 1;
            while option_block_end < new_lines.len() {
                let next_line = strip_comment(&new_lines[option_block_end]).trim().to_string();
                if !re_bracket.is_match(&next_line) {
                    break;
                }
                option_block_end += 1;
            }

            if !setting.options.is_empty() {
                let opt_header = line_stripped
                    .find('=')
                    .map(|p| line_stripped[..=p].to_string())
                    .unwrap_or_else(|| "Options\t=".to_string());
                let template_indent = if i + 1 < option_block_end {
                    leading_indent_before_bracket(&new_lines[i + 1])
                } else {
                    "         ".to_string()
                };
                let mut new_block: Vec<String> = Vec::new();
                for (opt_idx, opt) in setting.options.iter().enumerate() {
                    let opt_clean = opt.trim_start_matches('*').trim_start();
                    let is_active = setting.active_option == Some(opt_idx);
                    let star = if is_active { "*" } else { "" };
                    if opt_idx == 0 {
                        let original_line = new_lines[i].trim_end();
                        let comment = re_comment
                            .captures(original_line)
                            .and_then(|c| c.get(1).map(|m| m.as_str()))
                            .map(|c| format!("\t{c}"))
                            .unwrap_or_default();
                        new_block.push(format!(
                            "{}{}{}{}",
                            opt_header.as_str(),
                            star,
                            opt_clean,
                            comment
                        ));
                    } else {
                        let indent = if i + opt_idx < option_block_end {
                            leading_indent_before_bracket(&new_lines[i + opt_idx])
                        } else {
                            template_indent.clone()
                        };
                        new_block.push(format!("{indent}{star}{opt_clean}"));
                    }
                }
                let new_len = new_block.len();
                new_lines.splice(i..option_block_end, new_block);
                i += new_len;
                continue;
            }
        }

        if re_value.is_match(&line_stripped) {
            if let Some(ref val) = setting.value {
                let v_header = line_stripped
                    .find('=')
                    .map(|p| line_stripped[..=p].to_string())
                    .unwrap_or_else(|| "Value\t=".to_string());
                let comment = re_comment
                    .captures(raw_line)
                    .and_then(|c| c.get(1).map(|m| m.as_str()))
                    .map(|c| format!("\t{c}"))
                    .unwrap_or_default();
                let leading = re_leading_ws
                    .find(raw_line)
                    .map(|m| &raw_line[m.start()..m.end()])
                    .unwrap_or("");
                new_lines[i] = format!("{leading}{v_header}{val}{comment}");
            }
            i += 1;
            continue;
        }

        i += 1;
    }

    let mut out: Vec<u8> = Vec::new();
    for line in &new_lines {
        let (cow, _, _) = WINDOWS_1252.encode(line);
        out.write_all(cow.as_ref())?;
        out.push(b'\r');
        out.push(b'\n');
    }
    fs::write(path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn bios_setting_display_current_selected_option() {
        let s = BiosSetting {
            setup_question: "Q".into(),
            help_string: String::new(),
            token: "t".into(),
            offset: "o".into(),
            width: "1".into(),
            bios_default: None,
            options: vec!["[00]A".into(), "[01]B".into()],
            active_option: Some(1),
            value: None,
            content: vec![],
        };
        let d = s.display_current();
        assert!(d.contains('B'), "display_current: {d}");
    }

    #[test]
    fn load_minimal_nvram_with_options() {
        let path: PathBuf = std::env::temp_dir().join(format!(
            "nvram_editor_test_{}.txt",
            std::process::id()
        ));
        let text = "Setup Question = S1\n\
Token = 1\n\
Offset = 1\n\
Width = 01\n\
Options = *[00]Off  [01]On\n";
        std::fs::write(&path, text).unwrap();
        let loaded = load_nvram(&path).unwrap();
        assert_eq!(loaded.settings.len(), 1);
        assert_eq!(loaded.settings[0].setup_question, "S1");
        assert_eq!(loaded.settings[0].active_option, Some(0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_nvram_roundtrip_switch_active_option() {
        let path: PathBuf = std::env::temp_dir().join(format!(
            "nvram_editor_rt_{}.txt",
            std::process::id()
        ));
        let text = "Setup Question = S1\n\
Token = 1\n\
Offset = 1\n\
Width = 01\n\
Options = *[00]Off  [01]On\n";
        std::fs::write(&path, text).unwrap();
        let mut loaded = load_nvram(&path).unwrap();
        loaded.settings[0].active_option = Some(1);
        save_nvram(&path, &loaded.original_lines, &loaded.settings).unwrap();
        let again = load_nvram(&path).unwrap();
        assert_eq!(again.settings[0].active_option, Some(1));
        let _ = std::fs::remove_file(&path);
    }
}
