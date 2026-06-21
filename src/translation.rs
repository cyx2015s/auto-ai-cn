#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LocaleInfo {
    /// lang code -> lang info
    pub contents: indexmap::IndexMap<String, LangInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LangInfo {
    /// file name -> ini content
    pub contents: indexmap::IndexMap<String, String>,
}

pub fn as_ini(s: &str) -> ini::Ini {
    ini::Ini::load_from_str(s).unwrap()
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

/// 根据参考文件 A，已有文件 B，将差分文件 C 合并到 B 上，同时删除 B 中被 A 移除的内容
pub fn merge_ini(reference: &ini::Ini, old: &ini::Ini, diff: &ini::Ini) -> ini::Ini {
    let mut merged = ini::Ini::new();
    for (sec, prop) in reference.iter() {
        let old_prop = old.section(sec);
        let diff_prop = diff.section(sec);
        for (k, v) in prop.iter() {
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
        let old = as_ini(
            r#"[section1]
key1=value1
key3=value3
[section2]
keyA=valueA
"#,
        );
        let new = as_ini(
            r#"[section1]
key1=value1
key2=value2
[section3]
keyB=valueB
"#,
        );
        let diff = diff_ini(&old, &new);
        // Add assertions to verify the expected behavior of the diff
        dbg!(diff);
    }

    #[test]
    fn test_merge_ini() {
        let reference = as_ini(
            r#"[section1]
key1=value1
key2=value2
[section3]
keyB=valueB
"#,
        );
        let old = as_ini(
            r#"[section1]
key1=value1
key3=value3
[section2]
keyA=valueA
"#,
        );
        let mut diff = diff_ini(&old, &reference);
        diff.with_section(Some("section3")).set("keyB", "22222");
        diff.with_section(Some("section1")).set("key1", "22333");
        // Add assertions to verify the expected behavior of the diff
        dbg!(&diff);
        let merged = merge_ini(&reference, &old, &diff);
        // Add assertions to verify the expected behavior of the merged result
        dbg!(&merged);
    }
}
