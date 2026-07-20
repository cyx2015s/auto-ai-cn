use std::{
    io::Read,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use log::{LevelFilter::Debug, info};

use tanvec_ai_cn::flow::FlowConfig;
use tanvec_ai_cn::{flow, pack};

/// Tanvec AI CN — 异星工厂模组自动汉化工具
#[derive(Parser, Debug)]
#[command(name = "tanvec-ai-cn", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 运行翻译管道（自动获取更新 mod 并翻译）
    Translate {
        /// 从指定时间点开始（ISO 8601 或 "1d"/"6h"/"30m"）
        #[arg(long, value_parser = parse_since)]
        since: Option<DateTime<Utc>>,

        /// 最大处理 mod 数量
        #[arg(long)]
        limit: Option<usize>,

        /// 手动指定要翻译的 mod 名称
        #[arg(value_name = "MOD")]
        mods: Vec<String>,

        /// 从 Factorio 的 mod-list.json 中读取启用的 mod 列表
        #[arg(long)]
        mod_list: Option<PathBuf>,
    },

    /// 将缓存中的翻译打包为 1 个 Factorio mod zip
    Pack {
        /// 缓存目录 [默认: ./cache]
        #[arg(long, default_value = "./cache")]
        cache_dir: PathBuf,

        /// 输出目录 [默认: ./output]
        #[arg(long, default_value = "./output")]
        output_dir: PathBuf,

        /// 翻译包名 [默认: tanvec-ai-cn]
        #[arg(long, default_value = "tanvec-ai-cn")]
        name: String,

        /// 保护模式：覆盖原版翻译的 mod 自动标记为 .cfg.disabled
        #[arg(long)]
        protect: bool,
    },

    /// 上传 mod zip 到 Factorio Mod Portal
    Upload {
        /// 要上传的 mod zip 文件路径
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
    },
}

fn parse_since(s: &str) -> Result<DateTime<Utc>, String> {
    if let Some(rest) = s.strip_suffix('d') {
        let days: i64 = rest.parse().map_err(|_| format!("无效的天数: {}", rest))?;
        return Ok(Utc::now() - Duration::days(days));
    }
    if let Some(rest) = s.strip_suffix('h') {
        let hours: i64 = rest
            .parse()
            .map_err(|_| format!("无效的小时数: {}", rest))?;
        return Ok(Utc::now() - Duration::hours(hours));
    }
    if let Some(rest) = s.strip_suffix('m') {
        let minutes: i64 = rest
            .parse()
            .map_err(|_| format!("无效的分钟数: {}", rest))?;
        return Ok(Utc::now() - Duration::minutes(minutes));
    }
    DateTime::from_str(s).map_err(|e| format!("无法解析时间 '{}': {}", s, e))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let logger = env_logger::builder().filter_level(Debug).build();
    let level = logger.filter();
    indicatif_log_bridge::LogWrapper::new(tanvec_ai_cn::progress::MULTI.clone(), logger)
        .try_init()?;
    log::set_max_level(level);
    let cli = Cli::parse();
    match cli.command {
        // 无子命令 → 默认翻译
        None => {
            let config = FlowConfig::from_env()?;
            flow::run_translation_pipeline(config, None, None, None).await?;
        }

        Some(Command::Translate {
            since,
            limit,
            mut mods,
            mod_list,
        }) => {
            let config = FlowConfig::from_env()?;

            // 从 mod-list.json 中读取启用的 mod
            if let Some(ref path) = mod_list {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("无法读取 mod-list.json: {}", path.display()))?;
                let json: serde_json::Value =
                    serde_json::from_str(&content).context("无法解析 mod-list.json")?;
                if let Some(list) = json["mods"].as_array() {
                    for m in list {
                        if m["enabled"].as_bool().unwrap_or(true) {
                            if let Some(name) = m["name"].as_str() {
                                mods.push(name.to_string());
                            }
                        }
                    }
                }
            }

            let mod_names: Option<Vec<String>> = if mods.is_empty() { None } else { Some(mods) };
            flow::run_translation_pipeline(config, since, limit, mod_names.as_deref()).await?;
        }

        Some(Command::Pack {
            cache_dir,
            output_dir,
            name,
            protect,
        }) => {
            let base_keys = if protect {
                let game_data_path = std::env::var("FACTORIO_DATA_PATH")
                    .context("protect 模式需要设置 FACTORIO_DATA_PATH 环境变量")?;
                Some(flow::extract_base_all(Path::new(&game_data_path))?)
            } else {
                None
            };
            pack::pack_all_to_one_mod(&cache_dir, &output_dir, &name, protect, base_keys.as_ref())?;
        }

        Some(Command::Upload { file }) => {
            let api_key = std::env::var("FACTORIO_MOD_PORTAL_KEY")
                .context("请设置 FACTORIO_MOD_PORTAL_KEY 环境变量（Mod Portal 上传 API Key）")?;

            let zip_data = std::fs::read(&file)
                .with_context(|| format!("无法读取文件: {}", file.display()))?;

            // 从 zip 中的 info.json 提取 mod 名称
            let cursor = std::io::Cursor::new(&zip_data);
            let mut archive = zip::ZipArchive::new(cursor).context("无法打开 zip 文件")?;
            let mod_name = {
                let mut found_name = None;
                for i in 0..archive.len() {
                    if let Ok(mut entry) = archive.by_index(i) {
                        let name = entry.name().to_string();
                        if name.ends_with("info.json") || name == "info.json" {
                            let mut content = String::new();
                            if entry.read_to_string(&mut content).is_ok()
                                && let Ok(json) =
                                    serde_json::from_str::<serde_json::Value>(&content)
                                && let Some(n) = json["name"].as_str()
                            {
                                found_name = Some(n.to_string());
                                break;
                            }
                        }
                    }
                }
                found_name.context("无法从 zip 中提取 mod 名称（info.json 未找到或无效）")?
            };

            info!("准备上传 mod: {} (文件: {})", mod_name, file.display());

            let client = factorio_api::FactorioWebClient::anonymous();
            client.upload_mod(&api_key, &mod_name, &zip_data).await?;
            info!("上传成功: {}", mod_name);
        }
    }

    Ok(())
}
