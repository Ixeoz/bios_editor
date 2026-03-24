//! Sanity checks for free-form Value= lines (hex vs decimal, width, angle brackets).
//! Enum options don't go through this.

use crate::nvram::BiosSetting;

pub fn setting_value_changed(current: &BiosSetting, original: &BiosSetting) -> bool {
    current.active_option != original.active_option || current.value != original.value
}

pub fn extract_angle_value(v: &str) -> Option<&str> {
    if v.len() >= 2 && v.starts_with('<') && v.ends_with('>') {
        Some(&v[1..v.len() - 1])
    } else {
        None
    }
}

fn parse_width_bytes(width: &str) -> Option<u32> {
    let w = width.trim();
    if w.is_empty() {
        return None;
    }
    u32::from_str_radix(w.trim_start_matches("0x").trim_start_matches("0X"), 16)
        .ok()
        .filter(|v| *v > 0)
}

fn is_hex_like(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("0x")
        || t.starts_with("0X")
        || t.ends_with('h')
        || t.ends_with('H')
        || t.chars().any(|c| ('a'..='f').contains(&c) || ('A'..='F').contains(&c))
}

fn is_decimal_like(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

fn is_valid_hex_token(s: &str) -> bool {
    let t = s.trim();
    let core = if t.starts_with("0x") || t.starts_with("0X") {
        &t[2..]
    } else if t.ends_with('h') || t.ends_with('H') {
        &t[..t.len().saturating_sub(1)]
    } else {
        t
    };
    !core.is_empty() && core.chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_u64_token(s: &str, as_hex: bool) -> Option<u64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if as_hex {
        let core = if t.starts_with("0x") || t.starts_with("0X") {
            &t[2..]
        } else if t.ends_with('h') || t.ends_with('H') {
            &t[..t.len().saturating_sub(1)]
        } else {
            t
        };
        u64::from_str_radix(core, 16).ok()
    } else {
        t.parse::<u64>().ok()
    }
}

pub fn validate_all_settings(
    settings: &[BiosSetting],
    originals: &[BiosSetting],
) -> Result<(), String> {
    for (i, current) in settings.iter().enumerate() {
        let Some(original) = originals.get(i) else {
            continue;
        };
        if !setting_value_changed(current, original) {
            continue;
        }
        if !current.options.is_empty() {
            continue;
        }
        let Some(cur_raw) = current.value.as_ref() else {
            continue;
        };
        let cur_raw = cur_raw.trim();
        if cur_raw.is_empty() {
            return Err(format!(
                "Invalid value in '{}': value cannot be empty.",
                current.setup_question.trim()
            ));
        }

        let original_raw = original.value.as_deref().unwrap_or("").trim();
        let cur_inner = extract_angle_value(cur_raw);
        let orig_inner = extract_angle_value(original_raw);
        if orig_inner.is_some() != cur_inner.is_some() {
            return Err(format!(
                "Invalid format in '{}': expected angle-bracket format like <...>.",
                current.setup_question.trim()
            ));
        }
        let cur_token = cur_inner.unwrap_or(cur_raw).trim();
        let orig_token = orig_inner.unwrap_or(original_raw).trim();

        let expect_hex = is_hex_like(orig_token);
        let expect_dec = is_decimal_like(orig_token) && !expect_hex;
        if expect_hex && !is_valid_hex_token(cur_token) {
            return Err(format!(
                "Invalid value in '{}': expected hexadecimal format.",
                current.setup_question.trim()
            ));
        }
        if expect_dec && !is_decimal_like(cur_token) {
            return Err(format!(
                "Invalid value in '{}': expected decimal digits.",
                current.setup_question.trim()
            ));
        }

        if let Some(bytes) = parse_width_bytes(&current.width) {
            if bytes <= 8 {
                if let Some(v) = parse_u64_token(cur_token, expect_hex) {
                    let max = if bytes == 8 {
                        u64::MAX
                    } else {
                        (1u64 << (bytes * 8)) - 1
                    };
                    if v > max {
                        return Err(format!(
                            "Value out of range in '{}': max for Width={} is 0x{:X} ({max}).",
                            current.setup_question.trim(),
                            current.width.trim(),
                            max
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_angle() {
        assert_eq!(extract_angle_value("<deadbeef>"), Some("deadbeef"));
        assert_eq!(extract_angle_value("nope"), None);
    }
}
