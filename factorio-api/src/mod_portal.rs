use chrono::{DateTime, Utc};

// ============================================================================
// API 基础 URL 常量
// ============================================================================

const MOD_API_BASE: &str = "https://mods.factorio.com/api";
const MOD_BASE: &str = "https://mods.factorio.com";
const AUTH_BASE: &str = "https://auth.factorio.com";
const THUMBNAIL_BASE: &str = "https://assets-mod.factorio.com";
const MOD_API_V2_BASE: &str = "https://mods.factorio.com/api/v2";

// ============================================================================
// 客户端
// ============================================================================

/// API 认证凭据
#[derive(Debug, Clone)]
pub struct Config {
    pub user: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct FactorioWebClient {
    pub client: reqwest::Client,
    pub config: Config,
}

// ============================================================================
// API 类型定义 — 全部按 Factorio Wiki / Mod_portal_API 的规格编写
// ============================================================================

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
// /api/search 端点专用类型
// ============================================================================

/// 搜索分页信息（无 links 字段，与 `/api/mods` 的 Pagination 不同）
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SearchPagination {
    /// 模组总数
    pub count: u64,
    /// 当前页码（1-based）
    pub page: u64,
    /// 总页数
    pub page_count: u64,
    /// 每页模组数
    pub page_size: u64,
}

/// 搜索结果高亮字段
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct HighlightedFields {
    pub name: String,
    pub owner: String,
    pub summary: String,
    pub title: String,
}

/// `/api/search` 返回的单条结果。
///
/// **注意**：`latest_release` 为十六进制哈希字符串，
/// 不同于 `ResultEntry.latest_release`（完整的 `Release` 对象）。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SearchResultEntry {
    pub name: String,
    pub title: String,
    pub owner: String,
    pub summary: String,
    pub downloads_count: u64,

    /// 最新发布版本的十六进制哈希值，可用于拼接下载 URL
    #[serde(default)]
    pub latest_release: Option<String>,

    /// 最新发布版本号（如 `"0.5.2"`）
    #[serde(default)]
    pub latest_release_version: Option<String>,

    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub deprecated: Option<bool>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub requires_space_age: Option<bool>,
    #[serde(default)]
    pub thumbnail: Option<String>,

    /// 高亮字段（搜索词匹配部分）
    #[serde(default)]
    pub highlighted_fields: Option<HighlightedFields>,

    /// 兼容的 Factorio 版本列表
    #[serde(default)]
    pub factorio_versions: Option<Vec<String>>,

    /// 标签
    #[serde(default, deserialize_with = "deserialize_optional_one_or_many")]
    pub tags: Option<Vec<Tag>>,
}

impl SearchResultEntry {
    /// 将搜索结果条目转换为 `ResultEntry`。
    ///
    /// 从哈希字符串构造合成的 `Release` 对象（下载 URL = `/download/{name}/{hash}`）。
    /// 其余未在搜索响应中出现的字段置为 `None`。
    /// 如需完整信息（changelog、description、license 等），应随后调用 `get_mod()`。
    pub fn into_result_entry(self) -> ResultEntry {
        let synthetic_release = self.latest_release.as_ref().map(|hash| Release {
            download_url: format!("/download/{}/{}", self.name, hash),
            file_name: String::new(),
            info_json: InfoJson {
                factorio_version: String::new(),
                dependencies: None,
            },
            released_at: String::new(),
            version: self.latest_release_version.clone().unwrap_or_default(),
            sha1: String::new(),
            feature_flags: None,
        });

        ResultEntry {
            name: self.name,
            title: self.title,
            owner: self.owner,
            summary: self.summary,
            downloads_count: self.downloads_count,
            latest_release: synthetic_release,
            releases: None,
            category: self.category,
            changelog: None,
            created_at: self.created_at,
            description: None,
            deprecated: self.deprecated,
            deprecated_reason: None,
            source_url: self.source_url,
            github_path: None,
            homepage: None,
            tags: self.tags,
            license: None,
            updated_at: self.updated_at,
            last_highlighted_at: None,
            thumbnail: self.thumbnail,
        }
    }

    /// 拼接缩略图的完整 URL
    pub fn thumbnail_url(&self) -> Option<String> {
        self.thumbnail
            .as_ref()
            .map(|t| format!("{THUMBNAIL_BASE}{t}"))
    }
}

/// `/api/search` 完整响应体
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SearchResponse {
    /// 各分类命中数
    #[serde(default)]
    pub category_hits: Option<serde_json::Value>,
    /// 扩展包命中数
    #[serde(default)]
    pub expansion_hits: Option<serde_json::Value>,
    /// 标签命中数
    #[serde(default)]
    pub tag_hits: Option<serde_json::Value>,
    /// 分页信息
    pub pagination: SearchPagination,
    /// 搜索结果列表
    pub results: Vec<SearchResultEntry>,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 解析 Factorio API 返回的 ISO 8601 / RFC 3339 时间字符串
fn parse_iso8601(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| {
        // chrono 的 DateTime::parse_from_rfc3339 比较严格，
        // 但 Factorio 返回的格式有多种变体，用宽松解析
        dbg!(DateTime::parse_from_rfc3339(s)
            .ok())
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
// 查询参数类型
// ============================================================================

/// 搜索请求体（`POST /api/search`）
///
/// `username` 和 `token` 由 `search_mods` 方法从 `self.config` 自动注入，
/// 调用方无需设置。
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct SearchQuery {
    /// Factorio 游戏版本号（必填）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// 搜索关键词（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// 排序字段（如 `"last_updated_at"`、`"created_at"`、`"name"`、`"downloads_count"`）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_attribute: Option<String>,
    /// 排序方向：`"asc"` 或 `"desc"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    /// 页码（1-based）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    /// 每页大小
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u64>,
}

/// 内部使用的搜索请求体 —— 包含认证信息（由 `search_mods` 自动注入）
#[derive(Debug, Clone, serde::Serialize)]
struct SearchRequestBody<'a> {
    #[serde(flatten)]
    query: &'a SearchQuery,
    username: &'a str,
    token: &'a str,
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

    /// 创建无需 Factorio 认证的匿名客户端（用于上传等场景）
    pub fn anonymous() -> Self {
        Self {
            client: reqwest::Client::new(),
            config: Config {
                user: String::new(),
                token: String::new(),
            },
        }
    }
}

// ============================================================================
// Mod Portal API 方法
// ============================================================================

impl FactorioWebClient {
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

    /// 搜索模组（`POST /api/search`）。
    ///
    /// `username` 和 `token` 由 `self.config` 自动注入，调用方无需在 `SearchQuery` 中设置。
    pub async fn search_mods(&self, search: &SearchQuery) -> anyhow::Result<SearchResponse> {
        let url = format!("{MOD_API_BASE}/search");
        let body = SearchRequestBody {
            query: search,
            username: &self.config.user,
            token: &self.config.token,
        };
        let resp = self.client.post(&url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<ErrorResponse>(&text) {
                return Err(anyhow::anyhow!("API 错误 ({}): {}", status, err.message));
            }
            return Err(anyhow::anyhow!("API 错误 ({}): {}", status, text));
        }
        let body: SearchResponse = resp.json().await?;
        dbg!(&body.results[0..2]);
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

    /// 获取下载请求的原始 Response（调用方自行流式读取，可用于显示进度）。
    pub async fn download_mod_response(
        &self,
        download_url: &str,
    ) -> anyhow::Result<reqwest::Response> {
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
        Ok(resp)
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
    // 高级组合方法 — 基于 /api/search 的多步编排
    // ------------------------------------------------------------------

    /// 按名称列表获取模组信息。
    ///
    /// 并发调用 `get_mod()`，每个名称独立查询，返回精确匹配结果。
    /// 查询失败的模组会被静默跳过（只记录警告日志）。
    pub async fn mods_by_names(&self, names: &[&str]) -> anyhow::Result<Vec<ResultEntry>> {
        use futures_util::StreamExt;
        use futures_util::stream::FuturesUnordered;

        let futures: FuturesUnordered<_> = names
            .iter()
            .map(|name| async move {
                let result = self.get_mod(name).await;
                (name, result)
            })
            .collect();

        let mut results = Vec::with_capacity(names.len());
        let mut stream = futures;
        while let Some((name, result)) = stream.next().await {
            match result {
                Ok(entry) => results.push(entry),
                Err(e) => {
                    log::warn!("获取模组 {} 信息失败: {}", name, e);
                }
            }
        }

        Ok(results)
    }

    /// 获取最近更新的前 `limit` 个模组。
    ///
    /// 使用 `/api/search` 端点，按 `last_updated_at` 降序排列。
    pub async fn get_recently_updated(
        &self,
        limit: u64,
        version: &str,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let query = SearchQuery {
            version: Some(version.to_string()),
            sort_attribute: Some("last_updated_at".to_string()),
            order: Some("desc".to_string()),
            page_size: Some(limit),
            page: Some(1),
            query: None,
        };
        let resp = self.search_mods(&query).await?;
        let entries: Vec<ResultEntry> = resp
            .results
            .into_iter()
            .map(|sr| sr.into_result_entry())
            .collect();
        Ok(entries)
    }

    /// 获取最新创建的前 `limit` 个模组。
    ///
    /// 使用 `/api/search` 端点，按 `created_at` 降序排列。
    pub async fn get_newest_mods(
        &self,
        limit: u64,
        version: &str,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let query = SearchQuery {
            version: Some(version.to_string()),
            sort_attribute: Some("created_at".to_string()),
            order: Some("desc".to_string()),
            page_size: Some(limit),
            page: Some(1),
            query: None,
        };
        let resp = self.search_mods(&query).await?;
        let entries: Vec<ResultEntry> = resp
            .results
            .into_iter()
            .map(|sr| sr.into_result_entry())
            .collect();
        Ok(entries)
    }

    /// 获取在指定时间之后更新的所有模组。
    ///
    /// 策略：使用 `/api/search` 按 `last_updated_at` 降序分页遍历，
    /// 用 `updated_at` 做客户端过滤。
    /// 当一整页条目都不满足时间条件时提前终止（因为降序排列）。
    ///
    /// `page_size` 控制每页获取数量（默认 100，上限 100）。
    /// `max_size` 限制返回的模组总数（None = 不限制）。
    pub async fn get_mods_updated_since(
        &self,
        since: DateTime<Utc>,
        version: &str,
        page_size: Option<u64>,
        max_size: Option<u64>,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let ps = page_size.unwrap_or(100).min(100);
        let mut all_results: Vec<ResultEntry> = Vec::new();
        let mut page: u64 = 1;

        loop {
            let query = SearchQuery {
                version: Some(version.to_string()),
                sort_attribute: Some("last_updated_at".to_string()),
                order: Some("desc".to_string()),
                page_size: Some(ps),
                page: Some(page),
                query: None,
            };
            let resp = self.search_mods(&query).await?;
            let results = resp.results;

            if results.is_empty() {
                break;
            }

            let mut page_has_match = false;
            for entry in results {
                dbg!(&entry.updated_at);
                dbg!(parse_iso8601(entry.updated_at.as_deref()));
                dbg!(&since);
                let updated_after =
                    parse_iso8601(entry.updated_at.as_deref()).is_some_and(|dt| dt >= since);

                if updated_after {
                    page_has_match = true;
                    all_results.push(entry.into_result_entry());
                }

                if all_results.len() as u64 >= max_size.unwrap_or(u64::MAX) {
                    break;
                }
            }

            // 降序排列，整页无匹配则后续页也不可能匹配
            if !page_has_match {
                break;
            }

            if resp.pagination.page >= resp.pagination.page_count {
                break;
            }

            page += 1;
        }

        Ok(all_results)
    }

    /// 从已有条目列表中过滤出 `latest_release.released_at >= since` 的模组。
    ///
    /// **注意**：仅适用于从 `get_mod()` / `get_mod_full()` 获取的完整条目
    /// （其 `latest_release` 包含真实的 `released_at`）。
    /// 对于从 `get_mods_updated_since()` 返回的条目（其 `latest_release` 为合成对象），
    /// `released_at_dt()` 会返回 `None`，应改用 `updated_at_dt()`。
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
    /// 先并发调用 `get_mod()` 获取完整信息（含 `releases`），再客户端按时间过滤。
    pub async fn find_updated_since_by_names(
        &self,
        names: &[&str],
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<ResultEntry>> {
        let entries = self.mods_by_names(names).await?;
        let filtered: Vec<ResultEntry> = entries
            .into_iter()
            .filter(|e| e.released_at_dt().is_some_and(|dt| dt >= since))
            .collect();
        Ok(filtered)
    }

    // ------------------------------------------------------------------
    // POST /api/v2/mods/releases/init_upload — 上传 mod
    // ------------------------------------------------------------------

    /// 上传 mod zip 到 Mod Portal（使用 API Key 认证）。
    ///
    /// 流程：
    /// 1. 调用 `init_upload` 获取上传 URL
    /// 2. 如果 mod 尚未发布，回退到 `init_publish`
    /// 3. POST zip 文件到上传 URL
    ///
    /// `api_key` 从 https://factorio.com/profile 获取的个人 API Key。
    pub async fn upload_mod(
        &self,
        api_key: &str,
        mod_name: &str,
        zip_data: &[u8],
    ) -> anyhow::Result<()> {
        let auth_header = format!("Bearer {}", api_key);

        // Step 1: init_upload
        let init_url = format!("{MOD_API_V2_BASE}/mods/releases/init_upload");
        let (upload_url, is_publish) = match self
            .client
            .post(&init_url)
            .header("Authorization", &auth_header)
            .form(&[("mod", mod_name)])
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let json: serde_json::Value = resp.json().await?;
                (json["upload_url"].as_str().unwrap_or("").to_string(), false)
            }
            _ => {
                // Step 2: 回退 — mod 可能尚未发布，使用 init_publish
                let publish_url = format!("{MOD_API_V2_BASE}/mods/init_publish");
                let resp = self
                    .client
                    .post(&publish_url)
                    .header("Authorization", &auth_header)
                    .form(&[("mod", mod_name)])
                    .send()
                    .await?;
                if !resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    anyhow::bail!("init_publish 失败: {}", text);
                }
                let json: serde_json::Value = resp.json().await?;
                (json["upload_url"].as_str().unwrap_or("").to_string(), true)
            }
        };

        if upload_url.is_empty() {
            anyhow::bail!(
                "{} 失败：未返回 upload_url",
                if is_publish {
                    "init_publish"
                } else {
                    "init_upload"
                }
            );
        }

        // Step 3: 上传 zip
        let file_part = reqwest::multipart::Part::bytes(zip_data.to_vec())
            .file_name(format!("{}.zip", mod_name))
            .mime_str("application/zip")?;

        let form = reqwest::multipart::Form::new().part("file", file_part);
        let resp = self.client.post(&upload_url).multipart(form).send().await?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("上传失败: {}", text)
        }
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

    #[test]
    fn test_time_parsing() {
        let s1 = Some("2026-07-23T23:31:36.025000");
        dbg!(parse_iso8601(s1));
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
    async fn test_search_mods() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let query = SearchQuery {
            version: Some("2.1".to_string()),
            page_size: Some(5),
            page: Some(1),
            sort_attribute: Some("last_updated_at".to_string()),
            order: Some("desc".to_string()),
            query: None,
        };
        let resp = client.search_mods(&query).await?;
        println!(
            "总数: {}, 当前页大小: {}",
            resp.pagination.count, resp.pagination.page_size
        );
        for m in &resp.results {
            println!(
                "  - {} by {} (downloads: {}, category: {:?}, latest_release: {:?})",
                m.name, m.owner, m.downloads_count, m.category, m.latest_release
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
    async fn test_mods_updated_since() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let since = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(1))
            .unwrap();
        dbg!(since);
        let mods = client
            .get_mods_updated_since(since, "2.1", Some(50), Some(50))
            .await?;
        println!("{} 之后更新的模组:", since);
        for m in mods {
            println!("  - {} (updated: {:?})", m.name, m.updated_at);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_mods_by_names() -> anyhow::Result<()> {
        let (username, password) = get_credentials();
        let client = FactorioWebClient::login(username, password).await?;

        let results = client.mods_by_names(&["rso-mod", "flib"]).await?;
        println!("找到 {} 个模组:", results.len());
        for m in &results {
            println!(
                "  - {} by {} (releases: {:?})",
                m.name,
                m.owner,
                m.releases.as_ref().map(|r| r.len())
            );
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
