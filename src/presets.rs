//! Bulk-apply those "Setting [Option]" / "Setting = Option" lines from the text box.

use crate::nvram::{option_pretty, BiosSetting, LoadedNvram};

fn canonical_text(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn canonical_compact(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn singularize_token(t: &str) -> String {
    if t.len() > 4 && t.ends_with("es") {
        return t[..t.len() - 2].to_string();
    }
    if t.len() > 3 && t.ends_with('s') {
        return t[..t.len() - 1].to_string();
    }
    t.to_string()
}

pub fn setting_name_matches(preset_name: &str, setup_question: &str) -> bool {
    let a = canonical_text(preset_name);
    let b = canonical_text(setup_question);
    if a == b {
        return true;
    }
    let ac = canonical_compact(&a);
    let bc = canonical_compact(&b);
    if ac == bc || (!ac.is_empty() && (bc.contains(&ac) || ac.contains(&bc))) {
        return true;
    }
    let asg = singularize_token(&ac);
    let bsg = singularize_token(&bc);
    if !asg.is_empty() && (bsg.contains(&asg) || asg.contains(&bsg)) {
        return true;
    }
    let p = preset_name
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();
    let q = setup_question
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();
    if (4..=48).contains(&p.len()) && p.len() <= q.len() && q.contains(&p) {
        return true;
    }
    false
}

fn option_value_matches_single(preset_option: &str, option_text: &str) -> bool {
    let a = canonical_text(preset_option);
    let b = canonical_text(option_text);
    if a == b {
        return true;
    }
    let ac = canonical_compact(&a);
    let bc = canonical_compact(&b);
    if ac == bc || (!ac.is_empty() && (bc.starts_with(&ac) || ac.starts_with(&bc))) {
        return true;
    }
    let asg = singularize_token(&ac);
    let bsg = singularize_token(&bc);
    !asg.is_empty() && (bsg.starts_with(&asg) || asg.starts_with(&bsg))
}

/// Map sloppy preset words to what the BIOS file actually says (Disable ~= Disabled).
fn preset_option_synonym_strings(compact: &str) -> Vec<&'static str> {
    match compact {
        "disable" | "disabled" => vec!["Disable", "Disabled"],
        "enable" | "enabled" => vec!["Enable", "Enabled"],
        "on" => vec!["On", "Enabled", "Enable"],
        "off" => vec!["Off", "Disabled", "Disable"],
        "auto" => vec!["Auto"],
        _ => vec![],
    }
}

pub fn option_value_matches(preset_option: &str, option_text: &str) -> bool {
    if option_value_matches_single(preset_option, option_text) {
        return true;
    }
    let compact = canonical_compact(&canonical_text(preset_option));
    for alt in preset_option_synonym_strings(&compact) {
        if option_value_matches_single(alt, option_text) {
            return true;
        }
    }
    false
}

fn option_label_only(opt: &str) -> String {
    let pretty = option_pretty(opt);
    if let Some(close) = pretty.find(']') {
        return pretty[close + 1..].trim().to_string();
    }
    pretty.trim().to_string()
}

/// Same label twice in the dump → keep selections in sync when user picks one row.
pub fn duplicate_name_option_sync_plan(
    data: &LoadedNvram,
    setup_key: &str,
    new_sel: usize,
    source_opts: &[String],
) -> Vec<(usize, usize)> {
    if new_sel >= source_opts.len() {
        return Vec::new();
    }
    let chosen_pretty = option_pretty(&source_opts[new_sel]);
    let chosen_label = option_label_only(&source_opts[new_sel]);
    let mut out = Vec::new();
    for (j, s) in data.settings.iter().enumerate() {
        if s.setup_question.trim() != setup_key.trim() {
            continue;
        }
        if s.options.is_empty() {
            continue;
        }
        let ts = if s.options.as_slice() == source_opts {
            new_sel
        } else {
            let Some(k) = s.options.iter().position(|opt| {
                let full = option_pretty(opt);
                let lab = option_label_only(opt);
                full == chosen_pretty
                    || lab == chosen_label
                    || option_value_matches(&chosen_pretty, &full)
                    || option_value_matches(&chosen_label, &lab)
            }) else {
                continue;
            };
            k
        };
        if ts >= s.options.len() {
            continue;
        }
        if s.active_option == Some(ts) {
            continue;
        }
        out.push((j, ts));
    }
    out
}

fn normalize_preset_line(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\u{FEFF}' => ' ',
            '\u{00A0}' | '\u{202F}' => ' ',
            '\u{FF1D}' => '=',
            '\u{FF1A}' => ':',
            _ => c,
        })
        .collect::<String>()
}

fn parse_preset_target(raw: &str) -> String {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return inner.trim().to_string();
    }
    t.to_string()
}

pub fn parse_preset_line(line: &str) -> Option<(String, String)> {
    let t = normalize_preset_line(line).trim().to_string();
    if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
        return None;
    }
    let eq_split = t
        .split_once('=')
        .or_else(|| t.split_once('：'))
        .or_else(|| t.split_once(':'));
    if let Some((left, right)) = eq_split {
        let setting = left.trim();
        let target = parse_preset_target(right);
        if !setting.is_empty() && !target.is_empty() {
            return Some((setting.to_string(), target));
        }
    }
    if t.ends_with(']') {
        if let Some(pos) = t.rfind('[') {
            let setting = t[..pos].trim();
            let target = t[pos + 1..t.len() - 1].trim();
            if !setting.is_empty() && !target.is_empty() {
                return Some((setting.to_string(), target.to_string()));
            }
        }
    }
    None
}

/// One tuple per non-empty line: 1-based line no, setting name, wanted option.
pub fn parse_preset_input_lines(input: &str) -> Vec<(usize, String, String)> {
    input
        .trim_start_matches('\u{FEFF}')
        .lines()
        .enumerate()
        .filter_map(|(i, line)| parse_preset_line(line).map(|(a, b)| (i + 1, a, b)))
        .collect()
}

pub struct PresetApplyOutcome {
    pub applied: usize,
    pub unchanged: usize,
    pub errors: Vec<String>,
    /// Rows we feed into the session change log.
    pub logs: Vec<(String, String, String, String)>,
}

pub fn apply_presets_to_settings(
    settings: &mut [BiosSetting],
    parsed: &[(usize, String, String)],
) -> PresetApplyOutcome {
    let mut applied = 0usize;
    let mut unchanged = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut logs: Vec<(String, String, String, String)> = Vec::new();

    for (line_no, setting_name, target_option) in parsed {
        let matching_indices: Vec<usize> = settings
            .iter()
            .enumerate()
            .filter(|(_, s)| setting_name_matches(setting_name, s.setup_question.trim()))
            .map(|(i, _)| i)
            .collect();

        if matching_indices.is_empty() {
            errors.push(format!("Line {line_no}: setting not found: {setting_name}"));
            continue;
        }

        for idx in matching_indices {
            let s = &mut settings[idx];
            if s.options.is_empty() {
                errors.push(format!(
                    "Line {line_no}: '{}' (token {} / offset {}) does not have selectable options.",
                    s.setup_question.trim(),
                    s.token.trim(),
                    s.offset.trim()
                ));
                continue;
            }

            let target_idx = s.options.iter().position(|opt| {
                let full = option_pretty(opt);
                let label = option_label_only(opt);
                option_value_matches(target_option, &full)
                    || option_value_matches(target_option, &label)
            });
            let Some(new_idx) = target_idx else {
                errors.push(format!(
                    "Line {line_no}: option '{target_option}' not found for '{}' (token {} / offset {}).",
                    s.setup_question.trim(),
                    s.token.trim(),
                    s.offset.trim()
                ));
                continue;
            };

            if s.active_option == Some(new_idx) {
                unchanged += 1;
                let same_s = s
                    .options
                    .get(new_idx)
                    .map(|v| option_pretty(v))
                    .unwrap_or_else(|| "(unset)".to_string());
                logs.push((
                    s.setup_question.clone(),
                    s.token.clone(),
                    s.offset.clone(),
                    format!("{same_s} → {same_s} (preset)"),
                ));
                continue;
            }
            let old_s = s
                .active_option
                .and_then(|oi| s.options.get(oi))
                .map(|v| option_pretty(v))
                .unwrap_or_else(|| "(unset)".to_string());
            let new_s = s
                .options
                .get(new_idx)
                .map(|v| option_pretty(v))
                .unwrap_or_else(|| "(unset)".to_string());
            s.active_option = Some(new_idx);
            applied += 1;
            logs.push((
                s.setup_question.clone(),
                s.token.clone(),
                s.offset.clone(),
                format!("{old_s} → {new_s}"),
            ));
        }
    }

    PresetApplyOutcome {
        applied,
        unchanged,
        errors,
        logs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bracket_form() {
        let r = parse_preset_line("Global C-state Control [Disabled]").unwrap();
        assert_eq!(r.0, "Global C-state Control");
        assert_eq!(r.1, "Disabled");
    }

    #[test]
    fn parse_equals_form() {
        let r = parse_preset_line("Cstates = Disable").unwrap();
        assert_eq!(r.0, "Cstates");
        assert_eq!(r.1, "Disable");
    }

    #[test]
    fn setting_name_fuzzy() {
        assert!(setting_name_matches(
            "Cstate Control",
            "Global C-State Control"
        ));
    }
}
