use crate::Config;

use chrono::{DateTime, Utc};

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
    #[serde(default, deserialize_with = "deserialize_optional_one_or_many")]
    pub tags: Option<Vec<Tag>>,
    #[serde(default, deserialize_with = "deserialize_optional_one_or_many")]
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

    // ---- 时间字段 → chrono 转换 ----

    /// `created_at` 解析为 `DateTime<Utc>`
    pub fn created_at_dt(&self) -> Option<DateTime<Utc>> {
        parse_iso8601(self.created_at.as_deref())
    }

    /// `updated_at` 解析为 `DateTime<Utc>`
    pub fn updated_at_dt(&self) -> Option<DateTime<Utc>> {
        parse_iso8601(self.updated_at.as_deref())
    }

    /// `latest_release.released_at` 解析为 `DateTime<Utc>`
    pub fn released_at_dt(&self) -> Option<DateTime<Utc>> {
        self.latest_release
            .as_ref()
            .and_then(|r| parse_iso8601(Some(&r.released_at)))
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
// 辅助函数
// ============================================================================

/// 解析 Factorio API 返回的 ISO 8601 / RFC 3339 时间字符串
fn parse_iso8601(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| {
        // chrono 的 DateTime::parse_from_rfc3339 比较严格，
        // 但 Factorio 返回的格式有多种变体，用宽松解析
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .or_else(|| {
                // 回退：尝试 naive datetime + 假定 UTC
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ")
                    .ok()
                    .or_else(|| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
                    .map(|naive| DateTime::from_naive_utc_and_offset(naive, Utc))
            })
    })
}

// ============================================================================
// 辅助反序列化 — API 对单元素数组有时返回裸对象而非 `[{...}]`
// ============================================================================

/// 反序列化：接受 JSON null → `None`，单对象 → `Some(vec![obj])`，数组 → `Some(vec)`
fn deserialize_optional_one_or_many<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    use serde::Deserialize;

    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum OneOrMany<T> {
        One(T),
        Many(Vec<T>),
    }

    // `Option<OneOrMany<T>>` 将 null 反序列化为 None，值反序列化为一或多项
    match Option::<OneOrMany<T>>::deserialize(deserializer)? {
        None => Ok(None),
        Some(OneOrMany::One(item)) => Ok(Some(vec![item])),
        Some(OneOrMany::Many(items)) => Ok(Some(items)),
    }
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
        let resp = self.client.post(&url).json(search).send().await?;
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

    // ------------------------------------------------------------------
    // 高级组合方法 — 基于底层 API 的多步编排
    // ------------------------------------------------------------------

    /// 获取最近更新的前 `limit` 个模组。
    ///
    /// 通过 `sort=updated_at&sort_order=desc` 实现，服务端排序。
    /// `version` 为 Factorio 版本号（如 `"2.0.76"`），用于过滤不兼容的模组。
    pub async fn get_recently_updated(
        &self,
        limit: u64,
        version: &str,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let query = ModsQuery {
            page_size: Some(PageSize::Size(limit)),
            sort: Some(SortBy::UpdatedAt),
            sort_order: Some(SortOrder::Desc),
            hide_deprecated: Some(true),
            version: Some(version.to_string()),
            ..Default::default()
        };
        let resp = self.list_mods(Some(&query)).await?;
        Ok(resp.results)
    }

    /// 获取最新创建的前 `limit` 个模组。
    ///
    /// 通过 `sort=created_at&sort_order=desc` 实现。
    pub async fn get_newest_mods(
        &self,
        limit: u64,
        version: &str,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let query = ModsQuery {
            page_size: Some(PageSize::Size(limit)),
            sort: Some(SortBy::CreatedAt),
            sort_order: Some(SortOrder::Desc),
            hide_deprecated: Some(true),
            version: Some(version.to_string()),
            ..Default::default()
        };
        let resp = self.list_mods(Some(&query)).await?;
        Ok(resp.results)
    }

    /// 获取在指定时间之后更新的所有模组（尽力而为）。
    ///
    /// 策略：按 `updated_at` 降序分页遍历，用 `latest_release.released_at` 做
    /// 客户端过滤。当一整页条目都不满足时间条件时提前终止（因为降序排列）。
    ///
    /// **注意**：列表端点不返回 `updated_at` 字段，这里使用
    /// `latest_release.released_at` 作为近似判断依据。如果某个模组的最新发布版
    /// 是在 `since` 之前但其 `updated_at` 在 `since` 之后（如仅更新了描述），
    /// 此方法会漏掉它。需要精确结果请对结果逐个调用 `get_mod_full`。
    ///
    /// `page_size` 控制每页获取数量（默认 100）。
    pub async fn get_mods_updated_since(
        &self,
        since: DateTime<Utc>,
        version: &str,
        page_size: Option<u64>,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let ps = page_size.unwrap_or(100).min(100);
        let mut all_results: Vec<ResultEntry> = Vec::new();
        let mut page: u64 = 1;

        loop {
            let query = ModsQuery {
                page_size: Some(PageSize::Size(ps)),
                page: Some(page),
                sort: Some(SortBy::UpdatedAt),
                sort_order: Some(SortOrder::Desc),
                hide_deprecated: Some(true),
                version: Some(version.to_string()),
                ..Default::default()
            };
            let resp = self.list_mods(Some(&query)).await?;
            let results = resp.results;

            if results.is_empty() {
                break;
            }

            // 客户端过滤 + 提前终止判断
            let mut page_has_match = false;
            for entry in results {
                let released_after = entry.released_at_dt().is_some_and(|dt| dt >= since);

                if released_after {
                    page_has_match = true;
                    all_results.push(entry);
                }
            }

            // 降序排列，如果整页没有匹配项，后续页也不会有了
            if !page_has_match {
                break;
            }

            // 如果返回的条目数少于 page_size，说明已是最后一页
            if (resp.pagination.page) >= resp.pagination.page_count {
                break;
            }

            page += 1;
        }

        Ok(all_results)
    }

    /// 从已有条目列表中过滤出 `latest_release.released_at >= since` 的模组。
    ///
    /// 可用于对 `all_mods()` 或任意列表结果做二次过滤。
    pub fn filter_updated_since(
        entries: &[ResultEntry],
        since: DateTime<Utc>,
    ) -> Vec<&ResultEntry> {
        entries
            .iter()
            .filter(|e| e.released_at_dt().is_some_and(|dt| dt >= since))
            .collect()
    }

    /// 按名称搜索并过滤出在指定时间之后更新的模组。
    ///
    /// 先用 `mods_by_names` 批量获取，再客户端按时间过滤。
    pub async fn find_updated_since_by_names(
        &self,
        names: &[&str],
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let resp = self.mods_by_names(names).await?;
        let filtered: Vec<ResultEntry> = resp
            .results
            .into_iter()
            .filter(|e| e.released_at_dt().is_some_and(|dt| dt >= since))
            .collect();
        Ok(filtered)
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
        println!(
            "总数: {}, 当前页大小: {}",
            resp.pagination.count, resp.pagination.page_size
        );
        for m in &resp.results {
            println!(
                "  - {} by {} (downloads: {})",
                m.name, m.owner, m.downloads_count
            );
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
            if releases.last().is_some() {
                println!("最新版本: {:?}", releases[releases.len() - 1]);
            }
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

    #[tokio::test]
    async fn test_mods_since() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let since = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(1))
            .unwrap();
        dbg!(since);
        let mods = client
            .get_mods_updated_since(since, "2.0.76", Some(50))
            .await?;
        println!("{} 之后更新的模组:", since);
        for m in mods {
            println!("  - {} (latest release: {:?})", m.name, m.latest_release);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_download_mod() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let mod_info = client.get_mod("rso-mod").await?;
        if let Some(ref release) = mod_info.latest_release {
            println!("下载模组: {} (version {})", mod_info.name, release.version);
            let data = client.download_release(release).await?;
            println!("下载完成，文件大小: {} bytes", data.len());
            let file = std::path::Path::new(&release.file_name);
            tokio::fs::write(file, &data).await?;
            println!("文件已保存到: {:?}", file);
        } else if let Some(ref releases) = mod_info.releases
            && let Some(latest) = releases.last()
        {
            println!("下载模组: {} (version {})", mod_info.name, latest.version);
            let data = client.download_release(latest).await?;
            println!("下载完成，文件大小: {} bytes", data.len());
            let file = std::path::Path::new(&latest.file_name);
            tokio::fs::write(file, &data).await?;
            println!("文件已保存到: {:?}", file);
        } else {
            println!("模组没有发布版本，无法下载");
        }
        Ok(())
    }
}
