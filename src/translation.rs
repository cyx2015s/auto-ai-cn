#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LocaleInfo {
    /// lang code -> lang info
    pub contents: indexmap::IndexMap<String, LangInfo>,
    /// 版本号
    pub version: String,
    /// 从 info.json 提取的模组依赖列表（已去除版本约束和可选前缀，排除 base/core 与冲突项）
    #[serde(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LangInfo {
    /// file name -> ini content
    pub contents: indexmap::IndexMap<String, String>,
}

/// 将 key 中的未转义 `:` 转义为 `\:`，避免被 `rust-ini` 误解析为键值分隔符。
fn escape_colons_in_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut backslashes = 0usize;
    for ch in key.chars() {
        if ch == '\\' {
            backslashes += 1;
            out.push(ch);
        } else if ch == ':' {
            if backslashes.is_multiple_of(2) {
                out.push('\\');
            }
            out.push(':');
            backslashes = 0;
        } else {
            backslashes = 0;
            out.push(ch);
        }
    }
    out
}

/// 将 Mode 2（`section` + `entries`）直接提交的译文中的转义序列还原，
/// 避免经过 `ini_to_str` 时被二次转义为 `\\n`。
pub fn unescape_submitted_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn str_to_ini(s: &str) -> anyhow::Result<ini::Ini> {
    let s = s.strip_prefix('\u{feff}').unwrap_or(s);

    // 清洗：
    // 1. 去掉每行前导空白，修复缩进的 # 注释被误认为 key 的问题
    // 2. 转义 key 中的 `:`，避免 rust-ini 将 `:` 视作键值分隔符
    // 3. 忽略既不是注释/section 也不包含 `=` 的游离行
    let cleaned: String = s
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                String::new()
            } else if trimmed.starts_with('#') || trimmed.starts_with(';') {
                trimmed.to_string()
            } else if trimmed.starts_with('[') {
                if trimmed.trim_end().ends_with(']') && !trimmed.contains('=') {
                    trimmed.to_string()
                } else {
                    String::new()
                }
            } else if let Some(eq_pos) = trimmed.find('=') {
                let key_part = &trimmed[..eq_pos];
                let val_part = &trimmed[eq_pos..];
                if key_part.trim().is_empty() {
                    String::new()
                } else {
                    format!("{}{}", escape_colons_in_key(key_part), val_part)
                }
            } else {
                String::new()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    ini::Ini::load_from_str_opt(
        &cleaned,
        ini::ParseOption {
            enabled_quote: false,
            ..Default::default()
        },
    )
    .map_err(|e| anyhow::anyhow!("INI 解析失败: {}", e))
}

pub fn ini_to_str(ini: &ini::Ini) -> anyhow::Result<String> {
    let mut output = Vec::new();
    ini.write_to_opt(
        &mut output,
        ini::WriteOption {
            line_separator: ini::LineSeparator::CR,
            ..Default::default()
        },
    )
    .map_err(|e| anyhow::anyhow!("INI 写入失败: {}", e))?;
    String::from_utf8(output).map_err(|e| anyhow::anyhow!("INI 非 UTF-8: {}", e))
}

/// 返回新 ini 中新出现的内容，不包括被移除的内容
pub fn diff_ini(old: &ini::Ini, new: &ini::Ini) -> ini::Ini {
    let mut diff = ini::Ini::new();
    for (sec, prop) in new.iter() {
        let old_prop = old.section(sec);
        for (k, v) in prop.iter() {
            if old_prop
                .and_then(|p| p.get(k))
                .map(|ov| ov == v)
                .unwrap_or(false)
            {
                continue;
            }
            diff.with_section(sec).set(k, v);
        }
    }
    diff
}

pub fn diff_ini_keys_only(old: &ini::Ini, new: &ini::Ini) -> ini::Ini {
    let mut diff = ini::Ini::new();
    for (sec, prop) in new.iter() {
        let old_prop = old.section(sec);
        for (k, v) in prop.iter() {
            if old_prop.and_then(|p| p.get(k)).is_some() {
                continue;
            }
            diff.with_section(sec).set(k, v);
        }
    }
    diff
}

/// 根据参考文件 A，已有文件 B，将差分文件 C 合并到 B 上，同时删除 B 中被 A 移除的内容
pub fn merge_ini(reference: &ini::Ini, old: &ini::Ini, diff: &ini::Ini) -> ini::Ini {
    let mut merged = ini::Ini::new();
    for (sec, prop) in reference.iter() {
        let old_prop = old.section(sec);
        let diff_prop = diff.section(sec);
        for (k, _) in prop.iter() {
            if let Some(diff_v) = diff_prop.and_then(|p| p.get(k)) {
                merged.with_section(sec).set(k, diff_v);
            } else if let Some(old_v) = old_prop.and_then(|p| p.get(k)) {
                merged.with_section(sec).set(k, old_v);
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_ini() {
        let old = str_to_ini(
            r#"[section1]
key1=value1
key3=value3
[section2]
keyA=valueA
"#,
        )
        .unwrap();
        let new = str_to_ini(
            r#"[section1]
key1=value1
key2=value2
[section3]
keyB=valueB
"#,
        )
        .unwrap();
        let diff = diff_ini(&old, &new);
        // Add assertions to verify the expected behavior of the diff
        dbg!(diff);
    }

    #[test]
    fn test_merge_ini() {
        let reference = str_to_ini(
            r#"[section1]
key1=value1
key2=value2
[section3]
keyB=valueB
"#,
        )
        .unwrap();
        let old = str_to_ini(
            r#"[section1]
key1=value1
key3=value3
[section2]
keyA=valueA
"#,
        )
        .unwrap();
        let mut diff = diff_ini(&old, &reference);
        diff.with_section(Some("section3")).set("keyB", "22222");
        diff.with_section(Some("section1")).set("key1", "22333");
        // Add assertions to verify the expected behavior of the diff
        dbg!(&diff);
        let merged = merge_ini(&reference, &old, &diff);
        // Add assertions to verify the expected behavior of the merged result
        dbg!(&merged);
    }

    #[test]
    fn test_colon_in_key_roundtrip() {
        let input = "[ai-glossary]\nSpace Age: Hard Mode=太空时代：硬核模式\nArtisanal Reskins: Library=工匠重绘：核心库\n";
        let ini = str_to_ini(input).unwrap();
        let sec = ini.section(Some("ai-glossary")).unwrap();
        assert_eq!(sec.get("Space Age: Hard Mode"), Some("太空时代：硬核模式"));
        assert_eq!(sec.get("Artisanal Reskins: Library"), Some("工匠重绘：核心库"));

        let out = ini_to_str(&ini).unwrap();
        let reparsed = str_to_ini(&out).unwrap();
        let reparsed_sec = reparsed.section(Some("ai-glossary")).unwrap();
        assert_eq!(
            reparsed_sec.get("Space Age: Hard Mode"),
            Some("太空时代：硬核模式")
        );
        assert_eq!(
            reparsed_sec.get("Artisanal Reskins: Library"),
            Some("工匠重绘：核心库")
        );
    }

    #[test]
    fn test_stray_line_without_equals() {
        let input = "[section]\nkey1=value1\nstray line with: colon and no equals\n[color=red]broken tag\nkey2=value2\n";
        let ini = str_to_ini(input).unwrap();
        let sec = ini.section(Some("section")).unwrap();
        assert_eq!(sec.len(), 2);
        assert_eq!(sec.get("key1"), Some("value1"));
        assert_eq!(sec.get("key2"), Some("value2"));
    }

    #[test]
    fn test_unescape_submitted_value() {
        let raw = r"第一行\n第二行\t缩进\\反斜杠";
        let unescaped = unescape_submitted_value(raw);
        assert_eq!(unescaped, "第一行\n第二行\t缩进\\反斜杠");

        let mut ini = ini::Ini::new();
        ini.with_section(Some("sec")).set("k", unescaped);
        let serialized = ini_to_str(&ini).unwrap();
        assert_eq!(serialized, "[sec]\nk=第一行\\n第二行\\t缩进\\\\反斜杠\n");
    }
}
