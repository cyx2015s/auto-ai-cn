
use chrono::{Days, Utc};
use log::LevelFilter::Debug;

use crate::flow::FlowConfig;


pub mod flow;
pub mod persistent;
pub mod translation;
#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    env_logger::builder().filter_level(Debug).init();
    log::info!("Starting Tanvec AI CN...");
    flow::run_translation_pipeline(FlowConfig::from_env().unwrap(), Some(Utc::now()- Days::new(1)), None).await.unwrap();
}
