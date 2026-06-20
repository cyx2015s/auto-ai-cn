use crate::Config;

// ============================================================================
// API 基础 URL 常量
// ============================================================================

const MOD_API_BASE: &str = "https://mods.factorio.com/api";
const MOD_BASE: &str = "https://mods.factorio.com";
const AUTH_BASE: &str = "https://auth.factorio.com";
const THUMBNAIL_BASE: &str = "https://assets-mod.factorio.com";

// ============================================================================
// 客户端
// ============================================================================

#[derive(Debug, Clone)]
pub struct FactorioWebClient {
    pub client: reqwest::Client,
    pub config: Config,
}

// ============================================================================
// API 类型定义 — 全部按 Factorio Wiki / Mod_portal_API 的规格编写
// ============================================================================

/// 分页信息
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Pagination {
    /// 模组总数
    pub count: u64,
    /// 当前页码（1-based）
    pub page: u64,
    /// 总页数
    pub page_count: u64,
    /// 每页模组数
    pub page_size: u64,
    /// 关联页面的链接
    pub links: PageLinks,
}

/// 分页链接 — 字段值为 `null` 时表示不存在对应页面
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PageLinks {
    pub first: Option<String>,
    pub last: Option<String>,
    pub next: Option<String>,
    pub prev: Option<String>,
}

/// 模组列表响应（`/api/mods` 及 `/api/search` 的返回值）
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ModListResponse {
    pub pagination: Pagination,
    pub results: Vec<ResultEntry>,
}

/// 搜索结果响应 — 结构同 ModListResponse
pub type SearchResponse = ModListResponse;

/// 单个模组的条目。
///
/// 根据端点的不同，某些字段可能为 `None`：
///
/// | 端点                     | 字段覆盖                                               |
/// |--------------------------|--------------------------------------------------------|
/// | `/api/mods`              | 基本字段 + `latest_release`，无 `releases`              |
/// | `/api/mods/{name}`       | 基本字段 + `releases`，无 `latest_release`              |
/// | `/api/mods/{name}/full`  | 完整字段                                               |
///
/// 因此所有非"必定存在"的字段都使用 `Option<T>`。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ResultEntry {
    // ---- 所有端点都返回的字段 ----
    pub name: String,
    pub title: String,
    pub owner: String,
    pub summary: String,
    pub downloads_count: u64,

    // ---- 列表端点返回 ----
    #[serde(default)]
    pub latest_release: Option<Release>,

    // ---- 单模组 / 完整端点返回 ----
    #[serde(default)]
    pub releases: Option<Vec<Release>>,

    // ---- 完整端点额外字段 ----
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub deprecated: Option<bool>,
    #[serde(default)]
    pub deprecated_reason: Option<Vec<String>>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub github_path: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<Tag>>,
    #[serde(default)]
    pub license: Option<Vec<License>>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub last_highlighted_at: Option<String>,

    // ---- 缩略图 ----
    #[serde(default)]
    pub thumbnail: Option<String>,
}

impl ResultEntry {
    /// 拼接缩略图的完整 URL，若无缩略图返回 `None`
    pub fn thumbnail_url(&self) -> Option<String> {
        self.thumbnail
            .as_ref()
            .map(|t| format!("{THUMBNAIL_BASE}{t}"))
    }

    /// 拼接 GitHub 完整 URL（仅当 `github_path` 有值时才有意义）
    pub fn github_url(&self) -> Option<String> {
        self.github_path
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| format!("https://github.com/{p}"))
    }
}

/// 模组发布版本信息
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Release {
    pub download_url: String,
    pub file_name: String,
    /// `info.json` 内容摘要
    pub info_json: InfoJson,
    pub released_at: String,
    pub version: String,
    pub sha1: String,
    /// 空间时代功能标志（可选）
    #[serde(default)]
    pub feature_flags: Option<Vec<String>>,
}

/// `info.json` 文件摘要
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct InfoJson {
    pub factorio_version: String,
    /// 仅在 `/full` 端点返回
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
}

/// 许可证
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct License {
    pub description: String,
    pub id: String,
    pub name: String,
    pub title: String,
    pub url: String,
}

/// 标签 — 兼容字符串和 `{"name": "..."}` 两种格式
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum Tag {
    Name(String),
    Named { name: String },
}

impl Tag {
    pub fn name(&self) -> &str {
        match self {
            Tag::Name(s) => s.as_str(),
            Tag::Named { name } => name.as_str(),
        }
    }
}

/// API 错误响应
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ErrorResponse {
    pub message: String,
}

// ============================================================================
// 查询参数辅助类型
// ============================================================================

/// 每页大小 — 整数或 `"max"`
#[derive(Debug, Clone)]
pub enum PageSize {
    Size(u64),
    Max,
}

impl std::fmt::Display for PageSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageSize::Size(n) => write!(f, "{n}"),
            PageSize::Max => write!(f, "max"),
        }
    }
}

/// 排序字段
#[derive(Debug, Clone)]
pub enum SortBy {
    Name,
    CreatedAt,
    UpdatedAt,
}

impl AsRef<str> for SortBy {
    fn as_ref(&self) -> &str {
        match self {
            SortBy::Name => "name",
            SortBy::CreatedAt => "created_at",
            SortBy::UpdatedAt => "updated_at",
        }
    }
}

/// 排序方向
#[derive(Debug, Clone)]
pub enum SortOrder {
    Asc,
    Desc,
}

impl AsRef<str> for SortOrder {
    fn as_ref(&self) -> &str {
        match self {
            SortOrder::Asc => "asc",
            SortOrder::Desc => "desc",
        }
    }
}

/// `/api/mods` 列表查询参数
#[derive(Debug, Clone, Default)]
pub struct ModsQuery {
    /// 是否隐藏不兼容的模组。默认 `true`
    pub hide_deprecated: Option<bool>,
    /// 页码（1-based），`page_size = "max"` 时忽略
    pub page: Option<u64>,
    /// 每页大小；默认 25
    pub page_size: Option<PageSize>,
    /// 排序字段；默认 `name`
    pub sort: Option<SortBy>,
    /// 排序方向；默认 `desc`
    pub sort_order: Option<SortOrder>,
    /// 按模组名称过滤
    pub namelist: Option<Vec<String>>,
    /// Factorio 游戏版本号（如 `"2.0.76"`）
    pub version: Option<String>,
}

impl ModsQuery {
    /// 将查询参数挂载到 RequestBuilder 上
    fn apply(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(v) = self.hide_deprecated {
            let s = v.to_string();
            req = req.query(&[("hide_deprecated", s.as_str())]);
        }
        if let Some(v) = self.page {
            let s = v.to_string();
            req = req.query(&[("page", s.as_str())]);
        }
        if let Some(ref v) = self.page_size {
            let s = v.to_string();
            req = req.query(&[("page_size", s.as_str())]);
        }
        if let Some(ref v) = self.sort {
            req = req.query(&[("sort", v.as_ref())]);
        }
        if let Some(ref v) = self.sort_order {
            req = req.query(&[("sort_order", v.as_ref())]);
        }
        if let Some(ref v) = self.namelist {
            for name in v {
                req = req.query(&[("namelist", name.as_str())]);
            }
        }
        if let Some(ref v) = self.version {
            req = req.query(&[("version", v.as_str())]);
        }
        req
    }
}

/// 搜索请求体（`POST /api/search`）
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchQuery {
    /// Factorio 游戏版本号（必填）
    pub version: String,
    /// 搜索关键词（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// 排序方式
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    /// 页码
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    /// 每页大小
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u64>,
}

// ============================================================================
// 构造函数（登录 / 预填充）
// ============================================================================

impl FactorioWebClient {
    /// 使用用户名和密码登录，获取 token
    pub async fn login(user: String, password: String) -> anyhow::Result<Self> {
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{AUTH_BASE}/api-login"))
            .form(&[
                ("username", user.as_str()),
                ("password", password.as_str()),
                ("api_version", "6"),
            ])
            .send()
            .await?;
        let body: serde_json::Value = response.json().await?;

        if let Some(error) = body.get("error") {
            let message = body
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            Err(anyhow::anyhow!("登录失败: {} - {}", error, message))
        } else if let (Some(token), Some(username)) = (body.get("token"), body.get("username")) {
            Ok(Self {
                client,
                config: Config {
                    user: username.as_str().unwrap_or("").to_string(),
                    token: token.as_str().unwrap_or("").to_string(),
                },
            })
        } else {
            Err(anyhow::anyhow!("接收到未知的响应格式: {}", body))
        }
    }

    /// 使用已有的 username 和 token 创建客户端（跳过登录）
    pub async fn prefilled(user: String, token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            config: Config { user, token },
        }
    }
}

// ============================================================================
// Mod Portal API 方法
// ============================================================================

impl FactorioWebClient {
    // ------------------------------------------------------------------
    // GET /api/mods — 获取模组列表
    // ------------------------------------------------------------------

    /// 获取模组列表。
    ///
    /// 可通过 `ModsQuery` 控制分页、排序、过滤、版本。
    pub async fn list_mods(&self, query: Option<&ModsQuery>) -> anyhow::Result<ModListResponse> {
        let default_query = ModsQuery::default();
        let q = query.unwrap_or(&default_query);
        let req = self.client.get(format!("{MOD_API_BASE}/mods"));
        let resp = q.apply(req).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("API 错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("API 错误 ({}): {}", status, text));
        }
        let body: ModListResponse = resp.json().await?;
        Ok(body)
    }

    /// 获取所有模组（通过 `page_size = "max"`）
    pub async fn all_mods(&self) -> anyhow::Result<ModListResponse> {
        let query = ModsQuery {
            page_size: Some(PageSize::Max),
            ..Default::default()
        };
        self.list_mods(Some(&query)).await
    }

    /// 按名称精确查找模组列表
    pub async fn mods_by_names(&self, names: &[&str]) -> anyhow::Result<ModListResponse> {
        let query = ModsQuery {
            namelist: Some(names.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        };
        self.list_mods(Some(&query)).await
    }

    // ------------------------------------------------------------------
    // GET /api/mods/{name} — 获取模组简短信息
    // ------------------------------------------------------------------

    /// 获取指定模组的简短信息（含 `releases`）。
    pub async fn get_mod(&self, mod_name: &str) -> anyhow::Result<ResultEntry> {
        let url = format!("{MOD_API_BASE}/mods/{mod_name}");
        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("API 错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("API 错误 ({}): {}", status, text));
        }
        let body: ResultEntry = resp.json().await?;
        Ok(body)
    }

    // ------------------------------------------------------------------
    // GET /api/mods/{name}/full — 获取模组完整信息
    // ------------------------------------------------------------------

    /// 获取指定模组的完整信息（含 `changelog`, `description`, `tags`, `license` 等）。
    pub async fn get_mod_full(&self, mod_name: &str) -> anyhow::Result<ResultEntry> {
        let url = format!("{MOD_API_BASE}/mods/{mod_name}/full");
        let resp = self.client.get(&url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("API 错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("API 错误 ({}): {}", status, text));
        }
        let body: ResultEntry = resp.json().await?;
        Ok(body)
    }

    // ------------------------------------------------------------------
    // POST /api/search — 搜索模组
    // ------------------------------------------------------------------

    /// 搜索模组。
    ///
    /// 必填参数：`version`（如 `"2.0.76"`）。
    /// 可选参数：`query`（搜索关键词）、`order`、`page`、`page_size`。
    pub async fn search_mods(&self, search: &SearchQuery) -> anyhow::Result<SearchResponse> {
        let url = format!("{MOD_API_BASE}/search");
        let resp = self
            .client
            .post(&url)
            .json(search)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("API 错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("API 错误 ({}): {}", status, text));
        }
        let body: SearchResponse = resp.json().await?;
        Ok(body)
    }

    // ------------------------------------------------------------------
    // 下载模组
    // ------------------------------------------------------------------

    /// 根据 `Release::download_url` 下载模组文件。
    ///
    /// 返回原始字节，请按 `Release::file_name` 保存。
    pub async fn download_mod(&self, download_url: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("{MOD_BASE}{download_url}");
        let resp = self
            .client
            .get(&url)
            .query(&[
                ("username", self.config.user.as_str()),
                ("token", self.config.token.as_str()),
            ])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("下载错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("下载错误 ({}): {}", status, text));
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    }

    /// 下载指定 release 的模组文件（快捷方法）。
    pub async fn download_release(&self, release: &Release) -> anyhow::Result<Vec<u8>> {
        self.download_mod(&release.download_url).await
    }

    /// 将下载的模组保存到本地文件
    pub async fn download_and_save(
        &self,
        download_url: &str,
        file_path: &std::path::Path,
    ) -> anyhow::Result<()> {
        let data = self.download_mod(download_url).await?;
        tokio::fs::write(file_path, &data).await?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // GET /api/bookmarks — 获取收藏的模组列表
    // ------------------------------------------------------------------

    /// 获取已认证用户的模组书签列表（返回模组名称数组）。
    pub async fn get_bookmarks(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{MOD_API_BASE}/bookmarks");
        let resp = self
            .client
            .get(&url)
            .query(&[
                ("username", self.config.user.as_str()),
                ("token", self.config.token.as_str()),
            ])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("API 错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("API 错误 ({}): {}", status, text));
        }
        let body: Vec<String> = resp.json().await?;
        Ok(body)
    }

    // ------------------------------------------------------------------
    // POST /api/bookmarks/toggle — 切换书签
    // ------------------------------------------------------------------

    /// 收藏或取消收藏指定模组。
    ///
    /// - `state = true` 表示收藏
    /// - `state = false` 表示取消收藏
    pub async fn toggle_bookmark(&self, mod_name: &str, state: bool) -> anyhow::Result<()> {
        let state_str = if state { "on" } else { "off" };
        let url = format!("{MOD_API_BASE}/bookmarks/toggle");
        let resp = self
            .client
            .post(&url)
            .form(&[
                ("username", self.config.user.as_str()),
                ("token", self.config.token.as_str()),
                ("mod", mod_name),
                ("state", state_str),
            ])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("API 错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("API 错误 ({}): {}", status, text));
        }
        Ok(())
    }

    /// 收藏指定模组
    pub async fn bookmark(&self, mod_name: &str) -> anyhow::Result<()> {
        self.toggle_bookmark(mod_name, true).await
    }

    /// 取消收藏指定模组
    pub async fn unbookmark(&self, mod_name: &str) -> anyhow::Result<()> {
        self.toggle_bookmark(mod_name, false).await
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn get_credentials() -> (String, String) {
        dotenvy::dotenv().ok();
        let username = dotenvy::var("FACTORIO_USERNAME").expect("FACTORIO_USERNAME must be set");
        let password = dotenvy::var("FACTORIO_PASSWORD").expect("FACTORIO_PASSWORD must be set");
        (username, password)
    }

    #[tokio::test]
    async fn test_login() {
        let (username, password) = get_credentials();
        match FactorioWebClient::login(username, password).await {
            Ok(client) => {
                println!("登录测试成功!");
                println!("用户: {}", client.config.user);
                println!("Token: {}", client.config.token);
            }
            Err(e) => {
                eprintln!("登录测试失败: {:?}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_list_mods() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let query = ModsQuery {
            page_size: Some(PageSize::Size(3)),
            page: Some(1),
            hide_deprecated: Some(true),
            version: Some("2.0.76".to_string()),
            ..Default::default()
        };
        let resp = client.list_mods(Some(&query)).await?;
        println!("总数: {}, 当前页大小: {}", resp.pagination.count, resp.pagination.page_size);
        for m in &resp.results {
            println!("  - {} by {} (downloads: {})", m.name, m.owner, m.downloads_count);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_get_mod() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let m = client.get_mod("rso-mod").await?;
        println!("模组: {} — {}", m.name, m.title);
        if let Some(ref releases) = m.releases {
            println!("发布版本数: {}", releases.len());
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_get_mod_full() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let m = client.get_mod_full("rso-mod").await?;
        println!("模组: {} — {}", m.name, m.title);
        println!("描述: {:?}", m.description);
        println!("标签: {:?}", m.tags);
        println!("许可证: {:?}", m.license);
        Ok(())
    }

    #[tokio::test]
    async fn test_bookmarks() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let bookmarks = client.get_bookmarks().await?;
        println!("收藏的模组: {:?}", bookmarks);
        Ok(())
    }
}
