//! 批量替换模组缓存中的中文翻译文本
//!
//! 只处理缓存 JSON 中 `zh-CN` 的翻译内容，绝不触碰 `en` 原文，
//! 避免在 VSCode 等编辑器中搜索替换时误伤原始文本。
//!
//! 替换只作用于 INI 的 **value**（`=` 右侧）：内容先按 INI 解析，
//! 遍历每个 section 下的键值对，仅对 value 应用正则替换，再序列化回写。
//! key、section 名、注释不会被匹配。
//!
//! 用法：
//! ```bash
//! # 直接执行
//! batch-replace '旧文本' '新文本'
//!
//! # 先预览再确认
//! batch-replace '旧文本' '新文本' --confirm
//!
//! # 指定缓存目录
//! batch-replace '旧文本' '新文本' --cache-dir /path/to/cache
//! ```
//!
//! 正则语法与 Rust `regex` crate 一致（不支持 lookaround / backreference，
//! 支持 `$1` 捕获组引用）。

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use regex::Regex;
use tanvec_ai_cn::translation::{self, LocaleInfo};

/// 批量替换模组中文翻译文本
#[derive(Parser, Debug)]
#[command(name = "batch-replace", about = "批量替换模组缓存中的中文翻译文本")]
struct Cli {
    /// 要匹配的正则表达式（作用于 INI 键值对的 value）
    pattern: String,

    /// 替换为的文本（支持 $1 捕获组引用）
    replacement: String,

    /// 缓存目录 [默认: ./cache]
    #[arg(long, default_value = "./cache")]
    cache_dir: PathBuf,

    /// 替换前先预览并确认（交互式，逐文件确认）
    #[arg(long)]
    confirm: bool,
}

/// 单个 mod 的替换计划
struct ModPlan {
    path: PathBuf,
    mod_name: String,
    /// (文件, 替换前完整内容, 替换后完整内容, 变更示例)
    changes: Vec<(String, String, String, Vec<String>)>,
}

/// 对单个 INI 文本做 value 级正则替换，返回 (替换后文本, 变更示例列表)
fn replace_ini_values(
    content: &str,
    re: &Regex,
    replacement: &str,
) -> anyhow::Result<(String, Vec<String>)> {
    let mut ini = translation::str_to_ini(content)?;

    // 先收集所有需要替换的 (section, key, 新值)
    let mut replacements: Vec<(Option<String>, String, String)> = Vec::new();
    let mut examples: Vec<String> = Vec::new();

    for (section, props) in ini.iter() {
        let sec = section.map(|s| s.to_string());
        for (key, value) in props.iter() {
            if re.is_match(value) {
                let new_value = re.replace_all(value, replacement).to_string();
                let sec_str = sec.clone().unwrap_or_default();
                examples.push(format!("[{}] {}: {} → {}", sec_str, key, value, new_value));
                replacements.push((sec.clone(), key.to_string(), new_value));
            }
        }
    }

    if replacements.is_empty() {
        return Ok((content.to_string(), Vec::new()));
    }

    // 应用替换
    for (section, key, new_value) in &replacements {
        ini.with_section(section.as_deref()).set(key, new_value);
    }

    let new_content = translation::ini_to_str(&ini)?;
    Ok((new_content, examples))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let re =
        Regex::new(&cli.pattern).with_context(|| format!("正则表达式无效: {}", cli.pattern))?;

    // 扫描缓存目录
    let mut cache_files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&cli.cache_dir)
        .with_context(|| format!("无法读取缓存目录: {}", cli.cache_dir.display()))?
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
        anyhow::bail!(
            "缓存目录中没有找到翻译缓存文件: {}",
            cli.cache_dir.display()
        );
    }

    // 构建替换计划（不写入）
    let mut plans: Vec<ModPlan> = Vec::new();
    let mut total_changes = 0usize;

    for path in &cache_files {
        let mod_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取缓存: {}", path.display()))?;
        let locale: LocaleInfo = serde_json::from_str(&content)
            .with_context(|| format!("无法解析缓存: {}", path.display()))?;

        let Some(zh) = locale.contents.get("zh-CN") else {
            continue;
        };

        let mut changes: Vec<(String, String, String, Vec<String>)> = Vec::new();
        for (file_name, file_content) in &zh.contents {
            let (new_content, examples) = replace_ini_values(file_content, &re, &cli.replacement)?;
            if new_content != *file_content {
                changes.push((
                    file_name.clone(),
                    file_content.clone(),
                    new_content,
                    examples,
                ));
                total_changes += 1;
            }
        }

        if changes.is_empty() {
            continue;
        }

        plans.push(ModPlan {
            path: path.clone(),
            mod_name,
            changes,
        });
    }

    if plans.is_empty() {
        println!("没有找到匹配的翻译（zh-CN 中无匹配文本）");
        return Ok(());
    }

    // 预览
    println!(
        "共 {} 个 mod / {} 个文件将受影响，替换为: {}",
        plans.len(),
        total_changes,
        cli.replacement
    );
    for plan in &plans {
        println!("  {} ({} 个文件)", plan.mod_name, plan.changes.len());
        for (i, (file, _, _, examples)) in plan.changes.iter().enumerate().take(3) {
            println!("    [{}] {}:", i + 1, file);
            for ex in examples.iter().take(2) {
                println!("      {}", ex);
            }
        }
        if plan.changes.len() > 3 {
            println!("    ... 还有 {} 个文件", plan.changes.len() - 3);
        }
    }

    // 确认模式
    if cli.confirm {
        print!("确认执行替换？(y/N) ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("已取消");
            return Ok(());
        }
    }

    // 执行：重新解析每个文件，替换 value 并保存
    for plan in &plans {
        let content = std::fs::read_to_string(&plan.path)
            .with_context(|| format!("无法重新读取缓存: {}", plan.path.display()))?;
        let mut locale: LocaleInfo = serde_json::from_str(&content)
            .with_context(|| format!("无法重新解析缓存: {}", plan.path.display()))?;

        let Some(zh) = locale.contents.get_mut("zh-CN") else {
            continue;
        };
        for (file_name, _, _, _) in &plan.changes {
            if let Some(file_content) = zh.contents.get_mut(file_name) {
                let (new_content, _) = replace_ini_values(file_content, &re, &cli.replacement)?;
                *file_content = new_content;
            }
        }

        let json = serde_json::to_string_pretty(&locale)?;
        std::fs::write(&plan.path, json)
            .with_context(|| format!("无法写入缓存: {}", plan.path.display()))?;
        println!("✓ {} 已更新", plan.mod_name);
    }

    println!("完成，共替换 {} 个文件", total_changes);
    Ok(())
}
