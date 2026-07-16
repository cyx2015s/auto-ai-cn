//! gavin-server — 从 tanvec-ai-cn 翻译包中提取特定 mod 的翻译，重打包为独立 mod。
//!
//! 放到 Factorio mods 目录运行，读取 mod-list.json，从已安装的 tanvec-ai-cn zip
//! 中提取启用 mod 的 locale 文件，打包为新的 Factorio mod zip。
//!
//! ## 依赖标记
//!
//! - `+` 前缀：推荐依赖（所有覆盖到的 mod）
//! - `!` 前缀：冲突依赖（tanvec-ai-cn，避免重复加载翻译）

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::{FixedOffset, Utc};
use clap::Parser;
use log::info;

/// 从 tanvec-ai-cn 翻译包提取翻译并重打包
#[derive(Parser)]
#[command(name = "gavin-server", version, about)]
struct Cli {
    /// 输出 mod 名 [默认: gavin-server]
    #[arg(long, default_value = "gavin-server")]
    name: String,

    /// 作者名 [默认: gavin]
    #[arg(long, default_value = "gavin")]
    author: String,

    /// 版本号 [默认: UTC+8 当天日期]
    #[arg(long)]
    version: Option<String>,

    /// mod-list.json 路径 [默认: ./mod-list.json]
    #[arg(long, default_value = "./mod-list.json")]
    mod_list: PathBuf,

    /// 输出目录 [默认: .]
    #[arg(long, default_value = ".")]
    output_dir: PathBuf,

    /// 可选：上传到 Mod Portal 的 API Key
    #[arg(long)]
    upload_key: Option<String>,

    /// tanvec-ai-cn 模组包路径（zip 文件），默认自动查找
    #[arg(long)]
    pack_path: Option<PathBuf>,
}

/// 查找 mods 目录下以给定前缀开头的 zip 文件，返回最新版本
fn find_pack(mods_dir: &Path, prefix: &str) -> anyhow::Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(mods_dir)
        .with_context(|| format!("无法读取 mods 目录: {}", mods_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(prefix) && name_str.ends_with(".zip") {
            candidates.push(entry.path());
        }
    }
    if candidates.is_empty() {
        anyhow::bail!("未找到以 '{}' 开头的 zip 文件", prefix);
    }
    // 按修改时间排序，取最新
    candidates.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    Ok(candidates.last().unwrap().clone())
}

/// 从 zip 中提取 locale/zh-CN/*.cfg 文件内容
fn extract_locale_files(zip_path: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let data = std::fs::read(zip_path)?;
    let cursor = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let mut files: BTreeMap<String, String> = BTreeMap::new();

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        // 匹配 <root>/locale/zh-CN/<mod>.cfg 或 <root>/locale/zh-CN/<mod>.cfg.disabled
        if let Some(rest) = name.strip_prefix("tanvec-ai-cn/locale/zh-CN/") {
            let mod_name = rest
                .strip_suffix(".cfg")
                .or_else(|| rest.strip_suffix(".cfg.disabled"))
                .unwrap_or(rest)
                .to_string();
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            files.insert(mod_name, content);
        }
    }

    Ok(files)
}

/// 读取 mod-list.json，返回启用的 mod 名称集合
fn load_enabled_mods(path: &Path) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;
    let mut mods = Vec::new();
    if let Some(list) = json["mods"].as_array() {
        for m in list {
            if m["enabled"].as_bool().unwrap_or(true)
                && let Some(name) = m["name"].as_str()
            {
                mods.push(name.to_string());
            }
        }
    }
    Ok(mods)
}

const IGNORED_MODS: &[&'static str] = &[
    "base",
    "space-age",
    "quality",
    "recycler",
    "elevated-rails",
    "tanvec-ai-cn",
];
fn pause() {
    use std::io::Read;
    println!();
    println!("按回车键退出...");
    std::io::stdin().read_exact(&mut [0]).ok();
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    if let Err(e) = run().await {
        eprintln!("\n❌ 错误: {:#}", e);
        pause();
        std::process::exit(1);
    }
    pause();
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    println!("========================================");
    println!("  gavin-server — 翻译包重打包工具");
    println!("========================================");
    println!();

    // 版本号默认为 UTC+8 当天日期
    let version = cli.version.unwrap_or_else(|| {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        Utc::now()
            .with_timezone(&offset)
            .format("%Y.%m.%d")
            .to_string()
    });
    println!("📋 配置信息:");
    println!("   输出模组名: {}_{}", cli.name, version);
    println!("   作者: {}", cli.author);
    println!("   mod-list.json: {}", cli.mod_list.display());
    println!();

    // 确定 tanvec-ai-cn zip 路径
    println!("🔍 正在查找翻译包...");
    let pack_path = match cli.pack_path {
        Some(ref p) => {
            println!("   使用指定路径: {}", p.display());
            p.clone()
        }
        None => {
            let path = find_pack(Path::new("."), "tanvec-ai-cn")?;
            println!("   自动找到: {}", path.display());
            path
        }
    };
    info!("使用翻译包: {}", pack_path.display());

    // 提取翻译文件
    println!("📦 正在解压翻译包...");
    let locale_files = extract_locale_files(&pack_path)?;
    println!("   翻译包中包含 {} 个 mod 的翻译", locale_files.len());

    // 读取启用的 mod 列表
    println!("📖 正在读取 mod-list.json...");
    let enabled_mods = load_enabled_mods(&cli.mod_list)?;
    println!("   共 {} 个启用的 mod", enabled_mods.len());
    info!("mod-list.json 中启用了 {} 个 mod", enabled_mods.len());

    // 统计
    let mut matched: Vec<String> = Vec::new();
    let mut unmatched: Vec<String> = Vec::new();

    for mod_name in &enabled_mods {
        if IGNORED_MODS.contains(&mod_name.as_str()) {
            info!("忽略 mod: {}", mod_name);
            continue;
        }
        if mod_name == &cli.name {
            info!("忽略输出 mod 自身: {}", mod_name);
            // 避免自引用
            continue;
        }
        if locale_files.contains_key(mod_name) {
            matched.push(mod_name.clone());
        } else {
            unmatched.push(mod_name.clone());
        }
    }

    if !unmatched.is_empty() {
        println!();
        println!(
            "⚠️  以下 {} 个启用 mod 在翻译包中未找到翻译:",
            unmatched.len()
        );
        for m in &unmatched {
            println!("   - {}", m);
        }
    }

    if matched.is_empty() {
        anyhow::bail!("没有找到任何启用 mod 的翻译文件。请确保已安装 tanvec-ai-cn 翻译包。");
    }

    println!();
    println!("✅ 匹配到 {} 个 mod 的翻译，正在打包...", matched.len());

    // 打包
    let output_name = format!("{}_{}", cli.name, version);
    let output_zip = cli.output_dir.join(format!("{}.zip", output_name));
    let file = std::fs::File::create(&output_zip)?;
    let mut zip_writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for mod_name in &matched {
        if let Some(content) = locale_files.get(mod_name) {
            let zip_path = format!("{}/locale/zh-CN/{}.cfg", output_name, mod_name);
            zip_writer.start_file(&zip_path, options)?;
            zip_writer.write_all(content.as_bytes())?;
        }
    }

    // 构建 info.json
    let mut dependencies: Vec<String> = vec!["base >= 2.1".to_string()];
    for m in &matched {
        dependencies.push(format!("+ {}", m));
    }
    dependencies.push("! tanvec-ai-cn".to_string());

    let info_json = serde_json::json!({
        "name": cli.name,
        "title": format!("{} 的专用汉化整合包", cli.author),
        "author": cli.author,
        "version": version,
        "description": format!("包含 {} 个模组的 AI 中文翻译。由 gavin-server 从 tanvec-ai-cn 翻译包中自动提取。", matched.len()),
        "factorio_version": "2.1",
        "dependencies": dependencies,
    });

    let info_path = format!("{}/info.json", output_name);
    zip_writer.start_file(&info_path, options)?;
    zip_writer.write_all(serde_json::to_string_pretty(&info_json)?.as_bytes())?;

    zip_writer.finish()?;

    // 结果展示
    println!();
    println!("========================================");
    println!("  🎉 打包完成！");
    println!("========================================");
    println!("  文件: {}", output_zip.display());
    println!("  模组名: {}_{}", cli.name, version);
    println!("  包含 {} 个 mod 的翻译:", matched.len());
    for m in &matched {
        println!("    ✓ {}", m);
    }
    if !unmatched.is_empty() {
        println!("  {} 个 mod 无翻译（已跳过）:", unmatched.len());
        for m in &unmatched {
            println!("    ✗ {}", m);
        }
    }
    println!();

    // 可选上传
    if let Some(ref api_key) = cli.upload_key {
        println!("📤 正在上传到 Mod Portal...");
        let client = factorio_api::FactorioWebClient::anonymous();
        let zip_data = std::fs::read(&output_zip)?;
        client.upload_mod(api_key, &cli.name, &zip_data).await?;
        println!("✅ 上传成功！模组名: {}", cli.name);
    } else {
        println!("💡 提示: 使用 --upload-key 参数可自动上传到 Mod Portal");
    }

    Ok(())
}
