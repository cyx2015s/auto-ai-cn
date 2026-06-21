// use chrono::{Days, Utc};
use log::LevelFilter::Debug;

use crate::flow::FlowConfig;

pub mod flow;
pub mod persistent;
pub mod translation;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    env_logger::builder().filter_level(Debug).init();
    log::info!("Starting Tanvec AI CN...");
    let config = FlowConfig::from_env()?;
    flow::run_translation_pipeline(config, None, None).await?;
    Ok(())
}
