use std::{path::PathBuf, str::FromStr};

use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use log::LevelFilter::Debug;

use crate::flow::FlowConfig;

pub mod flow;
pub mod pack;
pub mod persistent;
pub mod translation;

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
    },
}

fn parse_since(s: &str) -> Result<DateTime<Utc>, String> {
    if let Some(rest) = s.strip_suffix('d') {
        let days: i64 = rest.parse().map_err(|_| format!("无效的天数: {}", rest))?;
        return Ok(Utc::now() - Duration::days(days));
    }
    if let Some(rest) = s.strip_suffix('h') {
        let hours: i64 = rest.parse().map_err(|_| format!("无效的小时数: {}", rest))?;
        return Ok(Utc::now() - Duration::hours(hours));
    }
    if let Some(rest) = s.strip_suffix('m') {
        let minutes: i64 = rest.parse().map_err(|_| format!("无效的分钟数: {}", rest))?;
        return Ok(Utc::now() - Duration::minutes(minutes));
    }
    DateTime::from_str(s).map_err(|e| format!("无法解析时间 '{}': {}", s, e))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    env_logger::builder().filter_level(Debug).init();

    let cli = Cli::parse();

    match cli.command {
        // 无子命令 → 默认翻译
        None => {
            let config = FlowConfig::from_env()?;
            flow::run_translation_pipeline(config, None, None, None).await?;
        }

        Some(Command::Translate { since, limit, mods }) => {
            let config = FlowConfig::from_env()?;
            let mod_names: Option<Vec<String>> = if mods.is_empty() {
                None
            } else {
                Some(mods)
            };
            flow::run_translation_pipeline(config, since, limit, mod_names.as_deref()).await?;
        }

        Some(Command::Pack {
            cache_dir,
            output_dir,
            name,
        }) => {
            pack::pack_all_to_one_mod(&cache_dir, &output_dir, &name)?;
        }
    }

    Ok(())
}
