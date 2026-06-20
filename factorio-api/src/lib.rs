mod mod_portal;
pub use mod_portal::*;

/// API 认证凭据
#[derive(Debug, Clone)]
pub struct Config {
    pub user: String,
    pub token: String,
}
