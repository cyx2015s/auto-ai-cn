use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use log::LevelFilter::Debug;

use crate::flow::FlowConfig;

pub mod flow;
pub mod persistent;
pub mod translation;

/// Tanvec AI CN — 异星工厂模组自动汉化工具
#[derive(Parser, Debug)]
#[command(name = "tanvec-ai-cn", version, about)]
struct Cli {
    /// 从指定时间点开始获取更新的 mod（ISO 8601 格式或相对时间如 "1d", "6h"）
    #[arg(long, value_parser = parse_since)]
    since: Option<DateTime<Utc>>,

    /// 最大处理 mod 数量
    #[arg(long)]
    limit: Option<usize>,

    /// 手动指定要翻译的 mod 名称（可多次指定，不指定则自动获取更新的 mod）
    #[arg(value_name = "MOD")]
    mods: Vec<String>,
}

fn parse_since(s: &str) -> Result<DateTime<Utc>, String> {
    // 尝试解析相对时间，如 "1d", "6h", "30m"
    if let Some(rest) = s.strip_suffix('d') {
        let days: i64 = rest.parse().map_err(|_| format!("无效的天数: {}", rest))?;
        let duration = Duration::days(days);
        return Ok(Utc::now() - duration);
    }
    if let Some(rest) = s.strip_suffix('h') {
        let hours: i64 = rest
            .parse()
            .map_err(|_| format!("无效的小时数: {}", rest))?;
        let duration = Duration::hours(hours);
        return Ok(Utc::now() - duration);
    }
    if let Some(rest) = s.strip_suffix('m') {
        let minutes: i64 = rest
            .parse()
            .map_err(|_| format!("无效的分钟数: {}", rest))?;
        let duration = Duration::minutes(minutes);
        return Ok(Utc::now() - duration);
    }

    // 尝试解析 ISO 8601
    DateTime::from_str(s).map_err(|e| format!("无法解析时间 '{}': {}", s, e))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    env_logger::builder().filter_level(Debug).init();

    let cli = Cli::parse();

    let config = FlowConfig::from_env()?;

    let mod_names: Option<Vec<String>> = if cli.mods.is_empty() {
        None
    } else {
        Some(cli.mods)
    };

    flow::run_translation_pipeline(config, cli.since, cli.limit, mod_names.as_deref()).await?;

    Ok(())
}
