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
//! 1. 获取自上次运行以来更新的所有 mod，并合并 `.pipeline_state.failed_mods` 中的失败项
//! 2. 对于每个 mod：
//!    - 获取 mod 的所有翻译文件（下载 zip → 解压 → 收集 `locale/` 下的 `.cfg` 文件）
//!    - 如果本地有缓存，则获取差异
//!    - 将预先构筑好的提示词送入 LLM
//!    - 将 LLM 的输出与本地缓存进行合并，生成新的翻译
//!    - 保存新的翻译文件到本地缓存
//! 3. 成功项移出失败队列，错误项留待下次自动运行重试；同名 mod 优先处理增量中的新版本
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
    collections::{BTreeMap, BTreeSet},
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
use futures_util::StreamExt;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};

use crate::{
    persistent::persistent_via_file,
    translation::{self, LangInfo, LocaleInfo},
};

// ══════════════════════════════════════════════════════════════════════════════
// 全局调试开关
// ══════════════════════════════════════════════════════════════════════════════

/// 全局 LLM 详细日志开关（打印思考内容 / 文本内容 / 工具调用）
static LLM_VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 设置是否打印 LLM 的思考内容、文本内容和工具调用信息（调试用）
pub fn set_llm_verbose(enabled: bool) {
    LLM_VERBOSE.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

// ══════════════════════════════════════════════════════════════════════════════
// 类型别名
// ══════════════════════════════════════════════════════════════════════════════

/// Mod 名称和版本的组合键
pub type ModKey = (String, String);

// ══════════════════════════════════════════════════════════════════════════════
// 管道状态（上次运行时间和待重试 mod）
// ══════════════════════════════════════════════════════════════════════════════

/// 持久化的管道状态，失败只记录名称，重试时重新获取最新版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineState {
    /// 上次运行时间（UTC）
    pub last_run: DateTime<Utc>,
    /// 因错误未能完成的 mod；兼容只有 last_run 的旧状态文件。
    #[serde(default)]
    pub failed_mods: BTreeSet<String>,
}

impl Default for PipelineState {
    fn default() -> Self {
        Self {
            last_run: DateTime::UNIX_EPOCH,
            failed_mods: BTreeSet::new(),
        }
    }
}

impl PipelineState {
    fn record_mod_result(&mut self, name: &str, succeeded: bool) {
        if succeeded {
            self.failed_mods.remove(name);
        } else {
            self.failed_mods.insert(name.to_owned());
        }
    }
}

/// 增量结果优先，重试只补充尚未出现的名称，避免下载同一 mod 的旧版本。
fn merge_mod_candidates(
    updated: Vec<factorio_api::ResultEntry>,
    retries: Vec<factorio_api::ResultEntry>,
) -> Vec<factorio_api::ResultEntry> {
    let mut seen = BTreeSet::new();
    updated
        .into_iter()
        .chain(retries)
        .filter(|entry| seen.insert(entry.name.clone()))
        .collect()
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
    /// Factorio 游戏数据目录（用于提取原版对照表）
    pub game_data_path: Option<PathBuf>,
    /// API 请求间隔（毫秒），避免触发速率限制
    pub api_delay_ms: u64,
    /// DeepSeek API Key
    pub deepseek_key: String,
}

impl FlowConfig {
    /// 从环境变量构建配置，缺失的变量使用默认值
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();

        let game_version = std::env::var("FACTORIO_VERSION").unwrap_or_else(|_| "2.1".to_string());

        let cache_dir = std::env::var("TANVEC_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./cache"));

        let base_locale_path = std::env::var("TANVEC_BASE_LOCALE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/base_game_locale.ini"));

        let system_prompt_path = std::env::var("TANVEC_SYSTEM_PROMPT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/system_prompt.txt"));

        let game_data_path = std::env::var("FACTORIO_DATA_PATH").ok().map(PathBuf::from);

        let deepseek_key = std::env::var("DEEPSEEK_KEY").context("环境变量 DEEPSEEK_KEY 未设置")?;

        Ok(Self {
            game_version,
            cache_dir,
            base_locale_path,
            system_prompt_path,
            game_data_path,
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
/// Factorio mod 的 zip 内部通常有一层根目录（如 `mod-name_version/`），
/// 本函数会先自动检测并剥离这层前缀，再查找 `locale/<语言代码>/<文件名>.cfg`。
///
/// 返回 `LocaleInfo`，其中 key 为语言代码（如 `"zh-CN"`, `"en"`），
/// value 为该语言下所有 `.cfg` 文件的内容。
/// 从 `info.json` 的 `dependencies` 字段提取依赖模组名列表。
///
/// 忽略冲突依赖（`!` 开头）以及 `base` / `core` 官方基础模块，
/// 去除可选/前置修饰符（`(?)`、`?`、`~`、`+`）和版本约束（`>= 1.0.0` 等）。
pub fn parse_mod_dependencies(info: &serde_json::Value) -> Vec<String> {
    let raw_deps: Vec<&str> = match info.get("dependencies") {
        Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str()).collect(),
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        _ => Vec::new(),
    };

    let mut deps = Vec::new();
    let mut seen = BTreeSet::new();

    for raw in raw_deps {
        let mut s = raw.trim();
        if s.is_empty() || s.starts_with('!') {
            continue;
        }
        loop {
            let trimmed = s.trim_start();
            if let Some(rest) = trimmed.strip_prefix("(?)") {
                s = rest;
            } else if let Some(rest) = trimmed
                .strip_prefix('?')
                .or_else(|| trimmed.strip_prefix('~'))
                .or_else(|| trimmed.strip_prefix('+'))
            {
                s = rest;
            } else {
                s = trimmed;
                break;
            }
        }
        let mod_name = s
            .split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '='))
            .next()
            .unwrap_or("")
            .trim();

        if mod_name.is_empty() || mod_name == "base" || mod_name == "core" {
            continue;
        }
        if seen.insert(mod_name.to_string()) {
            deps.push(mod_name.to_string());
        }
    }

    deps
}

pub fn extract_locale_from_zip(zip_bytes: &[u8]) -> anyhow::Result<LocaleInfo> {
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("无法打开 zip 文件")?;

    let mut locale_info = LocaleInfo {
        contents: indexmap::IndexMap::new(),
        version: String::new(),
        dependencies: Vec::new(),
    };

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;

        let name = file.name().to_string();
        // 跳过目录、超大文件、macOS 资源分支
        if name.ends_with('/')
            || file.size() > 5 * 1024 * 1024
            || name.starts_with("__MACOSX")
            || name.contains("/__MACOSX")
            || name.starts_with('.')
        {
            continue;
        }

        // 在路径中查找 locale/<lang>/<filename>.cfg（不依赖前缀检测）
        if let Some(locale_pos) = name.find("/locale/")
            && !name[0..locale_pos].contains('/')
        {
            let after_locale = &name[locale_pos + 8..]; // 跳过 "/locale/"
            let parts: Vec<&str> = after_locale.splitn(2, '/').collect();
            if parts.len() == 2 {
                let lang_code = parts[0].to_string();
                let file_name = parts[1].to_string();
                if !(lang_code == "en" || lang_code == "zh-CN") {
                    continue;
                }
                if file_name.ends_with(".cfg") {
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
        // 兼容无前缀的 locale/ 路径
        else if let Some(rest) = name.strip_prefix("locale/") {
            let parts: Vec<&str> = rest.splitn(2, '/').collect();
            if parts.len() == 2 {
                let lang_code = parts[0].to_string();
                let file_name = parts[1].to_string();
                if file_name.ends_with(".cfg") {
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

        // 检测 info.json（结尾匹配，不依赖前缀）
        if name.ends_with("/info.json") || name == "info.json" {
            let mut info_content = String::new();
            if file.read_to_string(&mut info_content).is_ok()
                && let Ok(info) = serde_json::from_str::<serde_json::Value>(&info_content)
            {
                locale_info.version = info
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                locale_info.dependencies = parse_mod_dependencies(&info);
            }
        }
    }

    Ok(locale_info)
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 2: 加载外部参考文件
// ══════════════════════════════════════════════════════════════════════════════

/// 提取 Factorio 官方 mod 的中英文对照表。
///
/// 遍历 `core`, `base`, `quality`, `elevated-rails`, `space-age`, `recycler` 六个官方 mod 的
/// `locale/en/*.cfg` 和对应 `zh-CN/*.cfg`，生成 key → 中文翻译 的映射。
/// 后面的 mod（DLC）的翻译会覆盖前面的。
pub fn extract_base_glossary(game_data_path: &Path) -> anyhow::Result<ini::Ini> {
    const OFFICIAL_MODS: &[&str] = &[
        "core",
        "base",
        "quality",
        "elevated-rails",
        "space-age",
        "recycler",
    ];

    let mut glossary = ini::Ini::new();

    let mut sections_found = std::collections::HashSet::new();

    for mod_name in OFFICIAL_MODS {
        let mod_locale = game_data_path.join(mod_name).join("locale");
        let en_dir = mod_locale.join("en");
        let zh_dir = mod_locale.join("zh-CN");

        if !en_dir.exists() {
            continue;
        }

        for entry in std::fs::read_dir(&en_dir)
            .with_context(|| format!("无法读取 {} 的英文 locale: {:?}", mod_name, en_dir))?
        {
            let entry = entry?;
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy();

            if !file_name_str.ends_with(".cfg") {
                continue;
            }

            let en_content = std::fs::read_to_string(entry.path())?;
            let en_ini = translation::str_to_ini(&en_content)?;

            let zh_path = zh_dir.join(&file_name);
            let zh_ini = if zh_path.exists() {
                let zh_content = std::fs::read_to_string(&zh_path)?;
                translation::str_to_ini(&zh_content)?
            } else {
                continue;
            };

            for (section, props) in en_ini.iter() {
                if let Some(sec) = section {
                    sections_found.insert(sec.to_string());
                }
                if section.is_none_or(|s| !s.contains("name")) {
                    continue;
                }
                for (key, en_value) in props.iter() {
                    if en_value.is_empty()
                        || en_value
                            .chars()
                            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
                    {
                        continue;
                    }
                    if let Some(zh_value) = zh_ini.section(section).and_then(|s| s.get(key))
                        && !zh_value.is_empty() && zh_value != en_value {
                            glossary.with_general_section().set(en_value, zh_value);
                        }
                }
            }
        }
    }
    Ok(glossary)
}

const OFFICIAL_MODS: &[&str] = &[
    "core",
    "base",
    "quality",
    "elevated-rails",
    "space-age",
    "recycler",
];

fn extract_base_all_for_lang(
    game_data_path: &Path,
    lang: &str,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut keys = std::collections::HashMap::new();
    for mod_name in OFFICIAL_MODS {
        let lang_dir = game_data_path.join(mod_name).join("locale").join(lang);
        if !lang_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&lang_dir)
            .with_context(|| format!("无法读取 {} 的 {} locale: {:?}", mod_name, lang, lang_dir))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "cfg") {
                continue;
            }
            let content = std::fs::read_to_string(&path)?;
            let ini = translation::str_to_ini(&content)?;
            for (section, props) in ini.iter() {
                let sec_prefix = section.map_or_else(String::new, |s| format!("{}.", s));
                for (key, value) in props.iter() {
                    keys.insert(format!("{}{}", sec_prefix, key), value.to_string());
                }
            }
        }
    }
    Ok(keys)
}

/// 提取原版游戏中所有官方 mod 的 zh-CN locale key 集合（用于检测 mod 翻译是否覆盖原版）。
pub fn extract_base_all(
    game_data_path: &Path,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    extract_base_all_for_lang(game_data_path, "zh-CN")
}

/// 提取原版游戏中所有官方 mod 的 en locale key 集合（用于判断 mod 是否原封不动复制了原版英文）。
pub fn extract_base_all_en(
    game_data_path: &Path,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    extract_base_all_for_lang(game_data_path, "en")
}

/// 从缓存目录按 BFS 解析当前 mod 的依赖链（最多 3 层），末尾追加 `mod_name` 自身。
pub fn resolve_mod_dependency_chain(
    mod_name: &str,
    direct_deps: &[String],
    cache_dir: &Path,
) -> Vec<String> {
    #[derive(Deserialize)]
    struct CachedDeps {
        #[serde(default)]
        dependencies: Vec<String>,
    }

    let mut visited = BTreeSet::new();
    let mut chain = Vec::new();
    let mut queue = std::collections::VecDeque::new();

    for dep in direct_deps {
        if dep != mod_name && !dep.is_empty() {
            queue.push_back((dep.clone(), 1usize));
        }
    }

    while let Some((dep, depth)) = queue.pop_front() {
        if dep == mod_name || !visited.insert(dep.clone()) {
            continue;
        }
        chain.push(dep.clone());

        if depth < 3 {
            let dep_cache = cache_dir.join(format!("{}.json", dep));
            if dep_cache.exists()
                && let Ok(content) = std::fs::read_to_string(&dep_cache)
                && let Ok(parsed) = serde_json::from_str::<CachedDeps>(&content)
            {
                for sub_dep in parsed.dependencies {
                    if !sub_dep.is_empty() && sub_dep != mod_name && !visited.contains(&sub_dep) {
                        queue.push_back((sub_dep, depth + 1));
                    }
                }
            }
        }
    }

    chain.push(mod_name.to_string());
    chain
}

/// 按依赖链与待翻译原文构建大小写不敏感的上下文术语表：
/// 1. 遗留全局节（`None` / `ai-glossary`）：仅注入原文中实际出现过的条目（长度 >= 2）
/// 2. 原版官方对照表（`base_locale`）：仅注入原文中实际出现过的条目
/// 3. 当前模组及其依赖链（`dep_chain`）：条件载入对应 `[<mod_name>]` 节中的所有有效术语（最高优先级）
pub fn build_mod_glossary(
    base_locale: &ini::Ini,
    ai_glossary: &ini::Ini,
    dep_chain: &[String],
    file_diffs: &BTreeMap<String, ini::Ini>,
) -> ini::Ini {
    let mut diff_text = String::new();
    for diff_ini in file_diffs.values() {
        for (_sec, props) in diff_ini.iter() {
            for (_k, v) in props.iter() {
                diff_text.push_str(v);
                diff_text.push('\n');
            }
        }
    }
    let diff_text_lower = diff_text.to_lowercase();

    let mut entries: indexmap::IndexMap<String, (String, String)> = indexmap::IndexMap::new();

    // 1. 遗留全局术语（仅按原文命中注入，避免无关模组词条污染上下文）
    for legacy_sec in [None, Some("ai-glossary")] {
        if let Some(props) = ai_glossary.section(legacy_sec) {
            for (k, v) in props.iter() {
                let k_trimmed = k.trim();
                let v_trimmed = v.trim();
                if k_trimmed.len() >= 2
                    && !v_trimmed.is_empty()
                    && !k_trimmed.eq_ignore_ascii_case(v_trimmed)
                {
                    let lower = k_trimmed.to_lowercase();
                    if diff_text_lower.contains(&lower) {
                        entries.insert(lower, (k_trimmed.to_string(), v_trimmed.to_string()));
                    }
                }
            }
        }
    }

    // 2. 原版官方术语对照表（仅按原文命中注入）
    for (_sec, props) in base_locale.iter() {
        for (k, v) in props.iter() {
            let k_trimmed = k.trim();
            let v_trimmed = v.trim();
            if !k_trimmed.is_empty() && !v_trimmed.is_empty() {
                let lower = k_trimmed.to_lowercase();
                if diff_text_lower.contains(&lower) {
                    entries.insert(lower, (k_trimmed.to_string(), v_trimmed.to_string()));
                }
            }
        }
    }

    // 3. 依赖链与当前模组专有术语（最高优先级）
    for dep in dep_chain {
        if let Some(props) = ai_glossary.section(Some(dep.as_str())) {
            for (k, v) in props.iter() {
                let k_trimmed = k.trim();
                let v_trimmed = v.trim();
                if !k_trimmed.is_empty()
                    && !v_trimmed.is_empty()
                    && !k_trimmed.eq_ignore_ascii_case(v_trimmed)
                {
                    entries.insert(
                        k_trimmed.to_lowercase(),
                        (k_trimmed.to_string(), v_trimmed.to_string()),
                    );
                }
            }
        }
    }

    let mut out = ini::Ini::new();
    for (term, translation) in entries.values() {
        out.with_general_section().set(term, translation);
    }
    out
}

#[test]
fn test_extract_base_glossary() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let game_data_path = std::env::var("FACTORIO_DATA_PATH")
        .map(PathBuf::from)
        .expect("请设置 FACTORIO_DATA_PATH 环境变量指向游戏数据目录");
    // dbg!(&game_data_path);
    let glossary = extract_base_glossary(&game_data_path).expect("提取对照表失败");
    dbg!(&glossary);
    // println!("{}", translation::ini_to_str(&glossary)?);
    Ok(())
}

/// 加载或构建原版游戏中英文对照表（INI 格式）。
///
/// 优先级：
/// 1. 从游戏数据提取（如果配置了 `FACTORIO_DATA_PATH`）
/// 2. 加载已有的对照表文件
/// 3. 空对照表
pub fn load_base_locale(config: &FlowConfig) -> anyhow::Result<ini::Ini> {
    // 优先从游戏数据提取
    if let Some(ref game_data) = config.game_data_path
        && game_data.exists()
    {
        info!("从游戏数据提取原版对照表: {:?}", game_data);
        return extract_base_glossary(game_data);
    }

    let path = &config.base_locale_path;
    if !path.exists() {
        warn!("原版对照表文件不存在: {:?}，将以空对照表继续", path);
        return Ok(ini::Ini::new());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("无法读取原版对照表文件: {:?}", path))?;
    translation::str_to_ini(&content)
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
const DEFAULT_SYSTEM_PROMPT: &str = r#"
你是一个专业的中文本地化翻译专家，专门负责将 Factorio 模组的英文文本翻译成简体中文。

## 总体风格

### 标点符号

除了方括号包裹的变量内以外，优先使用直角引号「」和中文逗号，句号，省略号，感叹号等。长句子末尾必须有一个标点符号。句子中的参数、数字和词语不需要分开一个空格。如「参数__1__」比「参数 __1__」更优，「__1__分钟前」比「__1__ 分钟前」更优。

### 语态

优先使用主动语态。

### 单位

各类国际单位制单位都不需要翻译，只用保留原文。

### 伤害类型

统一为四字「XX伤害」。

### 装备模块

出现在equipment-name下的内容，末尾必须以「模块」结尾，必要时可以替换表示地面建筑的名词。如「太阳能板」→「太阳能模块」，不得翻译为「便携式太阳能板」，「裂变反应堆」→「裂变反应堆模块」，不得翻译为「便携式裂变反应堆」，「机器人指令平台」→「机器人指令模块」，不得翻译为「便携式机器人指令平台」。
注意：装备末尾必须以「模块」二字结尾。

Power armor翻译为能量装甲，不是动力装甲。

如果英文原文的模块以 Personal 开头，翻译可以省略“个人”这个前缀。

### 星球描述

有且仅在space-location-description下或technology-description下出现星球介绍时，开头需要以「星球名（英文原名）」开头。

#### 原版星球译名

- Nauvis: 新地星
- Gleba: 句芒星
- Vulcanus: 祝融星
- Fulgora: 雷神星
- Aquilo: 玄冥星

科技名称为发现某某星球时，翻译格式为：「发现星球：XXX」，如果天体不是行星，可以视情况变为「发现卫星」，「发现星系」等。

星球优先尝试意译，其次尝试音译，最次翻译为XX星，其中XX是英文原名。

### 星岩

asteroid 翻译为「星岩」，asteroid chunk 翻译为「星块」。

### 插件

用于增幅建筑物效果的module-name，需要将module翻译成「插件」，以下左侧的翻译需要更改为右侧的。efficiency module:「效率模块」→「节能插件」，productivity module:「生产模块」→「产能插件」，speed module:「速度模块」→「速度插件」，quality module:「质量模块」→「品质插件」。

### 插件塔

原版的beacon翻译为「插件效果分享塔」，模组中出现其他类型的带前缀描述的beacon时，可以适当缩减主体名称为「插件分享塔」或「插件塔」，以保证最终译名不超过六个字。

### 研究中心

lab统一翻译为「研究中心」，出现前缀时将前缀翻译添加即可。

### 科技

X-productivity科技需要翻译为「XX增产计划」。

### 科技包

科技包必须翻译为「XX科技包（Y瓶）」，其中XX为科技属性描述，Y为颜色。特别的，automation science pack或automation指代这个科技包的情况下，automation翻译为「机自」。

原版科技包颜色如下：

- 机自科技包（红瓶）
- 物流科技包（绿瓶）
- 军备科技包（灰瓶）
- 化工科技包（蓝瓶）
- 太空科技包（白瓶）
- 效能科技包（黄瓶）
- 生产科技包（紫瓶）
- 冶金科技包（橙瓶）
- 农业科技包（草瓶）
- 电磁科技包（粉瓶）
- 低温科技包（靛瓶）
- 钷素科技包（黑瓶）

对于模组中新添加的科技包，不要在后缀中添加颜色，直接翻译为「XX科技包」即可，尽可能以两字作为前缀。

### 机械臂

inserter需要翻译为「机械臂」，而不是「插入器」或「插入臂」。普通的inserter消耗电能，二外翻译为成「电力机械臂」。注意：burner开头的燃煤设备，前缀为「热能」，不得翻译为「燃烧器」。注意：long handed开头的翻译为「加长」。

#### 机械臂变种

stack描述堆叠行为，stack inserter需要翻译为「堆叠机械臂」，bulk描述机械臂大小，需要翻译为「集装机械臂」，相应的机械臂的手的容量表述为「集装机械臂搬运量」和「集装机械臂搬运上限」。

### 物流箱

物流功能的容器，翻译时保证主体不超过五个字，前四个字为固定搭配，括号标注颜色加箱子类型，括号内容不超过两个字。

注意：对应物流功能的箱子，颜色是固定的，必须括号标注颜色和箱子类型。比如
active provider warehouse: 主动供货仓（紫仓），warehouse可以翻译为「仓库」或「仓」。

- Active provider chest: 主动供货箱（紫箱）
- Passive provider chest: 被动供货箱（红箱）
- Storage chest: 被动存货箱（黄箱）
- Buffer chest: 主动存货箱（绿箱）
- Requester chest: 优先集货箱（蓝箱）

### 敌人

示例：

- biter: 撕咬虫
- biter spawner: 撕咬虫巢
- spitter: 喷吐虫
- spitter spawner: 喷吐虫巢
- biter egg: 异虫卵
- pentapod egg: 五足虫卵
- wriggler pentapod: 蠕动五足虫
- strafer pentapod: 扫射五足虫
- stomper pentapod: 重踏五足虫
- egg raft: 卵筏
- demolisher: 撼地虫

### 传送带与管道

无前缀的transport belt，underground belt, splitter, loader等，翻译为「基础传送带」，「基础地下传送带」，「基础分流器」和「基础装卸机」。faster前缀翻译为「高速」，express前缀翻译为「极速」，turbo翻译为「超速」。

pipe为「管道」，pipe-to-ground为「地下管道」

### 矿石

iron ore和copper ore翻译为铁矿、铜矿，不需要翻译为铁矿石、铜矿石。当矿物本身名字很长，或名字本身可以表示矿物而非对应纯净物时，可以翻译为本身。如「方解石」。加工结果中，plate为板，beam为梁，rod为棒，coil为卷，cable为缆，wire为线。特别的，steel plate翻译为钢材。液态金属配方使用「熔融」描述液态，使用「铸造」描述对应配方。

### 机器人

战斗无人机有三种，分别为defender「防御无人机」，distractor「掩护无人机」，destroyer「进攻无人机」。工作机器人有两种，分别为construction「建设机器人」和logistic「物流机器人」。roboport翻译为「机器人指令平台」。

### 其他

所有翻译均可参考上文给出的基础游戏中英文对照表。

## 特殊

双下划线包裹的内容不要翻译，如__ITEM__iron-plate__。
特殊情况时__plural_for_parameter__1__{1=item|rest=items}__，可以视情况移除变量，只用翻译为原型，因为中文没有单复数。
即：
物品 √
__plural_for_parameter__1__{1=物品|rest=物品}__ ×
但是，有时__plural_for_parameter__1__是用来控制表达方式的，此时不应移除。
如：
__plural_for_parameter__1__{1=Stop is|2=Both stops are|rest=All __1__ stops are}__ disabled.
__plural_for_parameter__1__{1=此车站|rest=共 __1__ 个车站}__已被禁用。

__plural_for_parameter__1__{0=No stop is accessible|1=Stop is inaccessible|2=Both stops are inaccessible|rest=All __1__ stops are inaccessible}__ from current position.
从此处出发__plural_for_parameter__1__{0=没有能抵达的车站|rest=有 __1__ 个车站无法抵达}__。
请根据上下文选择合适的转换方式。
方括号内内容也不要翻译，如[item=iron-plate]，[img=item.lab]之类都是富文本，保留原样。
虚拟物品结尾出现unit时，可以根据情况翻译为「单位」或「器」。如处理器，能源控制器，火箭控制器。
ghost表示没有完成建筑的建筑时，翻译为「虚影」。
模组中出现torio结尾的单词时，可以根据情况翻译为「XX工厂」，和异星工厂 factorio 匹配。
*如果模组中用不合意的单词覆盖了原版词汇，按照覆盖的词汇进行翻译，打包器默认会禁用此类mod，不用担心词条覆盖。*
如：`spoil-time=Decay time`时，翻译为`spoil-time=衰变时间`。

## 总结

翻译规则：
1. 保持游戏术语的一致性，参考提供的原版对照表
2. 对于技术类文本，使用准确、简洁的中文表达
3. 保留原文中的格式标记（如 __1__、[item=xxx] 等占位符）
4. 不要翻译专有名词、代码标识符
5. 使用简体中文书写风格
## 交互

- 使用 submit_translation 函数，传入 file_name + ini_content 一次性提交整个文件的简体中文翻译
- ini_content 应为标准 INI 格式文本，保留原文的 section 结构和 key，只将值翻译为中文存在 value 中
- 如果文件过大，可以按 section 分批提交（传入 section + entries），单个 section 也可以划分为多次函数调用提交
- 仅在遇到模组独创的虚构名称或专属概念时，使用 submit_glossary 函数提交其翻译（格式为 term + translation，可选 reason）；不要提交原版已有词条、未翻译的英文原词或依赖具体语境的常见普通英文词汇（如 angel、hauler、worker、mail 等）"#;

// ══════════════════════════════════════════════════════════════════════════════
// Step 3: 构建 Function Calling 的工具定义
// ══════════════════════════════════════════════════════════════════════════════

/// 创建翻译提交的 ToolObject 列表。
///
/// 支持两种提交模式（LLM 可自行选择）：
/// 1. 整文件模式：传入 `file_name` + `ini_content`（推荐，一次性提交整个文件）
/// 2. 按 section 模式：传入 `file_name` + `section` + `entries`（细粒度）
fn make_translation_tools() -> Vec<ToolObject> {
    let parameters = serde_json::from_str(
        r#"{
        "type": "object",
        "properties": {
            "file_name": {
                "type": "string",
                "description": "翻译文件名，例如 'base.cfg'"
            },
            "ini_content": {
                "type": "string",
                "description": "完整的 INI 格式翻译文本。保留 section 结构和 key，只将 value 翻译为中文。推荐使用此方式一次性提交整个文件。这项参数不为空时会忽略 section 和 entries。如\n\n```ini\n[item-name]\niron-plate=铁板\ncopper-plate=铜板\n```\n"
            },
            "section": {
                "type": "string",
                "description": "INI section 名称，例如 'entity-name'。仅在按 section 分批提交时使用，与 entries 配合。仅当 ini_content 为空时有效。"
            },
            "entries": {
                "type": "array",
                "description": "该 section 下的翻译条目。仅在按 section 分批提交时使用，与 section 配合。仅当 ini_content 为空时有效。",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": {
                            "type": "string",
                            "description": "翻译键（key）"
                        },
                        "translation": {
                            "type": "string",
                            "description": "中文翻译（value）"
                        }
                    },
                    "required": ["key", "translation"]
                }
            }
        },
        "required": ["file_name"]
    }"#,
    )
    .expect("内置 JSON Schema 格式错误");

    let glossary_parameters = serde_json::from_str(
        r#"{
        "type": "object",
        "properties": {
            "term": {
                "type": "string",
                "description": "英文专有名词/虚构术语"
            },
            "translation": {
                "type": "string",
                "description": "该术语的中文翻译"
            },
            "reason": {
                "type": "string",
                "description": "简要说明为什么认为这是专有名词（可选）"
            }
        },
        "required": ["term", "translation"]
    }"#,
    )
    .expect("内置 Glossary JSON Schema 格式错误");

    vec![
        ToolObject {
            tool_type: ToolType::Function,
            function: Function {
                name: "submit_translation".to_string(),
                description: "提交翻译结果。支持两种方式：1) 传入 file_name + ini_content 一次性提交整个文件的 INI 翻译；2) 传入 file_name + section + entries 按 section 分批提交。"
                    .to_string(),
                parameters,
            },
        },
        ToolObject {
            tool_type: ToolType::Function,
            function: Function {
                name: "submit_glossary".to_string(),
                description: "按当前模组及其下游依赖作用域提交专有名词/虚构概念的翻译。仅用于模组独创的虚构名称（星球、独特派系/科技/装备名），不要提交随上下文变化的普通英文词汇或未翻译的原词。".to_string(),
                parameters: glossary_parameters,
            },
        },
    ]
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 4: 构建发送给 LLM 的提示词
// ══════════════════════════════════════════════════════════════════════════════

/// 构建用户提示词内容。
///
/// 包含：
/// 1. 术语对照表（原版及依赖模组专有术语，作为参考）
/// 2. 如果提供了上次的翻译文件，附上仅 zh-CN 且限长的风格参考
/// 3. 需要翻译的内容（按文件分开的 INI 差异）
pub fn build_user_prompt(
    base_locale: &ini::Ini,
    file_diffs: &BTreeMap<String, ini::Ini>,
    previous_translations: Option<&LocaleInfo>,
) -> anyhow::Result<String> {
    let mut prompt = String::new();

    // 术语对照表
    if !base_locale.is_empty() {
        prompt.push_str("## 术语对照参考\n\n");
        prompt.push_str("以下为本次任务涉及的常见与专有术语翻译，请保持翻译一致性：\n\n");
        prompt.push_str("```ini\n");
        prompt.push_str(&translation::ini_to_str(base_locale)?);
        prompt.push_str("```\n\n");
    }

    // 上次的 zh-CN 翻译（仅作风格参考，最多保留 100 行非空内容）
    if let Some(prev) = previous_translations
        && let Some(zh_info) = prev.contents.get(TARGET_LANG)
        && !zh_info.contents.is_empty()
    {
        const MAX_PREV_LINES: usize = 100;
        let mut snippet = String::new();
        let mut line_count = 0usize;
        for (file_name, content) in &zh_info.contents {
            if line_count >= MAX_PREV_LINES {
                break;
            }
            let non_empty: Vec<&str> = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(MAX_PREV_LINES - line_count)
                .collect();
            if !non_empty.is_empty() {
                snippet.push_str(&format!("; 文件: {}\n", file_name));
                for line in &non_empty {
                    snippet.push_str(line);
                    snippet.push('\n');
                }
                line_count += non_empty.len();
            }
        }
        if !snippet.is_empty() {
            prompt.push_str("## 该模组之前的中文翻译（仅供参考风格，节选）\n\n");
            prompt.push_str("```ini\n");
            prompt.push_str(&snippet);
            prompt.push_str("```\n\n");
        }
    }

    // 当前翻译任务 — 按文件分开展示差异 INI
    prompt.push_str("## 当前翻译任务\n\n");
    prompt.push_str(
        "请将以下各文件的 INI 格式英文文本翻译为简体中文，保持 section 结构和 key 不变，只翻译 value：\n\n",
    );

    for (file_name, diff_ini) in file_diffs {
        prompt.push_str(&format!("### 文件: {}\n\n", file_name));
        prompt.push_str("```ini\n");
        prompt.push_str(&translation::ini_to_str(diff_ini)?);
        prompt.push_str("```\n\n");
    }

    prompt.push_str(
        "请为每个文件分别调用 submit_translation 函数，传入 file_name + ini_content 提交该文件的完整翻译。\n",
    );
    Ok(prompt)
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 5: LLM 交互 — 发送翻译请求并收集结果
// ══════════════════════════════════════════════════════════════════════════════

/// LLM 通过 function calling 提交的翻译条目
#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedEntry {
    pub key: String,
    pub original: Option<String>,
    pub translation: String,
}

/// LLM 提交的翻译结果（单个 function call 的数据）。
///
/// 支持两种模式：
/// - 整文件模式：`file_name` + `ini_content`（完整 INI 文本）
/// - 按 section 模式：`file_name` + `section` + `entries`
#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedTranslation {
    pub file_name: String,
    /// 完整的 INI 格式翻译文本（整文件模式）
    #[serde(default)]
    pub ini_content: Option<String>,
    /// INI section 名称（按 section 模式）
    #[serde(default)]
    pub section: Option<String>,
    /// 翻译条目列表（按 section 模式）
    #[serde(default)]
    pub entries: Option<Vec<SubmittedEntry>>,
}

/// LLM 提交的术语条目
#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedGlossaryEntry {
    pub term: String,
    pub translation: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// 聚合流式 tool_calls 增量（按 index 合并 id / name / arguments）
fn aggregate_delta_tool_calls(
    deltas: &[deepseek_api::response::DeltaToolCall],
) -> Vec<deepseek_api::response::ToolCall> {
    let mut map: BTreeMap<usize, (Option<String>, String, String)> = BTreeMap::new();
    for tc in deltas {
        let idx = tc.index.unwrap_or(0);
        let entry = map
            .entry(idx)
            .or_insert((None, String::new(), String::new()));
        if let Some(id) = &tc.id {
            entry.0 = Some(id.clone());
        }
        if let Some(f) = &tc.function {
            if let Some(name) = &f.name {
                entry.1 = name.clone();
            }
            if let Some(args) = &f.arguments {
                entry.2.push_str(args);
            }
        }
    }
    map.into_iter()
        .map(|(_, (id, name, arguments))| deepseek_api::response::ToolCall {
            id: id.unwrap_or_default(),
            tool_type: "function".to_string(),
            function: deepseek_api::response::Function { name, arguments },
        })
        .collect()
}

/// 获取一次 LLM 响应。
///
/// 始终使用流式（SSE）请求：长响应（推理模型的思考 + 超长输出）期间连接持续
/// 活跃，可规避网关超时导致的请求失败/截断。`verbose` 为 `true` 时额外打印
/// 思考内容、文本内容和工具调用增量。
async fn fetch_llm_response(
    client: &deepseek_api::DeepSeekClient,
    messages: &[MessageRequest],
    tools: &[ToolObject],
    verbose: bool,
) -> anyhow::Result<(
    deepseek_api::response::AssistantMessage,
    FinishReason,
)> {
    let chat_resp = CompletionsRequestBuilder::new(messages)
        .tools(tools)
        .stream(true)
        .do_request(client)
        .await
        .context("LLM API 请求失败")?;

    // 流式聚合 + 实时打印
    let mut stream = chat_resp.must_stream();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_deltas: Vec<deepseek_api::response::DeltaToolCall> = Vec::new();
    let mut finish_reason = FinishReason::Stop;

    if verbose {
        println!("\n===== LLM 流式响应 =====");
    }
    while let Some(item) = stream.next().await {
        let chunk = item.context("LLM 流式响应解析失败")?;
        if let Some(choice) = chunk.choices.first() {
            let delta = &choice.delta;
            if let Some(rc) = &delta.reasoning_content
                && !rc.is_empty() {
                    if verbose {
                        print!("{}", rc);
                    }
                    reasoning.push_str(rc);
                }
            if let Some(c) = &delta.content
                && !c.is_empty() {
                    if verbose {
                        print!("{}", c);
                    }
                    content.push_str(c);
                }
            if let Some(tcs) = &delta.tool_calls {
                for tc in tcs {
                    if verbose
                        && let Some(f) = &tc.function
                            && let Some(args) = &f.arguments
                        {
                            print!("{}", args);
                        }
                }
                tool_deltas.extend(tcs.iter().cloned());
            }
            if let Some(fr) = &choice.finish_reason {
                finish_reason = fr.clone();
            }
        }
    }
    if verbose {
        println!(
            "\n===== 流式响应结束 (finish_reason: {:?}) =====",
            finish_reason
        );
    }

    let tool_calls = aggregate_delta_tool_calls(&tool_deltas);
    let assistant_msg = deepseek_api::response::AssistantMessage {
        content,
        reasoning_content: Some(reasoning),
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        ..Default::default()
    };

    // 打印聚合后的工具调用信息
    if verbose
        && let Some(tcs) = &assistant_msg.tool_calls {
            println!("--- 工具调用 ---");
            for tc in tcs {
                println!("  - {} (id={})", tc.function.name, tc.id);
                println!("    arguments: {}", tc.function.arguments);
            }
        }

    Ok((assistant_msg, finish_reason))
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

fn resolve_submitted_file_name(submitted: &str, expected_files: &[String]) -> String {
    if expected_files.iter().any(|f| f == submitted) {
        return submitted.to_string();
    }
    if expected_files.len() == 1 {
        return expected_files[0].clone();
    }
    if let Some(matched) = expected_files
        .iter()
        .find(|f| submitted.ends_with(f.as_str()) || f.eq_ignore_ascii_case(submitted))
    {
        return matched.clone();
    }
    submitted.to_string()
}

/// 将多文件待翻译条目按最大条目数切分为若干批次，防止超大文件在单次工具调用中触发输出上限截断。
pub fn split_file_diffs_into_batches(
    file_diffs: &BTreeMap<String, ini::Ini>,
    max_entries: usize,
) -> Vec<BTreeMap<String, ini::Ini>> {
    let limit = max_entries.max(1);
    let mut batches = Vec::new();
    let mut current_batch: BTreeMap<String, ini::Ini> = BTreeMap::new();
    let mut current_count = 0usize;

    for (file_name, diff_ini) in file_diffs {
        for (sec, props) in diff_ini.iter() {
            for (k, v) in props.iter() {
                if current_count >= limit {
                    batches.push(std::mem::take(&mut current_batch));
                    current_count = 0;
                }
                current_batch
                    .entry(file_name.clone())
                    .or_default()
                    .with_section(sec)
                    .set(k, v);
                current_count += 1;
            }
        }
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }
    batches
}

fn normalize_merged_zh_ini(ini: &mut ini::Ini) {
    if ini
        .section(None::<String>)
        .and_then(|s| s.get("locale-identifier"))
        == Some("en")
    {
        ini.with_general_section()
            .set("locale-identifier", TARGET_LANG);
    }
}

fn merge_with_file_map(
    file_name: &str,
    reference_ini: &ini::Ini,
    old_ini: &ini::Ini,
    llm_by_file: &BTreeMap<String, ini::Ini>,
    known_files: &BTreeSet<String>,
) -> ini::Ini {
    let mut merged = if let Some(exact) = llm_by_file.get(file_name) {
        translation::merge_ini(reference_ini, old_ini, exact)
    } else {
        translation::merge_ini(reference_ini, old_ini, &ini::Ini::new())
    };

    // 若 LLM 偶发写错了文件名（不在 known_files 内），作为兜底补入缺失键
    for (fname, fallback_ini) in llm_by_file {
        if fname != file_name && !known_files.contains(fname) {
            merged = translation::merge_ini(reference_ini, &merged, fallback_ini);
        }
    }
    normalize_merged_zh_ini(&mut merged);
    merged
}

/// 调用 LLM 获取翻译。
///
/// 使用 function calling 机制，LLM 通过多次调用 `submit_translation`
/// 分批提交翻译结果，也可通过 `submit_glossary` 提交专有名词翻译。
///
/// `glossary` 会被原地更新（AI 提交的术语按 `[mod_name]` 分节写入）。
pub async fn call_llm_for_translation(
    client: &deepseek_api::DeepSeekClient,
    mod_name: &str,
    expected_files: &[String],
    system_prompt: &str,
    user_prompt: &str,
    glossary: &mut ini::Ini,
) -> anyhow::Result<BTreeMap<String, ini::Ini>> {
    let tools = make_translation_tools();
    let mut result_by_file: BTreeMap<String, ini::Ini> = BTreeMap::new();
    let mut warned_no_chinese: BTreeSet<String> = BTreeSet::new();
    let mut loop_count = 0;
    let mut parse_error_streak = 0usize;
    const MAX_LOOPS: usize = 256;

    let mut messages: Vec<MessageRequest> = vec![
        MessageRequest::sys(system_prompt),
        MessageRequest::user(user_prompt),
    ];

    while loop_count < MAX_LOOPS {
        loop_count += 1;

        let (assistant_msg, finish_reason) = fetch_llm_response(
            client,
            &messages,
            &tools,
            LLM_VERBOSE.load(std::sync::atomic::Ordering::Relaxed),
        )
        .await?;

        let msg = &assistant_msg;

        if let Some(ref tool_calls) = msg.tool_calls
            && !tool_calls.is_empty()
        {
            messages.push(MessageRequest::Assistant(msg.clone()));

            let mut has_valid_call = false;
            let mut needs_retry = false;

            for tool_call in tool_calls {
                let fn_name = tool_call.function.name.as_str();
                match fn_name {
                    "submit_translation" => {
                        match serde_json::from_str::<SubmittedTranslation>(
                            &tool_call.function.arguments,
                        ) {
                            Ok(submitted) => {
                                let target_file = resolve_submitted_file_name(
                                    &submitted.file_name,
                                    expected_files,
                                );

                                // 模式 1：整文件 INI 文本提交
                                if let Some(ref ini_content) = submitted.ini_content {
                                    match translation::str_to_ini(ini_content) {
                                        Ok(ini) => {
                                            let mut merged_count = 0usize;
                                            let mut has_chinese_count = 0usize;
                                            for (_sec, props) in ini.iter() {
                                                for (_k, v) in props.iter() {
                                                    if contains_cjk(v) {
                                                        has_chinese_count += 1;
                                                    }
                                                    merged_count += 1;
                                                }
                                            }
                                            debug!(
                                                "收到整文件翻译: file={}, sections={}, entries={}, chinese_characters={}",
                                                target_file,
                                                ini.iter().count(),
                                                merged_count,
                                                has_chinese_count
                                            );

                                            has_valid_call = true;
                                            let warn_key = format!("{}:*", target_file);
                                            if has_chinese_count == 0
                                                && merged_count > 0
                                                && warned_no_chinese.insert(warn_key)
                                            {
                                                warn!(
                                                    "整文件翻译中没有检测到中文字符: file={}, entries={:?}",
                                                    target_file, submitted.ini_content
                                                );
                                                warn!(
                                                    "额外参数情况：{:?}, {:?}",
                                                    submitted.section, submitted.entries
                                                );
                                                needs_retry = true;
                                                messages.push(MessageRequest::Tool(
                                                    ToolMessageRequest::new(
                                                        &format!(
                                                            "警告：整文件翻译中没有检测到中文字符，暂未写入。请将英文翻译为简体中文（若原文确为纯符号/代码无需翻译，请再次提交以确认）: {} (共 {} 条)",
                                                            target_file, merged_count
                                                        ),
                                                        &tool_call.id,
                                                    ),
                                                ));
                                            } else {
                                                let file_ini = result_by_file
                                                    .entry(target_file.clone())
                                                    .or_default();
                                                for (sec, props) in ini.iter() {
                                                    for (k, v) in props.iter() {
                                                        file_ini.with_section(sec).set(k, v);
                                                    }
                                                }
                                                messages.push(MessageRequest::Tool(
                                                    ToolMessageRequest::new(
                                                        &format!(
                                                            "已收到 {} 的整文件翻译（{} 条）",
                                                            target_file, merged_count
                                                        ),
                                                        &tool_call.id,
                                                    ),
                                                ));
                                            }
                                        }
                                        Err(e) => {
                                            warn!("解析整文件 INI 失败: file={}, err={}", target_file, e);
                                            needs_retry = true;
                                            messages.push(MessageRequest::Tool(
                                                ToolMessageRequest::new(
                                                    &format!("INI 解析失败: {}，请检查格式后重新提交", e),
                                                    &tool_call.id,
                                                ),
                                            ));
                                        }
                                    }
                                }
                                // 模式 2：按 section 提交
                                else if let (Some(section), Some(entries)) =
                                    (&submitted.section, &submitted.entries)
                                {
                                    let mut merged_count = 0usize;
                                    let mut has_chinese_count = 0usize;
                                    for entry in entries {
                                        if contains_cjk(&entry.translation) {
                                            has_chinese_count += 1;
                                        }
                                        merged_count += 1;
                                    }
                                    debug!(
                                        "收到翻译: file={}, section={}, entries={}",
                                        target_file, section, merged_count
                                    );
                                    has_valid_call = true;
                                    let warn_key = format!("{}:{}", target_file, section);
                                    if has_chinese_count == 0
                                        && merged_count > 0
                                        && warned_no_chinese.insert(warn_key)
                                    {
                                        warn!(
                                            "翻译中没有检测到中文字符: file={}, section={}, entries={:?}",
                                            target_file, section, entries
                                        );
                                        needs_retry = true;
                                        messages.push(MessageRequest::Tool(
                                            ToolMessageRequest::new(
                                                &format!(
                                                    "警告：翻译中没有检测到中文字符，暂未写入。请检查并翻译为简体中文（若原文确为纯符号/代码无需翻译，请再次提交以确认）: {} 下 {} 条",
                                                    section, merged_count
                                                ),
                                                &tool_call.id,
                                            ),
                                        ));
                                    } else {
                                        let sec_opt = if section.is_empty() {
                                            None
                                        } else {
                                            Some(section.as_str())
                                        };
                                        let file_ini =
                                            result_by_file.entry(target_file.clone()).or_default();
                                        for entry in entries {
                                            let unescaped = translation::unescape_submitted_value(
                                                &entry.translation,
                                            );
                                            file_ini
                                                .with_section(sec_opt)
                                                .set(&entry.key, unescaped);
                                        }
                                        messages.push(MessageRequest::Tool(
                                            ToolMessageRequest::new(
                                                &format!(
                                                    "已收到 {} 的 {} 下 {} 条翻译",
                                                    target_file, section, merged_count
                                                ),
                                                &tool_call.id,
                                            ),
                                        ));
                                    }
                                } else {
                                    warn!(
                                        "翻译提交格式无效: file={}, 缺少 ini_content 或 (section+entries)",
                                        submitted.file_name
                                    );
                                    needs_retry = true;
                                    messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                        "格式无效：请提供 ini_content（整文件）或 section+entries（按 section）",
                                        &tool_call.id,
                                    )));
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "解析翻译数据失败: {} — 原始参数: {}",
                                    e, tool_call.function.arguments
                                );
                                needs_retry = true;
                                messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                    &format!("解析失败: {}", e),
                                    &tool_call.id,
                                )));
                            }
                        }
                    }
                    "submit_glossary" => {
                        match serde_json::from_str::<SubmittedGlossaryEntry>(
                            &tool_call.function.arguments,
                        ) {
                            Ok(entry) => {
                                let term = entry.term.trim();
                                let trans = entry.translation.trim();
                                debug!(
                                    "AI 提交术语 [{}]: {} → {} ({})",
                                    mod_name,
                                    term,
                                    trans,
                                    entry.reason.as_deref().unwrap_or("无理由")
                                );
                                has_valid_call = true;
                                if term.is_empty()
                                    || trans.is_empty()
                                    || term.eq_ignore_ascii_case(trans)
                                {
                                    messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                        "术语无需记录（与原文相同或为空）",
                                        &tool_call.id,
                                    )));
                                } else {
                                    let existing =
                                        glossary.section(Some(mod_name)).and_then(|props| {
                                            props
                                                .iter()
                                                .find(|(k, _)| k.eq_ignore_ascii_case(term))
                                                .map(|(k, v)| (k.to_string(), v.to_string()))
                                        });
                                    if let Some((existing_key, old)) = existing {
                                        if old == trans {
                                            messages.push(MessageRequest::Tool(
                                                ToolMessageRequest::new(
                                                    &format!(
                                                        "术语已存在: {} → {}",
                                                        existing_key, old
                                                    ),
                                                    &tool_call.id,
                                                ),
                                            ));
                                        } else {
                                            debug!(
                                                "保留模组 [{}] 既有术语: {} → {} (忽略新建议: {})",
                                                mod_name, existing_key, old, trans
                                            );
                                            messages.push(MessageRequest::Tool(
                                                ToolMessageRequest::new(
                                                    &format!(
                                                        "已保留当前模组既有术语 {}: {}",
                                                        existing_key, old
                                                    ),
                                                    &tool_call.id,
                                                ),
                                            ));
                                        }
                                    } else {
                                        glossary.with_section(Some(mod_name)).set(term, trans);
                                        messages.push(MessageRequest::Tool(
                                            ToolMessageRequest::new(
                                                &format!(
                                                    "术语已记录到 [{}]: {} → {}",
                                                    mod_name, term, trans
                                                ),
                                                &tool_call.id,
                                            ),
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("解析术语数据失败: {}", e);
                                needs_retry = true;
                                messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                    &format!("解析术语失败: {}", e),
                                    &tool_call.id,
                                )));
                            }
                        }
                    }
                    _ => {
                        warn!("LLM 调用了未知函数: {}", fn_name);
                        needs_retry = true;
                        messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                            &format!("未知函数: {}", fn_name),
                            &tool_call.id,
                        )));
                    }
                }
            }

            if !has_valid_call {
                parse_error_streak += 1;
                if parse_error_streak >= 3 && finish_reason == FinishReason::Stop {
                    warn!("连续多次工具调用解析失败且模型已停止，退出循环");
                    break;
                }
                continue;
            }
            parse_error_streak = 0;

            if finish_reason == FinishReason::Stop && !needs_retry {
                break;
            }

            let next_prompt = if needs_retry {
                "请根据上述警告检查并重新提交或确认翻译。如果所有翻译已完成，请只回复'所有翻译已完成'。"
            } else {
                "请继续提交剩余的翻译条目，或调用 stop 完成。如果所有翻译已完成，请只回复'所有翻译已完成'。"
            };
            messages.push(MessageRequest::user(next_prompt));
            continue;
        }

        // 没有 tool_calls：如果已有 assistant 文本回复，结束；否则是纯文本响应
        if !msg.content.is_empty() {
            messages.push(MessageRequest::Assistant(msg.clone()));
            debug!("LLM 最终回复: {}", msg.content);
        }
        if finish_reason == FinishReason::Stop {
            break;
        }
    }

    if loop_count >= MAX_LOOPS {
        warn!("LLM 交互达到最大循环次数 ({})，返回已收集的翻译", MAX_LOOPS);
    }

    Ok(result_by_file)
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 6: 处理单个 mod
// ══════════════════════════════════════════════════════════════════════════════

/// 翻译源语言 → 目标语言（目前写死 en → zh-CN）
const SOURCE_LANG: &str = "en";
const TARGET_LANG: &str = "zh-CN";

/// 处理单个 mod 的翻译流程。
///
/// 给定 mod 的名称，从本地缓存加载上次翻译，下载 mod 并提取翻译文件，
/// 计算差异，调用 LLM 获取翻译，合并保存。
///
/// 当前固定从 en 翻译到 zh-CN：
/// - 如果 mod 只有 zh-CN 没有 en → 中文优先 mod，跳过
/// - 如果 mod 没有 en → 无法翻译，跳过
///
/// 流式下载（自动使用全局进度条，完成后自动消失）
async fn stream_download(resp: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    let total = resp.content_length().unwrap_or(0);
    let pb = crate::progress::MULTI.add(indicatif::ProgressBar::new(total));
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{bar:40.green/white} {bytes}/{total_bytes} 下载中",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    let mut data = Vec::with_capacity(total as usize);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| anyhow::anyhow!("下载错误: {}", e.without_url()))?;
        pb.inc(chunk.len() as u64);
        data.extend_from_slice(&chunk);
    }
    pb.finish_and_clear();
    Ok(data)
}

#[allow(clippy::too_many_arguments)]
async fn process_mod(
    client_fa: &FactorioWebClient,
    client_deepseek: &deepseek_api::DeepSeekClient,
    config: &FlowConfig,
    base_locale: &ini::Ini,
    system_prompt: &str,
    glossary: &mut ini::Ini,
    mod_name: &str,
    download_url: &str,
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

    // 2. 下载 mod 并提取翻译文件（流式 + 进度条）
    let resp = client_fa.download_mod_response(download_url).await?;
    let zip_data = stream_download(resp).await?;

    let current_locale = extract_locale_from_zip(&zip_data)
        .with_context(|| format!("无法从 {} 的 zip 中提取翻译文件", mod_name))?;

    let lang_count = current_locale.contents.len();
    if lang_count == 0 {
        info!("  ↳ mod 没有翻译文件，跳过");
        return Ok(());
    }
    info!("  ↳ 提取到 {} 种语言的翻译文件", lang_count);

    // 3.5 检查源语言和目标语言
    let has_source = current_locale.contents.contains_key(SOURCE_LANG);
    let has_target = current_locale.contents.contains_key(TARGET_LANG);

    if !has_source {
        if has_target {
            info!("  ↳ 中文优先 mod（无 en 但有 zh-CN），跳过");
        } else {
            info!("  ↳ mod 没有源语言（{}）翻译文件，跳过", SOURCE_LANG);
        }
        return Ok(());
    }

    let source_lang_info = &current_locale.contents[SOURCE_LANG];
    let source_ini_by_file: BTreeMap<String, ini::Ini> = source_lang_info
        .contents
        .iter()
        .map(|(fname, content)| {
            translation::str_to_ini(content).map(|ini| (fname.clone(), ini))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let known_source_files: BTreeSet<String> = source_ini_by_file.keys().cloned().collect();

    // 旧的 zh-CN 缓存（用于差异计算、上下文参考和合并）
    let mut old_target_ini_by_file: BTreeMap<String, ini::Ini> = cached_locale
        .as_ref()
        .and_then(|c| c.contents.get(TARGET_LANG))
        .map(|lang_info| {
            lang_info
                .contents
                .iter()
                .filter_map(|(fname, content)| {
                    translation::str_to_ini(content)
                        .ok()
                        .map(|ini| (fname.clone(), ini))
                })
                .collect()
        })
        .unwrap_or_default();

    // 合并模组自带 zh-CN（仅保留真正含中文且不同于英文原文的词条，且自带优先于旧缓存）
    let mut builtin_zh_by_file: BTreeMap<String, ini::Ini> = BTreeMap::new();
    if let Some(builtin_target) = current_locale.contents.get(TARGET_LANG) {
        for (file_name, builtin_content) in &builtin_target.contents {
            let Ok(builtin_ini) = translation::str_to_ini(builtin_content) else {
                continue;
            };
            let en_ini = source_ini_by_file.get(file_name);
            let mut valid_ini = ini::Ini::new();
            for (sec, props) in builtin_ini.iter() {
                for (k, v) in props.iter() {
                    let en_val = en_ini.and_then(|e| e.section(sec)).and_then(|p| p.get(k));
                    if contains_cjk(v) && en_val != Some(v) {
                        valid_ini.with_section(sec).set(k, v);
                    }
                }
            }
            if !valid_ini.is_empty() {
                let target = old_target_ini_by_file.entry(file_name.clone()).or_default();
                for (sec, props) in valid_ini.iter() {
                    for (k, v) in props.iter() {
                        target.with_section(sec).set(k, v);
                    }
                }
                builtin_zh_by_file.insert(file_name.clone(), valid_ini);
            }
        }
    }

    // 旧的 en 原文（用于 diff 计算：比较新旧 en，只翻译变更部分）
    let old_source_ini_by_file: BTreeMap<String, ini::Ini> = cached_locale
        .as_ref()
        .and_then(|c| c.contents.get(SOURCE_LANG))
        .map(|lang_info| {
            lang_info
                .contents
                .iter()
                .filter_map(|(fname, content)| {
                    translation::str_to_ini(content)
                        .ok()
                        .map(|ini| (fname.clone(), ini))
                })
                .collect()
        })
        .unwrap_or_default();

    // 4. 构建自愈式待翻译任务：(new_en vs old_en) ∪ (current_en 中缺失 zh-CN 的键) − 自带有效 zh-CN
    let mut file_diffs: BTreeMap<String, ini::Ini> = BTreeMap::new();
    let mut total_entries = 0usize;

    for (file_name, current_ini) in &source_ini_by_file {
        let en_diff = if let Some(old_source_ini) = old_source_ini_by_file.get(file_name) {
            translation::diff_ini(old_source_ini, current_ini)
        } else {
            current_ini.clone()
        };
        let missing_zh = if let Some(old_target_ini) = old_target_ini_by_file.get(file_name) {
            translation::diff_ini_keys_only(old_target_ini, current_ini)
        } else {
            current_ini.clone()
        };
        let builtin_file_ini = builtin_zh_by_file.get(file_name);

        let mut diff = ini::Ini::new();
        for (sec, props) in current_ini.iter() {
            let in_en_diff = en_diff.section(sec);
            let in_missing = missing_zh.section(sec);
            let in_builtin = builtin_file_ini.and_then(|b| b.section(sec));
            for (k, v) in props.iter() {
                if in_builtin.and_then(|p| p.get(k)).is_some() {
                    continue;
                }
                let changed = in_en_diff.and_then(|p| p.get(k)).is_some();
                let missing = in_missing.and_then(|p| p.get(k)).is_some();
                if changed || missing {
                    diff.with_section(sec).set(k, v);
                }
            }
        }

        if diff.is_empty() {
            continue;
        }
        total_entries += diff.iter().flat_map(|(_, props)| props.iter()).count();
        file_diffs.insert(file_name.clone(), diff);
    }

    let deps_changed = cached_locale
        .as_ref()
        .map(|c| c.dependencies != current_locale.dependencies)
        .unwrap_or(true);

    if file_diffs.is_empty() || total_entries == 0 {
        if cached_locale.is_none() || !builtin_zh_by_file.is_empty() || deps_changed {
            let mut merged_target_lang = LangInfo {
                contents: indexmap::IndexMap::new(),
            };
            for (file_name, reference_ini) in &source_ini_by_file {
                let old_ini = old_target_ini_by_file
                    .get(file_name)
                    .cloned()
                    .unwrap_or_default();
                let mut merged_ini =
                    translation::merge_ini(reference_ini, &old_ini, &ini::Ini::new());
                normalize_merged_zh_ini(&mut merged_ini);
                merged_target_lang
                    .contents
                    .insert(file_name.clone(), translation::ini_to_str(&merged_ini)?);
            }
            let mut merged_locale = cached_locale.unwrap_or_else(|| LocaleInfo {
                contents: indexmap::IndexMap::new(),
                version: String::new(),
                dependencies: Vec::new(),
            });
            merged_locale.version = current_locale.version.clone();
            merged_locale.dependencies = current_locale.dependencies.clone();
            merged_locale
                .contents
                .insert(SOURCE_LANG.to_string(), source_lang_info.clone());
            merged_locale
                .contents
                .insert(TARGET_LANG.to_string(), merged_target_lang);
            std::fs::create_dir_all(&config.cache_dir)?;
            let cache_file_path = config.cache_dir.join(format!("{}.json", mod_name));
            let _persistent = persistent_via_file(merged_locale, &cache_file_path);
            info!("  ↳ 无新增待译词条，已同步自带译文/依赖缓存，跳过 LLM");
        } else {
            info!("  ↳ 没有需要翻译的新内容，跳过");
        }
        return Ok(());
    }

    info!(
        "  ↳ 需要翻译 {} 个文件 / {} 个条目 ({} → {})",
        file_diffs.len(),
        total_entries,
        SOURCE_LANG,
        TARGET_LANG
    );

    // 5. 构建上下文参考（仅保留已合并自带译文的 zh-CN 供风格参考）与按依赖链筛选的术语表
    let dep_chain =
        resolve_mod_dependency_chain(mod_name, &current_locale.dependencies, &config.cache_dir);
    let style_locale = if old_target_ini_by_file.is_empty() {
        None
    } else {
        let mut style_zh = LangInfo {
            contents: indexmap::IndexMap::new(),
        };
        for (file_name, ini) in &old_target_ini_by_file {
            if let Ok(s) = translation::ini_to_str(ini) {
                style_zh.contents.insert(file_name.clone(), s);
            }
        }
        let mut contents = indexmap::IndexMap::new();
        contents.insert(TARGET_LANG.to_string(), style_zh);
        Some(LocaleInfo {
            contents,
            version: current_locale.version.clone(),
            dependencies: current_locale.dependencies.clone(),
        })
    };

    const MAX_BATCH_ENTRIES: usize = 120;
    let batches = split_file_diffs_into_batches(&file_diffs, MAX_BATCH_ENTRIES);
    let mut llm_translation_by_file: BTreeMap<String, ini::Ini> = BTreeMap::new();

    for (batch_idx, batch_diffs) in batches.iter().enumerate() {
        let mod_glossary =
            build_mod_glossary(base_locale, glossary, &dep_chain, batch_diffs);
        let user_prompt =
            build_user_prompt(&mod_glossary, batch_diffs, style_locale.as_ref())?;
        let expected_files: Vec<String> = batch_diffs.keys().cloned().collect();

        if batches.len() > 1 {
            info!(
                "  ↳ 调用 LLM 进行分批翻译 ({}/{})...",
                batch_idx + 1,
                batches.len()
            );
        } else {
            info!("  ↳ 调用 LLM 进行翻译...");
        }

        let batch_result = call_llm_for_translation(
            client_deepseek,
            mod_name,
            &expected_files,
            system_prompt,
            &user_prompt,
            glossary,
        )
        .await?;

        for (fname, ini) in batch_result {
            let entry = llm_translation_by_file.entry(fname).or_default();
            for (sec, props) in ini.iter() {
                for (k, v) in props.iter() {
                    entry.with_section(sec).set(k, v);
                }
            }
        }
    }

    if llm_translation_by_file.is_empty() {
        anyhow::bail!("LLM 未返回任何翻译结果");
    }
    info!(
        "  ↳ LLM 返回了 {} 个文件的翻译",
        llm_translation_by_file.len()
    );

    // 6. 合并翻译：以 en 原文为基准，LLM 翻译优先，旧 zh-CN（含自带 zh-CN）兜底
    let mut merged_target_lang = LangInfo {
        contents: indexmap::IndexMap::new(),
    };

    for (file_name, reference_ini) in &source_ini_by_file {
        let old_ini = old_target_ini_by_file
            .get(file_name)
            .cloned()
            .unwrap_or_default();
        let merged_ini = merge_with_file_map(
            file_name,
            reference_ini,
            &old_ini,
            &llm_translation_by_file,
            &known_source_files,
        );
        merged_target_lang
            .contents
            .insert(file_name.clone(), translation::ini_to_str(&merged_ini)?);
    }

    // 7. 完整性检查：逐文件对比 en 原文，找出遗漏的 key 并让 AI 分批补充
    const MAX_INTEGRITY_ROUNDS: usize = 2;
    for round in 0..MAX_INTEGRITY_ROUNDS {
        let mut missing_diffs: BTreeMap<String, ini::Ini> = BTreeMap::new();
        for (file_name, reference_ini) in &source_ini_by_file {
            let merged_ini = merged_target_lang
                .contents
                .get(file_name)
                .map(|s| translation::str_to_ini(s))
                .transpose()?
                .unwrap_or_default();
            let missing = translation::diff_ini_keys_only(&merged_ini, reference_ini);
            if !missing.is_empty() {
                missing_diffs.insert(file_name.clone(), missing);
            }
        }
        let missing_count: usize = missing_diffs
            .values()
            .flat_map(|ini| ini.iter().flat_map(|(_, p)| p.iter()))
            .count();
        if missing_diffs.is_empty() || missing_count == 0 {
            info!("  ↳ 完整性检查通过 (第 {} 轮)", round + 1);
            break;
        }
        info!(
            "  ↳ 发现 {} 个遗漏条目 (第 {} 轮)，让 AI 补充...",
            missing_count,
            round + 1
        );
        debug!("  ↳ 遗漏内容详细信息: {:?}", missing_diffs);

        let missing_batches = split_file_diffs_into_batches(&missing_diffs, MAX_BATCH_ENTRIES);
        let mut supplement_by_file: BTreeMap<String, ini::Ini> = BTreeMap::new();
        for missing_batch in &missing_batches {
            let mod_glossary =
                build_mod_glossary(base_locale, glossary, &dep_chain, missing_batch);
            let supplement_prompt = format!(
                "## 补充翻译任务\n\n以下键在上轮翻译中被遗漏，请补充翻译：\n\n{}",
                build_user_prompt(&mod_glossary, missing_batch, None)?
            );
            let expected_files: Vec<String> = missing_batch.keys().cloned().collect();
            let batch_result = call_llm_for_translation(
                client_deepseek,
                mod_name,
                &expected_files,
                system_prompt,
                &supplement_prompt,
                glossary,
            )
            .await?;
            for (fname, ini) in batch_result {
                let entry = supplement_by_file.entry(fname).or_default();
                for (sec, props) in ini.iter() {
                    for (k, v) in props.iter() {
                        entry.with_section(sec).set(k, v);
                    }
                }
            }
        }
        if supplement_by_file.is_empty() {
            warn!("  ↳ AI 未返回补充翻译，停止完整性检查");
            break;
        }
        for (file_name, reference_ini) in &source_ini_by_file {
            let old_ini = merged_target_lang
                .contents
                .get(file_name)
                .map(|s| translation::str_to_ini(s))
                .transpose()?
                .unwrap_or_default();
            let merged_ini = merge_with_file_map(
                file_name,
                reference_ini,
                &old_ini,
                &supplement_by_file,
                &known_source_files,
            );
            merged_target_lang
                .contents
                .insert(file_name.clone(), translation::ini_to_str(&merged_ini)?);
        }
    }

    // 构建最终 LocaleInfo：保留其他语言不变，更新 zh-CN、en 原文和依赖列表
    let mut merged_locale = cached_locale.unwrap_or_else(|| LocaleInfo {
        contents: indexmap::IndexMap::new(),
        version: String::new(),
        dependencies: Vec::new(),
    });
    merged_locale.version = current_locale.version.clone();
    merged_locale.dependencies = current_locale.dependencies.clone();
    merged_locale
        .contents
        .insert(SOURCE_LANG.to_string(), source_lang_info.clone());
    merged_locale
        .contents
        .insert(TARGET_LANG.to_string(), merged_target_lang);

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
/// - `mod_names`: 手动指定的 mod 名称列表（None = 自动获取更新的 mod）
///
/// ## 流程
///
/// 1. 初始化 Factorio 和 DeepSeek 客户端
/// 2. 加载外部参考文件
/// 3. 获取待处理的 mod 列表（自动模式合并增量更新和历史失败）
/// 4. 逐个处理每个 mod
/// 5. 保存失败队列，更新 last_run 时间（仅无限制自动模式）
pub async fn run_translation_pipeline(
    config: FlowConfig,
    since: Option<DateTime<Utc>>,
    limit: Option<usize>,
    mod_names: Option<&[String]>,
) -> anyhow::Result<()> {
    // 加载上次运行时间
    let state_path = config.cache_dir.join(".pipeline_state");
    let now = Utc::now();
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
    let base_locale = load_base_locale(&config)?;
    let system_prompt = load_system_prompt(&config.system_prompt_path)?;

    // 加载/创建 AI 术语表（跨模组专有名词翻译，AI 可提交新条目）
    let glossary_path = config.cache_dir.join(".ai_glossary");
    let mut ai_glossary: ini::Ini = if glossary_path.exists() {
        std::fs::read_to_string(&glossary_path)
            .ok()
            .and_then(|s| translation::str_to_ini(&s).ok())
            .unwrap_or_default()
    } else {
        ini::Ini::new()
    };

    info!("外部参考文件加载完成");

    // 获取待处理的 mod 列表
    let updated_mods: Vec<factorio_api::ResultEntry> = if let Some(names) = mod_names {
        // 手动指定 mod
        if names.is_empty() {
            info!("未指定任何 mod，退出");
            return Ok(());
        }
        let name_strs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let results = fa_client
            .mods_by_names(&name_strs)
            .await
            .context("获取指定 mod 信息失败")?;
        let found: BTreeSet<&str> = results.iter().map(|m| m.name.as_str()).collect();
        for name in names {
            if !found.contains(name.as_str()) {
                state.record_mod_result(name, false);
            }
        }
        info!("手动指定 {} 个 mod，找到 {} 个", names.len(), results.len());
        results
    } else {
        // 自动获取更新的 mod
        let page_size = limit.map(|l| l.min(100) as u64);
        let max_size = limit.map(|l| l as u64);
        let mods = fa_client
            .get_mods_updated_since(effective_since, &config.game_version, page_size, max_size)
            .await
            .context("获取更新的 mod 列表失败")?;
        info!("发现 {} 个更新的 mod", mods.len());
        let updated_names: BTreeSet<&str> = mods.iter().map(|m| m.name.as_str()).collect();
        let retry_names: Vec<&str> = state
            .failed_mods
            .iter()
            .map(String::as_str)
            .filter(|name| !updated_names.contains(name))
            .collect();
        let retries = if retry_names.is_empty() {
            Vec::new()
        } else {
            // mods_by_names 对单个查询错误仅记日志；原失败队列保留这些名称。
            let retries = fa_client
                .mods_by_names(&retry_names)
                .await
                .context("获取待重试 mod 信息失败")?;
            info!(
                "待重试 {} 个 mod，找到 {} 个",
                retry_names.len(),
                retries.len()
            );
            retries
        };
        merge_mod_candidates(mods, retries)
            .into_iter()
            .take(limit.unwrap_or(usize::MAX))
            .collect()
    };

    let pb = crate::progress::new_bar();
    pb.set_length(updated_mods.len() as u64);

    let is_auto = mod_names.is_none();
    let mut processed = 0;
    let mut failed = Vec::new();
    for mod_entry in &updated_mods {
        pb.set_message(format!("处理: {}", mod_entry.name));
        if is_auto
            && let Some(limit) = limit
            && processed >= limit
        {
            info!("已达到处理上限 ({})，停止", limit);
            break;
        }
        if mod_entry
            .category
            .as_deref()
            .is_some_and(|s| s == "localizations")
        {
            info!("跳过 mod {}（localizations 分类）", mod_entry.name);
            state.failed_mods.remove(&mod_entry.name);
            pb.inc(1);
            continue;
        }

        // 提取下载 URL（搜索结果的 latest_release 已合成，mods_by_names 的来自 API）
        let dl_url = mod_entry
            .latest_release
            .as_ref()
            .map(|r| r.download_url.as_str())
            .or_else(|| {
                mod_entry
                    .releases
                    .as_ref()
                    .and_then(|rs| rs.last())
                    .map(|r| r.download_url.as_str())
            });

        let Some(dl_url) = dl_url else {
            warn!("mod {} 没有可下载的版本，跳过", mod_entry.name);
            state.record_mod_result(&mod_entry.name, false);
            failed.push(mod_entry.name.clone());
            pb.inc(1);
            continue;
        };

        match process_mod(
            &fa_client,
            &deepseek_client,
            &config,
            &base_locale,
            &system_prompt,
            &mut ai_glossary,
            &mod_entry.name,
            dl_url,
        )
        .await
        {
            Ok(()) => {
                state.record_mod_result(&mod_entry.name, true);
                processed += 1;
                pb.set_message(format!("✓ {}", mod_entry.name));
                pb.inc(1);
            }
            Err(e) => {
                error!("处理 mod {} 失败: {:?}", mod_entry.name, e);
                state.record_mod_result(&mod_entry.name, false);
                failed.push(mod_entry.name.clone());
                pb.set_message(format!("✗ {}", mod_entry.name));
                pb.inc(1);
                // 继续处理下一个
            }
        }
    }

    // 有限运行可能尚未获取/处理全部增量，不能推进时间戳，否则会漏掉未处理项。
    if is_auto && limit.is_none() {
        state.last_run = now;
        info!(
            "翻译管道完成 — 处理了 {}/{} 个 mod，last_run 更新为: {}",
            processed,
            updated_mods.len(),
            now.format("%Y-%m-%d %H:%M:%S")
        );
    } else {
        info!("翻译完成 — 处理了 {} 个 mod", processed);
    }

    // 手动翻译也更新失败队列，但不推进自动增量时间戳。
    std::fs::create_dir_all(&config.cache_dir)?;
    std::fs::write(&state_path, serde_json::to_vec_pretty(&state)?)
        .with_context(|| format!("无法保存管道状态: {:?}", state_path))?;

    if failed.is_empty() && state.failed_mods.is_empty() {
        info!("所有 mod 处理成功");
    } else if failed.is_empty() {
        warn!("仍有 {} 个 mod 待下次重试", state.failed_mods.len());
    } else {
        warn!("以下 mod 处理失败: {:?}", failed);
    }
    // 保存 AI 术语表
    {
        let glossary_str = translation::ini_to_str(&ai_glossary)?;
        std::fs::write(&glossary_path, &glossary_str)
            .with_context(|| format!("无法保存 AI 术语表: {:?}", glossary_path))?;
    }

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

    #[test]
    fn test_pipeline_state_legacy_and_retry_lifecycle() {
        let mut state: super::PipelineState =
            serde_json::from_str(r#"{"last_run":"2026-10-31T23:00:00Z"}"#).unwrap();
        assert!(state.failed_mods.is_empty());
        let last_run = state.last_run;

        state.record_mod_result("failed", false);
        state.record_mod_result("failed", false);
        state.record_mod_result("unresolved", false);
        // 模拟跨会话序列化与重新加载。
        let mut state: super::PipelineState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(state.failed_mods.len(), 2);
        state.record_mod_result("failed", true);
        assert_eq!(state.failed_mods, ["unresolved".to_owned()].into());
        assert_eq!(state.last_run, last_run);
    }

    fn candidate(name: &str, version: &str) -> factorio_api::ResultEntry {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "title": name,
            "owner": "test",
            "summary": "",
            "downloads_count": 0,
            "latest_release": {
                "download_url": format!("/download/{name}/{version}"),
                "file_name": format!("{name}_{version}.zip"),
                "info_json": { "factorio_version": "2.1" },
                "released_at": "2026-10-31T23:00:00Z",
                "version": version,
                "sha1": ""
            }
        }))
        .unwrap()
    }

    #[test]
    fn test_pipeline_candidates_union_prefers_updated_version() {
        let mods = super::merge_mod_candidates(
            vec![candidate("both", "2.0.0"), candidate("new", "1.0.0")],
            vec![candidate("both", "1.0.0"), candidate("retry", "1.0.0")],
        );
        assert_eq!(
            mods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["both", "new", "retry"]
        );
        assert_eq!(mods[0].latest_release.as_ref().unwrap().version, "2.0.0");
    }

    #[test]
    fn test_pipeline_candidates_retry_without_updates() {
        let mods = super::merge_mod_candidates(
            Vec::new(),
            vec![candidate("retry", "1.0.0"), candidate("retry", "1.0.0")],
        );
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name, "retry");
    }

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

    /// 创建包含 locale 文件和 info.json 的测试 zip
    fn make_test_zip(prefix: &str) -> Vec<u8> {
        let cursor = std::io::Cursor::new(Vec::new());
        let mut zip_writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip_writer
            .start_file(format!("{prefix}locale/en/base.cfg"), options)
            .unwrap();
        zip_writer
            .write_all(b"[entity-name]\niron-plate=Iron plate\ncopper-plate=Copper plate\n")
            .unwrap();

        zip_writer
            .start_file(format!("{prefix}locale/zh-CN/base.cfg"), options)
            .unwrap();
        zip_writer
            .write_all("[entity-name]\niron-plate=铁板\n".as_bytes())
            .unwrap();

        zip_writer
            .start_file(format!("{prefix}info.json"), options)
            .unwrap();
        zip_writer
            .write_all(b"{\"name\":\"test-mod\",\"version\":\"1.0.0\"}")
            .unwrap();

        zip_writer.finish().unwrap().into_inner()
    }

    #[test]
    fn test_extract_locale_from_zip_no_prefix() {
        // 无根目录前缀（如手动构建的 zip）
        let zip_buf = make_test_zip("");
        let locale = super::extract_locale_from_zip(&zip_buf).unwrap();
        assert_eq!(locale.version, "1.0.0");
        assert_eq!(locale.contents.len(), 2);
        assert!(locale.contents.contains_key("en"));
        assert!(locale.contents.contains_key("zh-CN"));
        let en_base = locale.contents["en"].contents["base.cfg"].as_str();
        assert!(en_base.contains("iron-plate=Iron plate"));
    }

    #[test]
    fn test_extract_locale_from_zip_with_prefix() {
        // 有根目录前缀（模拟真实 Factorio mod zip，如 "test-mod_1.0.0/"）
        let zip_buf = make_test_zip("test-mod_1.0.0/");
        let locale = super::extract_locale_from_zip(&zip_buf).unwrap();
        assert_eq!(locale.version, "1.0.0");
        assert_eq!(locale.contents.len(), 2);
        assert!(locale.contents.contains_key("en"));
        assert!(locale.contents.contains_key("zh-CN"));
        let en_base = locale.contents["en"].contents["base.cfg"].as_str();
        assert!(en_base.contains("iron-plate=Iron plate"));
    }

    #[test]
    fn test_parse_mod_dependencies() {
        let info = serde_json::json!({
            "dependencies": [
                "base >= 2.1",
                "core",
                "! incompatible-mod",
                "(?) flib >= 0.14.0",
                "? space-age >= 2.0",
                "~ custom-lib=1.0",
                "+ helper-mod",
                "flib >= 0.12.0"
            ]
        });
        let deps = super::parse_mod_dependencies(&info);
        assert_eq!(deps, vec!["flib", "space-age", "custom-lib", "helper-mod"]);
    }

    #[test]
    fn test_resolve_mod_dependency_chain_bfs() {
        let tmp = std::env::temp_dir().join(format!("tanvec_dep_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        std::fs::write(
            tmp.join("dep-a.json"),
            r#"{"contents":{},"version":"1.0","dependencies":["dep-b","other-mod"]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("dep-b.json"),
            r#"{"contents":{},"version":"1.0","dependencies":["dep-c"]}"#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("dep-c.json"),
            r#"{"contents":{},"version":"1.0","dependencies":["dep-d-too-deep"]}"#,
        )
        .unwrap();

        let chain = super::resolve_mod_dependency_chain("my-mod", &["dep-a".to_string()], &tmp);
        assert_eq!(
            chain,
            vec!["dep-a", "dep-b", "other-mod", "dep-c", "my-mod"]
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_build_mod_glossary_dependency_and_case_insensitive() {
        let mut base = ini::Ini::new();
        base.with_general_section().set("Iron plate", "铁板");
        base.with_general_section().set("Copper plate", "铜板");

        let mut ai = ini::Ini::new();
        ai.with_section(Some("ai-glossary"))
            .set("Angel", "天使虫")
            .set("Stellar Forge", "恒星锻炉");
        ai.with_section(Some("unrelated-mod"))
            .set("Hauler", "货运飞艇");
        ai.with_section(Some("dep-mod"))
            .set("Hauler", "重型运输车")
            .set("stellar forge", "星辰锻炉");
        ai.with_section(Some("my-mod"))
            .set("hauler", "矿石拖运车");

        let mut diff = ini::Ini::new();
        diff.with_section(Some("item-name"))
            .set("x", "Use iron plate in Stellar Forge with hauler");

        let mut file_diffs = std::collections::BTreeMap::new();
        file_diffs.insert("locale.cfg".to_string(), diff);

        let dep_chain = vec!["dep-mod".to_string(), "my-mod".to_string()];
        let result = super::build_mod_glossary(&base, &ai, &dep_chain, &file_diffs);
        let general = result.section(None::<String>).unwrap();

        // 命中了 iron plate（大小写不敏感），未命中 Copper plate，未命中全局 Angel
        assert_eq!(general.get("Iron plate"), Some("铁板"));
        assert_eq!(general.get("Copper plate"), None);
        assert_eq!(general.get("Angel"), None);
        // dep-mod 覆盖了 ai-glossary 的 Stellar Forge，my-mod 覆盖了 dep-mod 的 Hauler（大小写去重）
        assert_eq!(general.get("stellar forge"), Some("星辰锻炉"));
        assert_eq!(general.get("Stellar Forge"), None);
        assert_eq!(general.get("hauler"), Some("矿石拖运车"));
        assert_eq!(general.get("Hauler"), None);
    }

    #[test]
    fn test_split_file_diffs_into_batches() {
        let mut ini1 = ini::Ini::new();
        for i in 0..150 {
            ini1.with_section(Some("item-name"))
                .set(format!("k{}", i), format!("val {}", i));
        }
        let mut ini2 = ini::Ini::new();
        for i in 0..30 {
            ini2.with_section(Some("recipe-name"))
                .set(format!("r{}", i), format!("rec {}", i));
        }
        let mut file_diffs = std::collections::BTreeMap::new();
        file_diffs.insert("a.cfg".to_string(), ini1);
        file_diffs.insert("b.cfg".to_string(), ini2);

        let batches = super::split_file_diffs_into_batches(&file_diffs, 120);
        assert_eq!(batches.len(), 2);
        let count0: usize = batches[0]
            .values()
            .flat_map(|i| i.iter().flat_map(|(_, p)| p.iter()))
            .count();
        let count1: usize = batches[1]
            .values()
            .flat_map(|i| i.iter().flat_map(|(_, p)| p.iter()))
            .count();
        assert_eq!(count0, 120);
        assert_eq!(count1, 60);
    }
}
