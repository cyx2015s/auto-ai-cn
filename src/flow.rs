//! 翻译流程
//!
//! ## 运行所需的外部文件
//!
//! 运行时需要以下外部文本文件，路径可通过环境变量配置：
//!
//! | 文件用途               | 默认路径                      | 环境变量                  | 说明 |
//! |------------------------|-------------------------------|---------------------------|------|
//! | 原版游戏中英文对照表   | `data/base_game_locale.ini`   | `TANVEC_BASE_LOCALE`      | 供 LLM 参考的官方译名对照，INI/CFG 格式，每行 `key=中文翻译` |
//! | 翻译系统提示词         | `data/system_prompt.txt`      | `TANVEC_SYSTEM_PROMPT`    | 给 LLM 的翻译指导（角色设定、规则、注意事项）|
//! | 翻译缓存目录           | `./cache`                     | `TANVEC_CACHE_DIR`        | 按模组名称命名的 JSON 缓存文件存放目录 |
//!
//! ## 其他环境变量
//!
//! | 变量名                | 说明 |
//! |-----------------------|------|
//! | `DEEPSEEK_KEY`        | DeepSeek API Key |
//! | `FACTORIO_USERNAME`   | Factorio 官网用户名 |
//! | `FACTORIO_PASSWORD`   | Factorio 官网密码（也支持 `FACTORIO_TOKEN` 跳过登录）|
//! | `FACTORIO_VERSION`    | 游戏版本号，默认 `"2.0.76"` |
//!
//! ## 流程概述
//!
//! 1. 获取自上次运行以来更新的所有 mod
//! 2. 对于每个 mod：
//!    - 获取 mod 的所有翻译文件（下载 zip → 解压 → 收集 `locale/` 下的 `.cfg` 文件）
//!    - 如果本地有缓存，则获取差异
//!    - 将预先构筑好的提示词送入 LLM
//!    - 将 LLM 的输出与本地缓存进行合并，生成新的翻译
//!    - 保存新的翻译文件到本地缓存
//!
//! ## 已知限制
//!
//! - 可能无法妥善处理文件重命名的情况
//! - 约定：通过 function calling 让 LLM 提交翻译
//!
//! ## Function Calling 约定
//!
//! LLM 通过调用 `submit_translation` 函数提交翻译结果：
//!
//! ```json
//! {
//!   "name": "submit_translation",
//!   "arguments": {
//!     "file_name": "base.cfg",
//!     "section": "entity-name",
//!     "entries": [
//!       {"key": "iron-plate", "original": "Iron plate", "translation": "铁板"},
//!       {"key": "copper-plate", "original": "Copper plate", "translation": "铜板"}
//!     ]
//!   }
//! }
//! ```

use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use deepseek_api::{
    CompletionsRequestBuilder, DeepSeekClientBuilder, RequestBuilder,
    request::{Function, MessageRequest, ToolMessageRequest, ToolObject, ToolType},
    response::FinishReason,
};
use factorio_api::FactorioWebClient;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};

use crate::{
    persistent::persistent_via_file,
    translation::{self, LangInfo, LocaleInfo},
};

// ══════════════════════════════════════════════════════════════════════════════
// 类型别名
// ══════════════════════════════════════════════════════════════════════════════

/// Mod 名称和版本的组合键
pub type ModKey = (String, String);

/// 缓存的翻译数据：key = mod 名称，value = LocaleInfo
type TranslationCache = BTreeMap<String, LocaleInfo>;

// ══════════════════════════════════════════════════════════════════════════════
// 管道状态（上次运行时间）
// ══════════════════════════════════════════════════════════════════════════════

/// 持久化的管道状态，记录上次运行时间
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineState {
    /// 上次运行时间（UTC）
    pub last_run: DateTime<Utc>,
}

impl Default for PipelineState {
    fn default() -> Self {
        Self {
            last_run: DateTime::UNIX_EPOCH,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// 配置
// ══════════════════════════════════════════════════════════════════════════════

/// 翻译管道的运行时配置
#[derive(Debug, Clone)]
pub struct FlowConfig {
    /// Factorio 游戏版本号
    pub game_version: String,
    /// 缓存目录路径
    pub cache_dir: PathBuf,
    /// 原版游戏中英文对照表文件路径
    pub base_locale_path: PathBuf,
    /// 翻译系统提示词文件路径
    pub system_prompt_path: PathBuf,
    /// API 请求间隔（毫秒），避免触发速率限制
    pub api_delay_ms: u64,
    /// DeepSeek API Key
    pub deepseek_key: String,
}

impl FlowConfig {
    /// 从环境变量构建配置，缺失的变量使用默认值
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        let game_version =
            std::env::var("FACTORIO_VERSION").unwrap_or_else(|_| "2.0.76".to_string());

        let cache_dir = std::env::var("TANVEC_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./cache"));

        let base_locale_path = std::env::var("TANVEC_BASE_LOCALE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/base_game_locale.ini"));

        let system_prompt_path = std::env::var("TANVEC_SYSTEM_PROMPT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/system_prompt.txt"));

        let deepseek_key = std::env::var("DEEPSEEK_KEY").context("环境变量 DEEPSEEK_KEY 未设置")?;

        Ok(Self {
            game_version,
            cache_dir,
            base_locale_path,
            system_prompt_path,
            api_delay_ms: 2000,
            deepseek_key,
        })
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 1: 从 mod zip 中提取翻译文件
// ══════════════════════════════════════════════════════════════════════════════

/// 从 zip 字节数据中提取 locale 目录下的翻译文件。
///
/// 返回 `LocaleInfo`，其中 key 为语言代码（如 `"zh-CN"`, `"en"`），
/// value 为该语言下所有 `.cfg` 文件的内容。
///
/// Factorio mod 的翻译文件位于 `locale/<语言代码>/<文件名>.cfg`。
pub fn extract_locale_from_zip(zip_bytes: &[u8]) -> anyhow::Result<LocaleInfo> {
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("无法打开 zip 文件")?;

    let mut locale_info = LocaleInfo {
        contents: indexmap::IndexMap::new(),
        version: String::new(),
    };

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;

        // 只处理大小合理的文件，跳过目录
        let name = file.name().to_string();
        if name.ends_with('/') || file.size() > 5 * 1024 * 1024 {
            continue;
        }

        // 解析 locale/<lang>/<filename>.cfg 路径
        if let Some(rest) = name.strip_prefix("locale/") {
            let parts: Vec<&str> = rest.splitn(2, '/').collect();
            if parts.len() == 2 {
                let lang_code = parts[0].to_string();
                let file_name = parts[1].to_string();

                // 只处理 .cfg 文件
                if !file_name.ends_with(".cfg") {
                    continue;
                }

                let mut content = String::new();
                file.read_to_string(&mut content)?;

                locale_info
                    .contents
                    .entry(lang_code)
                    .or_insert_with(|| LangInfo {
                        contents: indexmap::IndexMap::new(),
                    })
                    .contents
                    .insert(file_name, content);
            }
        }
    }

    // 如果 zip 中包含 info.json，提取版本号
    if let Ok(mut info_file) = archive.by_name("info.json") {
        let mut info_content = String::new();
        if info_file.read_to_string(&mut info_content).is_ok()
            && let Ok(info) = serde_json::from_str::<serde_json::Value>(&info_content) {
                locale_info.version = info
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
    }

    Ok(locale_info)
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 2: 加载外部参考文件
// ══════════════════════════════════════════════════════════════════════════════

/// 加载原版游戏中英文对照表（INI 格式）
pub fn load_base_locale(path: &Path) -> anyhow::Result<ini::Ini> {
    if !path.exists() {
        warn!("原版对照表文件不存在: {:?}，将以空对照表继续", path);
        return Ok(ini::Ini::new());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("无法读取原版对照表文件: {:?}", path))?;
    Ok(translation::str_to_ini(&content))
}

/// 加载翻译系统提示词
pub fn load_system_prompt(path: &Path) -> anyhow::Result<String> {
    if !path.exists() {
        warn!("系统提示词文件不存在: {:?}，将使用默认提示词", path);
        return Ok(DEFAULT_SYSTEM_PROMPT.to_string());
    }
    std::fs::read_to_string(path).with_context(|| format!("无法读取系统提示词文件: {:?}", path))
}

/// 默认系统提示词（当外部文件不存在时使用）
const DEFAULT_SYSTEM_PROMPT: &str = r#"你是一个专业的中文本地化翻译专家，专门负责将 Factorio 模组的英文文本翻译成简体中文。

翻译规则：
1. 保持游戏术语的一致性，参考提供的原版对照表
2. 对于技术类文本，使用准确、简洁的中文表达
3. 保留原文中的格式标记（如 __1__、[item=xxx] 等占位符）
4. 不要翻译专有名词、代码标识符
5. 使用简体中文书写风格

请通过调用 submit_translation 函数提交你的翻译结果。"#;

// ══════════════════════════════════════════════════════════════════════════════
// Step 3: 构建 Function Calling 的工具定义
// ══════════════════════════════════════════════════════════════════════════════

/// 创建翻译提交的 ToolObject 列表。
///
/// JSON Schema 参数通过 `serde_json::from_str` 解析，类型由
/// `Function.parameters` 字段的实际类型自动推断。
fn make_translation_tools() -> Vec<ToolObject> {
    let parameters = serde_json::from_str(
        r#"{
        "type": "object",
        "properties": {
            "file_name": {
                "type": "string",
                "description": "翻译文件名，例如 'base.cfg'"
            },
            "section": {
                "type": "string",
                "description": "INI section 名称，例如 'entity-name'、'item-name' 等"
            },
            "entries": {
                "type": "array",
                "description": "该文件该 section 下的所有翻译条目",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": {
                            "type": "string",
                            "description": "翻译键（key）"
                        },
                        "original": {
                            "type": "string",
                            "description": "英文原文"
                        },
                        "translation": {
                            "type": "string",
                            "description": "中文翻译"
                        }
                    },
                    "required": ["key", "original", "translation"]
                }
            }
        },
        "required": ["file_name", "section", "entries"]
    }"#,
    )
    .expect("内置 JSON Schema 格式错误");

    vec![ToolObject {
        tool_type: ToolType::Function,
        function: Function {
            name: "submit_translation".to_string(),
            description: "提交一个文件一个 section 下的所有翻译条目".to_string(),
            parameters,
        },
    }]
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 4: 构建发送给 LLM 的提示词
// ══════════════════════════════════════════════════════════════════════════════

/// 单条翻译任务（需要 LLM 翻译的键值对）
#[derive(Debug, Clone)]
pub struct TranslationEntry {
    pub file_name: String,
    pub section: String,
    pub key: String,
    pub original: String,
}

/// 构建用户提示词内容。
///
/// 包含：
/// 1. 原版游戏中英文对照表（作为参考）
/// 2. 需要翻译的内容列表
/// 3. 如果提供了上次的翻译文件，附上参考
pub fn build_user_prompt(
    base_locale: &ini::Ini,
    entries: &[TranslationEntry],
    previous_translations: Option<&LocaleInfo>,
) -> String {
    let mut prompt = String::new();

    // 原版对照表
    if !base_locale.is_empty() {
        prompt.push_str("## 原版游戏术语对照参考\n\n");
        prompt.push_str("以下为原版游戏中的常见术语翻译，请保持翻译一致性：\n\n");
        prompt.push_str("```ini\n");
        prompt.push_str(&translation::ini_to_str(base_locale));
        prompt.push_str("```\n\n");
    }

    // 上次的翻译文件（作为风格参考）
    if let Some(prev) = previous_translations
        && !prev.contents.is_empty() {
            prompt.push_str("## 该模组之前的翻译（仅供参考风格）\n\n");
            prompt.push_str("```ini\n");
            for (lang_code, lang_info) in &prev.contents {
                prompt.push_str(&format!("; --- 语言: {} ---\n", lang_code));
                for (file_name, content) in &lang_info.contents {
                    prompt.push_str(&format!("; 文件: {}\n", file_name));
                    prompt.push_str(content);
                    prompt.push('\n');
                }
            }
            prompt.push_str("```\n\n");
        }

    // 当前翻译任务
    prompt.push_str("## 当前翻译任务\n\n");
    prompt.push_str(
        "请将以下英文文本翻译为简体中文，按文件分 section 调用 submit_translation 函数提交：\n\n",
    );

    // 按文件和 section 分组展示
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<&TranslationEntry>>> = BTreeMap::new();
    for entry in entries {
        grouped
            .entry(entry.file_name.clone())
            .or_default()
            .entry(entry.section.clone())
            .or_default()
            .push(entry);
    }

    for (file_name, sections) in &grouped {
        prompt.push_str(&format!("### 文件: {}\n\n", file_name));
        for (section, sec_entries) in sections {
            prompt.push_str(&format!("#### Section: [{}]\n\n", section));
            prompt.push_str("| key | 英文原文 |\n");
            prompt.push_str("|-----|----------|\n");
            for e in sec_entries {
                prompt.push_str(&format!("| `{}` | {} |\n", e.key, e.original));
            }
            prompt.push('\n');
        }
    }

    prompt.push_str("请现在开始翻译，按文件按 section 调用 submit_translation 函数逐批提交。\n");
    prompt
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 5: LLM 交互 — 发送翻译请求并收集结果
// ══════════════════════════════════════════════════════════════════════════════

/// LLM 通过 function calling 提交的翻译条目
#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedEntry {
    pub key: String,
    pub original: String,
    pub translation: String,
}

/// LLM 提交的翻译结果（单个 function call 的数据）
#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedTranslation {
    pub file_name: String,
    pub section: String,
    pub entries: Vec<SubmittedEntry>,
}

/// 调用 LLM 获取翻译。
///
/// 使用 function calling 机制，LLM 通过多次调用 `submit_translation`
/// 分批提交翻译结果。返回合并后的 ini::Ini。
pub async fn call_llm_for_translation(
    client: &deepseek_api::DeepSeekClient,
    system_prompt: &str,
    user_prompt: &str,
) -> anyhow::Result<ini::Ini> {
    let tools = make_translation_tools();
    let mut result_ini = ini::Ini::new();
    let mut loop_count = 0;
    const MAX_LOOPS: usize = 20;

    // 将系统提示词合并到用户消息的开头
    let full_user_prompt = format!("[系统指令]\n{}\n\n---\n\n{}", system_prompt, user_prompt);

    let mut messages: Vec<MessageRequest> = vec![MessageRequest::user(&full_user_prompt)];

    while loop_count < MAX_LOOPS {
        loop_count += 1;

        let resp = CompletionsRequestBuilder::new(&messages)
            .tools(&tools)
            .do_request(client)
            .await
            .context("LLM API 请求失败")?
            .must_response();

        if resp.choices[0].finish_reason == FinishReason::ToolCalls
            && let Some(ref msg) = resp.choices[0].message {
                // 记录 assistant 的消息
                messages.push(MessageRequest::Assistant(msg.clone()));

                if let Some(ref tool_calls) = msg.tool_calls {
                    let mut has_valid_call = false;

                    for tool_call in tool_calls {
                        if tool_call.function.name != "submit_translation" {
                            warn!("LLM 调用了未知函数: {}", tool_call.function.name);
                            // 回复错误
                            messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                &format!("未知函数: {}", tool_call.function.name),
                                &tool_call.id,
                            )));
                            continue;
                        }

                        // 解析翻译数据
                        match serde_json::from_str::<SubmittedTranslation>(
                            &tool_call.function.arguments,
                        ) {
                            Ok(submitted) => {
                                debug!(
                                    "收到翻译: file={}, section={}, entries={}",
                                    submitted.file_name,
                                    submitted.section,
                                    submitted.entries.len()
                                );
                                // 将翻译结果合并到 result_ini
                                for entry in &submitted.entries {
                                    result_ini
                                        .with_section(Some(&submitted.section))
                                        .set(&entry.key, &entry.translation);
                                }
                                has_valid_call = true;

                                // 确认收到
                                messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                    &format!(
                                        "已收到 {} 的 {} 下 {} 条翻译",
                                        submitted.file_name,
                                        submitted.section,
                                        submitted.entries.len()
                                    ),
                                    &tool_call.id,
                                )));
                            }
                            Err(e) => {
                                warn!(
                                    "解析翻译数据失败: {} — 原始参数: {}",
                                    e, tool_call.function.arguments
                                );
                                messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                    &format!("解析失败: {}", e),
                                    &tool_call.id,
                                )));
                            }
                        }
                    }

                    if !has_valid_call {
                        // 如果有 tool_call 但都不是有效的翻译提交，让 LLM 继续
                        continue;
                    }

                    // 询问 LLM 是否还有更多翻译
                    messages.push(MessageRequest::user(
                        "请继续提交剩余的翻译条目，或调用 stop 完成。如果所有翻译已完成，请只回复'所有翻译已完成'。",
                    ));
                    continue;
                }
            }

        // finish_reason == Stop 或其他
        // 检查是否还有 tool_calls（某些模型可能在 stop 时附带 tool calls）
        if let Some(ref msg) = resp.choices[0].message {
            messages.push(MessageRequest::Assistant(msg.clone()));
            if msg.tool_calls.is_some() {
                // 有 tool calls 但 finish_reason 是 Stop，再发一次请求
                continue;
            }
        }

        // 真正结束
        let content = resp.choices[0]
            .message
            .as_ref()
            .map(|m| m.content.as_str())
            .unwrap_or("");

        debug!("LLM 最终回复: {}", content);
        break;
    }

    if loop_count >= MAX_LOOPS {
        warn!("LLM 交互达到最大循环次数 ({})，返回已收集的翻译", MAX_LOOPS);
    }

    Ok(result_ini)
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 6: 处理单个 mod
// ══════════════════════════════════════════════════════════════════════════════

/// 处理单个 mod 的翻译流程。
///
/// 给定 mod 的名称，从本地缓存加载上次翻译，下载 mod 并提取翻译文件，
/// 计算差异，调用 LLM 获取翻译，合并保存。
async fn process_mod(
    client_fa: &FactorioWebClient,
    client_deepseek: &deepseek_api::DeepSeekClient,
    config: &FlowConfig,
    base_locale: &ini::Ini,
    system_prompt: &str,
    mod_name: &str,
) -> anyhow::Result<()> {
    info!("处理 mod: {}", mod_name);

    // 1. 从本地缓存加载上次翻译
    let cache_path = config.cache_dir.join(format!("{}.json", mod_name));
    let cached_locale: Option<LocaleInfo> = if cache_path.exists() {
        match std::fs::read_to_string(&cache_path) {
            Ok(content) => match serde_json::from_str::<LocaleInfo>(&content) {
                Ok(locale) => {
                    info!("  ↳ 已加载缓存: {} 种语言", locale.contents.len());
                    Some(locale)
                }
                Err(e) => {
                    warn!("  ↳ 缓存文件损坏: {} — 将重新翻译", e);
                    None
                }
            },
            Err(e) => {
                warn!("  ↳ 无法读取缓存: {} — 将重新翻译", e);
                None
            }
        }
    } else {
        None
    };

    // 2. 获取 mod 信息
    let mod_info = match client_fa.get_mod(mod_name).await {
        Ok(m) => m,
        Err(e) => {
            error!("  ✗ 获取 mod 信息失败: {}", e);
            return Err(e);
        }
    };

    // 3. 下载 mod 并提取翻译文件
    let zip_data = if let Some(ref release) = mod_info.latest_release {
        client_fa.download_release(release).await?
    } else if let Some(ref releases) = mod_info.releases
        && let Some(latest) = releases.last()
    {
        client_fa.download_release(latest).await?
    } else {
        warn!("  ↳ mod 没有发布版本，跳过");
        return Ok(());
    };

    let current_locale = extract_locale_from_zip(&zip_data)
        .with_context(|| format!("无法从 {} 的 zip 中提取翻译文件", mod_name))?;

    let lang_count = current_locale.contents.len();
    if lang_count == 0 {
        info!("  ↳ mod 没有翻译文件，跳过");
        return Ok(());
    }
    info!("  ↳ 提取到 {} 种语言的翻译文件", lang_count);

    // 4. 构建翻译任务条目
    let mut entries: Vec<TranslationEntry> = Vec::new();

    for (lang_code, lang_info) in &current_locale.contents {
        for (file_name, content) in &lang_info.contents {
            let current_ini = translation::str_to_ini(content);

            // 确定需要翻译的条目
            let to_translate = if let Some(ref cached) = cached_locale
                && let Some(cached_lang) = cached.contents.get(lang_code)
                && let Some(cached_content) = cached_lang.contents.get(file_name)
            {
                let cached_ini = translation::str_to_ini(cached_content);
                translation::diff_ini(&cached_ini, &current_ini)
            } else {
                // 没有缓存，全部都是新的
                current_ini.clone()
            };

            for (section, props) in to_translate.iter() {
                let section_name = section.unwrap_or("(global)");
                for (key, value) in props.iter() {
                    entries.push(TranslationEntry {
                        file_name: file_name.clone(),
                        section: section_name.to_string(),
                        key: key.to_string(),
                        original: value.to_string(),
                    });
                }
            }
        }
    }

    if entries.is_empty() {
        info!("  ↳ 没有需要翻译的新内容，跳过");
        return Ok(());
    }
    info!("  ↳ 需要翻译 {} 个条目", entries.len());

    // 5. 构建提示词并调用 LLM
    let user_prompt = build_user_prompt(base_locale, &entries, cached_locale.as_ref());

    info!("  ↳ 调用 LLM 进行翻译...");
    let llm_translation =
        call_llm_for_translation(client_deepseek, system_prompt, &user_prompt).await?;

    if llm_translation.is_empty() {
        warn!("  ↳ LLM 未返回任何翻译结果");
        return Ok(());
    }
    info!(
        "  ↳ LLM 返回了 {} 个 section 的翻译",
        llm_translation.iter().count()
    );

    // 6. 合并翻译：以当前英文原文为基准，LLM 翻译优先，旧缓存兜底
    let mut merged_locale = LocaleInfo {
        contents: indexmap::IndexMap::new(),
        version: current_locale.version.clone(),
    };

    for (lang_code, lang_info) in &current_locale.contents {
        let mut merged_lang = LangInfo {
            contents: indexmap::IndexMap::new(),
        };

        for (file_name, content) in &lang_info.contents {
            let reference_ini = translation::str_to_ini(content); // 当前英文原文（作为 key 的参考）
            let old_ini = cached_locale
                .as_ref()
                .and_then(|c| c.contents.get(lang_code))
                .and_then(|l| l.contents.get(file_name))
                .map(|s| translation::str_to_ini(s));

            // 使用 merge_ini：reference = 当前英文原文，old = 旧翻译，diff = LLM 翻译
            let merged_ini = translation::merge_ini(
                &reference_ini,
                &old_ini.unwrap_or_default(),
                &llm_translation,
            );
            merged_lang
                .contents
                .insert(file_name.clone(), translation::ini_to_str(&merged_ini));
        }

        merged_locale
            .contents
            .insert(lang_code.clone(), merged_lang);
    }

    // 7. 保存到缓存（通过 Persistent 机制）
    std::fs::create_dir_all(&config.cache_dir)?;
    let cache_file_path = config.cache_dir.join(format!("{}.json", mod_name));

    {
        let _persistent = persistent_via_file(merged_locale, &cache_file_path);
        // Drop 时自动序列化写入文件
    }

    info!("  ✓ 翻译已保存到: {:?}", cache_file_path);

    // 8. API 间隔
    tokio::time::sleep(Duration::from_millis(config.api_delay_ms)).await;

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 主流程
// ══════════════════════════════════════════════════════════════════════════════

/// 运行完整的翻译管道。
///
/// ## 参数
///
/// - `config`: 管道配置
/// - `since`: 时间起点（None = 从上次运行时间开始）
/// - `limit`: 最大处理 mod 数量（None = 无限制）
///
/// ## 流程
///
/// 1. 初始化 Factorio 和 DeepSeek 客户端
/// 2. 加载外部参考文件
/// 3. 获取自 `last_run` 以来更新的 mod 列表
/// 4. 逐个处理每个 mod
/// 5. 更新 last_run 时间
pub async fn run_translation_pipeline(
    config: FlowConfig,
    since: Option<DateTime<Utc>>,
    limit: Option<usize>,
) -> anyhow::Result<()> {
    // 加载上次运行时间
    let state_path = config.cache_dir.join("_pipeline_state.json");
    let mut state: PipelineState = if state_path.exists() {
        std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        PipelineState::default()
    };

    let effective_since = since.unwrap_or(state.last_run);
    info!(
        "翻译管道启动 — 更新起点: {}",
        effective_since.format("%Y-%m-%d %H:%M:%S")
    );

    // 初始化客户端
    let deepseek_client = DeepSeekClientBuilder::new(config.deepseek_key.clone())
        .build()
        .context("无法创建 DeepSeek 客户端")?;

    let fa_client = if let (Ok(user), Ok(pass)) = (
        std::env::var("FACTORIO_USERNAME"),
        std::env::var("FACTORIO_PASSWORD"),
    ) {
        FactorioWebClient::login(user, pass).await?
    } else if let (Ok(user), Ok(token)) = (
        std::env::var("FACTORIO_USERNAME"),
        std::env::var("FACTORIO_TOKEN"),
    ) {
        FactorioWebClient::prefilled(user, token).await
    } else {
        anyhow::bail!("需要设置 FACTORIO_USERNAME + (FACTORIO_PASSWORD 或 FACTORIO_TOKEN)");
    };

    // 加载外部文件
    let base_locale = load_base_locale(&config.base_locale_path)?;
    let system_prompt = load_system_prompt(&config.system_prompt_path)?;

    info!("外部参考文件加载完成");

    // 获取更新的 mod 列表
    let updated_mods = fa_client
        .get_mods_updated_since(effective_since, &config.game_version, Some(100))
        .await
        .context("获取更新的 mod 列表失败")?;

    info!("发现 {} 个更新的 mod", updated_mods.len());

    let mut processed = 0;
    for mod_entry in &updated_mods {
        if let Some(limit) = limit
            && processed >= limit
        {
            info!("已达到处理上限 ({})，停止", limit);
            break;
        }

        match process_mod(
            &fa_client,
            &deepseek_client,
            &config,
            &base_locale,
            &system_prompt,
            &mod_entry.name,
        )
        .await
        {
            Ok(()) => processed += 1,
            Err(e) => {
                error!("处理 mod {} 失败: {:?}", mod_entry.name, e);
                // 继续处理下一个
            }
        }
    }

    // 更新上次运行时间（使用当前时间）
    let now = Utc::now();
    state.last_run = now;
    std::fs::create_dir_all(&config.cache_dir)?;
    {
        _ = persistent_via_file(state, &state_path);
    }

    info!(
        "翻译管道完成 — 处理了 {}/{} 个 mod，last_run 更新为: {}",
        processed,
        updated_mods.len(),
        now.format("%Y-%m-%d %H:%M:%S")
    );

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// 测试
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use std::{env::var, io::Write};

    use deepseek_api::{
        CompletionsRequestBuilder, DeepSeekClientBuilder, RequestBuilder,
        request::{Function, MessageRequest, ToolMessageRequest, ToolObject, ToolType},
        response::FinishReason,
    };

    #[tokio::test]
    async fn main() -> anyhow::Result<()> {
        dotenvy::dotenv().ok();
        let client = DeepSeekClientBuilder::new(var("DEEPSEEK_KEY")?).build()?;
        let parameters = serde_json::from_str(
            r#"{
            "type": "object",
            "properties": {
                "input": {
                    "type": "number",
                    "description": "The input to the function"
                }
            }
    }"#,
        )?;

        let tool_object = ToolObject {
            tool_type: ToolType::Function,
            function: Function {
                name: "test_function".to_string(),
                description: "A simple test function".to_string(),
                parameters,
            },
        };

        let tool_objects: Vec<ToolObject> = vec![tool_object];
        let mut messages = vec![MessageRequest::user(
            "Call the function with parameter to test the tool calling feature.",
        )];
        let resp = CompletionsRequestBuilder::new(&messages)
            .tools(&tool_objects)
            .do_request(&client)
            .await?
            .must_response();
        let mut id = String::new();
        let mut arguments = String::new();
        if resp.choices[0].finish_reason == FinishReason::ToolCalls {
            if let Some(msg) = &resp.choices[0].message {
                if let Some(tool) = &msg.tool_calls {
                    id = tool[0].id.clone();
                    println!("Function id: {}", id);
                    println!("Function name: {}", tool[0].function.name);
                    println!("Function parameters: {:?}", tool[0].function.arguments);
                    arguments = tool[0].function.arguments.clone();
                }
                messages.push(MessageRequest::Assistant(msg.clone()));
            }
        }

        messages.push(MessageRequest::Tool(ToolMessageRequest::new(
            &format!("Called test_function with arguments: {}", arguments),
            &id,
        )));
        let resp = CompletionsRequestBuilder::new(&messages)
            .tools(&tool_objects)
            .do_request(&client)
            .await?
            .must_response();
        println!(
            "Reply with my function: {:?}",
            resp.choices[0].message.as_ref().unwrap().content
        );
        dbg!(messages);
        Ok(())
    }

    #[test]
    fn test_extract_locale_from_zip() {
        // 创建一个测试用的 zip 文件
        let cursor = std::io::Cursor::new(Vec::new());
        let mut zip_writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file("locale/en/base.cfg", options)
            .unwrap();
        zip_writer
            .write_all(b"[entity-name]\niron-plate=Iron plate\ncopper-plate=Copper plate\n")
            .unwrap();

        zip_writer
            .start_file("locale/zh-CN/base.cfg", options)
            .unwrap();
        zip_writer
            .write_all("[entity-name]\niron-plate=铁板\n".as_bytes())
            .unwrap();

        zip_writer.start_file("info.json", options).unwrap();
        zip_writer
            .write_all(b"{\"name\":\"test-mod\",\"version\":\"1.0.0\"}")
            .unwrap();

        let zip_buf = zip_writer.finish().unwrap().into_inner();

        let locale = super::extract_locale_from_zip(&zip_buf).unwrap();
        assert_eq!(locale.version, "1.0.0");
        assert_eq!(locale.contents.len(), 2);
        assert!(locale.contents.contains_key("en"));
        assert!(locale.contents.contains_key("zh-CN"));

        let en_base = locale.contents["en"].contents["base.cfg"].as_str();
        assert!(en_base.contains("iron-plate=Iron plate"));
    }
}
