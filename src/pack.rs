//! 翻译包打包
//!
//! 将 cache 目录中的所有翻译缓存合并打包为 1 个 Factorio mod zip。
//!
//! 输出结构：
//! ```text
//! <pack_name>_<date>/
//!   locale/zh-CN/<mod_name>.cfg   （每个源 mod 一个文件）
//!   info.json
//! ```
//!
//! 翻译包不声明对源 mod 的依赖——未安装的源 mod 的翻译条目不会被游戏加载。

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::Utc;
use log::{info, warn};
use serde_json;

use crate::translation;

/// 将 cache 目录中所有翻译缓存打包为 1 个 Factorio mod zip。
///
/// 每个源 mod 的 zh-CN 翻译合并为一个 `.cfg` 文件。
pub fn pack_all_to_one_mod(
    cache_dir: &Path,
    output_dir: &Path,
    pack_name: &str,
) -> anyhow::Result<PathBuf> {
    // 扫描缓存目录
    let mut cache_files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(cache_dir)
        .with_context(|| format!("无法读取缓存目录: {}", cache_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !path.is_file() || !name.ends_with(".json") || name.starts_with('_') {
            continue;
        }
        cache_files.push(path);
    }

    if cache_files.is_empty() {
        anyhow::bail!("缓存目录中没有翻译缓存文件");
    }

    std::fs::create_dir_all(output_dir)?;

    let today = Utc::now().format("%Y.%m.%d").to_string();
    let zip_name = format!("{}_{}", pack_name, today);
    let output_path = output_dir.join(format!("{}.zip", zip_name));

    let file = std::fs::File::create(&output_path)?;
    let mut zip_writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut packed_count = 0;

    for cache_path in &cache_files {
        let mod_name = cache_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        let content = match std::fs::read_to_string(cache_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("无法读取缓存 {}: {}", mod_name, e);
                continue;
            }
        };

        let locale_info: translation::LocaleInfo = match serde_json::from_str(&content) {
            Ok(info) => info,
            Err(e) => {
                warn!("无法解析缓存 {}: {}", mod_name, e);
                continue;
            }
        };

        let Some(target_lang) = locale_info.contents.get("zh-CN") else {
            continue;
        };

        // 合并该 mod 的所有翻译文件为一个 INI
        let mut merged_ini = ini::Ini::new();
        for (_file_name, file_content) in &target_lang.contents {
            let ini = match translation::str_to_ini(file_content) {
                Ok(ini) => ini,
                Err(e) => {
                    warn!("无法解析 {} 的翻译文件: {}", mod_name, e);
                    continue;
                }
            };
            for (section, props) in ini.iter() {
                for (k, v) in props.iter() {
                    merged_ini.with_section(section).set(k, v);
                }
            }
        }

        if merged_ini.is_empty() {
            continue;
        }

        let ini_str = translation::ini_to_str(&merged_ini)?;
        let zip_path = format!("{}/locale/zh-CN/{}.cfg", pack_name, mod_name);
        zip_writer.start_file(&zip_path, options)?;
        zip_writer.write_all(ini_str.as_bytes())?;
        packed_count += 1;
    }

    // toggle.py
    let zip_path = format!("{}/locale/zh-CN/toggly_py", pack_name);
    const TOGGLE_PY: &'static str = include_str!("templates/toggle.py");
    zip_writer.start_file(&zip_path, options)?;
    zip_writer.write_all(TOGGLE_PY.as_bytes())?;

    // info.json
    let info_json = serde_json::json!({
        "name": pack_name,
        "version": today,
        "title": "切向量的 AI 汉化",
        "factorio_version": "2.1",
        "dependencies": ["base >= 2.1"],
        "author": "tanvec",
        "description": format!("包含 {} 个模组的 AI 中文翻译。在模组官网上查看详细更新信息。", packed_count)
    });
    let info_path = format!("{}/info.json", pack_name);
    zip_writer.start_file(&info_path, options)?;
    zip_writer.write_all(serde_json::to_string_pretty(&info_json)?.as_bytes())?;

    zip_writer.finish()?;

    info!("翻译包已生成: {:?} ({} 个 mod)", output_path, packed_count);
    Ok(output_path)
}
