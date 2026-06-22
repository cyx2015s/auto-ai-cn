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
pub fn extract_locale_from_zip(zip_bytes: &[u8]) -> anyhow::Result<LocaleInfo> {
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).context("无法打开 zip 文件")?;

    let mut locale_info = LocaleInfo {
        contents: indexmap::IndexMap::new(),
        version: String::new(),
    };

    // 收集所有文件名，检测公共根目录前缀
    let mut all_names = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            all_names.push(file.name().to_string());
        }
    }
    let strip_prefix = find_common_root_prefix(&all_names);

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;

        let raw_name = file.name().to_string();
        // 跳过目录和超大文件
        if raw_name.ends_with('/') || file.size() > 5 * 1024 * 1024 {
            continue;
        }

        // 剥离公共根目录前缀（如 "mod-name_version/"）
        let name = match &strip_prefix {
            Some(prefix) => raw_name.strip_prefix(prefix).unwrap_or(&raw_name),
            None => &raw_name,
        };

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

        // 同时检测 info.json（可能在根目录前缀下）
        if name == "info.json" {
            let mut info_content = String::new();
            if file.read_to_string(&mut info_content).is_ok()
                && let Ok(info) = serde_json::from_str::<serde_json::Value>(&info_content)
            {
                locale_info.version = info
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
        }
    }

    Ok(locale_info)
}

/// 从文件路径列表中检测公共根目录前缀。
///
/// 例：`["foo/bar.txt", "foo/baz/info.json"]` → `Some("foo/")`
/// 仅考虑目录级前缀（以 `/` 结尾），要求所有非目录条目共享该前缀。
fn find_common_root_prefix(names: &[String]) -> Option<String> {
    let files: Vec<&str> = names
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.ends_with('/'))
        .collect();
    if files.is_empty() {
        return None;
    }

    let first = files[0];
    let first_slash = first.find('/')?;

    // 候选前缀 = 第一个 '/' 之前的部分 + '/'
    let candidate = &first[..=first_slash];

    if files.iter().all(|f| f.starts_with(candidate)) {
        Some(candidate.to_string())
    } else {
        None
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Step 2: 加载外部参考文件
// ══════════════════════════════════════════════════════════════════════════════

/// 提取 Factorio 官方 mod 的中英文对照表。
///
/// 遍历 `core`, `base`, `quality`, `elevated-rails`, `space-age` 五个官方 mod 的
/// `locale/en/*.cfg` 和对应 `zh-CN/*.cfg`，生成 key → 中文翻译 的映射。
/// 后面的 mod（DLC）的翻译会覆盖前面的。
pub fn extract_base_glossary(game_data_path: &Path) -> anyhow::Result<ini::Ini> {
    const OFFICIAL_MODS: &[&str] = &["core", "base", "quality", "elevated-rails", "space-age"];

    let mut glossary = ini::Ini::new();

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
                    if let Some(zh_value) = zh_ini.section(section).and_then(|s| s.get(key)) {
                        if !zh_value.is_empty() && zh_value != en_value {
                            glossary.with_section(section).set(key, zh_value);
                        }
                    }
                }
            }
        }
    }

    Ok(glossary)
}

#[test]
fn test_extract_base_glossary() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let game_data_path = std::env::var("FACTORIO_DATA_PATH")
        .map(PathBuf::from)
        .expect("请设置 FACTORIO_DATA_PATH 环境变量指向游戏数据目录");
    dbg!(&game_data_path);
    let glossary = extract_base_glossary(&game_data_path).expect("提取对照表失败");
    dbg!(&glossary);
    println!("{}", translation::ini_to_str(&glossary)?);
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

### 星球描述

有且仅在space-location-description下或technology-description下出现星球介绍时，开头需要以「星球名（英文原名）」开头。

#### 原版星球译名

- Nauvis: 新地星
- Gleba: 句芒星
- Vulcanus: 祝融星
- Fulgora: 雷神星
- Aquilo: 玄冥星

科技名称为发现某某星球时，翻译格式为：「发现星球：XXX」，如果天体不是行星，可以视情况变为「发现卫星」，「发现星系」等。

### 插件

用于增幅建筑物效果的module-name，需要将module翻译成「插件」，以下左侧的翻译需要更改为右侧的。efficiency module:「效率模块」→「效率插件」，productivity module:「生产模块」→「产能插件」，speed module:「速度模块」→「速度插件」，quality module:「质量模块」→「品质插件」。

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

如果[mod-name]下出现title，更改为模组内部名。（模组作者太疏忽了）
双下划线包裹的内容不要翻译，如__ITEM__iron-plate__。
特殊情况时__plural_for_parameter_1__{1=item|rest=items}__，可以视情况移除变量，只用翻译为原型，因为中文没有单复数。
方括号内内容也不要翻译，如[item=iron-plate]，[img=item.lab]之类都是富文本，保留原样。
虚拟物品结尾出现unit时，可以根据情况翻译为「单位」或「器」。如处理器，能源控制器，火箭控制器。
ghost表示没有完成建筑的建筑时，翻译为「虚影」。
模组中出现torio结尾的单词时，可以根据情况翻译为「XX工厂」，和异星工厂 factorio 匹配。

## 总结

翻译规则：
1. 保持游戏术语的一致性，参考提供的原版对照表
2. 对于技术类文本，使用准确、简洁的中文表达
3. 保留原文中的格式标记（如 __1__、[item=xxx] 等占位符）
4. 不要翻译专有名词、代码标识符
5. 使用简体中文书写风格
## 交互

- 使用 submit_translation 函数，传入 file_name + ini_content 一次性提交整个文件的翻译
- ini_content 应为标准 INI 格式文本，保留原文的 section 结构和 key，只将 value 翻译为中文
- 如果文件过大，可以按 section 分批提交（传入 section + entries）
- 遇到虚构的名称，请额外使用 submit_glossary 函数提交其翻译，格式为 term（英文）+ translation（中文），可选 reason 字段说明判断依据"#;

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
                "description": "完整的 INI 格式翻译文本。保留 section 结构和 key，只将 value 翻译为中文。推荐使用此方式一次性提交整个文件。"
            },
            "section": {
                "type": "string",
                "description": "INI section 名称，例如 'entity-name'。仅在按 section 分批提交时使用，与 entries 配合。"
            },
            "entries": {
                "type": "array",
                "description": "该 section 下的翻译条目。仅在按 section 分批提交时使用，与 section 配合。",
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
                "description": "英文专有名词/术语"
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
                description: "提交跨模组专有名词的翻译，用于保持虚构物品/实体/概念等翻译的一致性。".to_string(),
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
/// 1. 原版游戏中英文对照表（作为参考）
/// 2. 需要翻译的内容（按文件分开的 INI 差异）
/// 3. 如果提供了上次的翻译文件，附上参考
pub fn build_user_prompt(
    base_locale: &ini::Ini,
    file_diffs: &BTreeMap<String, ini::Ini>,
    previous_translations: Option<&LocaleInfo>,
) -> anyhow::Result<String> {
    let mut prompt = String::new();

    // 原版对照表
    if !base_locale.is_empty() {
        prompt.push_str("## 原版游戏术语对照参考\n\n");
        prompt.push_str("以下为原版游戏中的常见术语翻译，请保持翻译一致性：\n\n");
        prompt.push_str("```ini\n");
        prompt.push_str(&translation::ini_to_str(base_locale)?);
        prompt.push_str("```\n\n");
    }

    // 上次的翻译文件（作为风格参考）
    if let Some(prev) = previous_translations
        && !prev.contents.is_empty()
    {
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
    pub original: String,
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

/// 调用 LLM 获取翻译。
///
/// 使用 function calling 机制，LLM 通过多次调用 `submit_translation`
/// 分批提交翻译结果，也可通过 `submit_glossary` 提交专有名词翻译。
///
/// `glossary` 会被原地更新（AI 提交的术语追加到对照表）。
pub async fn call_llm_for_translation(
    client: &deepseek_api::DeepSeekClient,
    system_prompt: &str,
    user_prompt: &str,
    glossary: &mut ini::Ini,
) -> anyhow::Result<ini::Ini> {
    let tools = make_translation_tools();
    let mut result_ini = ini::Ini::new();
    let mut loop_count = 0;
    const MAX_LOOPS: usize = 256;

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

        // 无论 finish_reason 是什么，只要 assistant 消息带 tool_calls 就处理
        let msg = resp.choices[0].message.as_ref();

        if let Some(msg) = msg
            && msg.tool_calls.is_some()
        {
            messages.push(MessageRequest::Assistant(msg.clone()));

            if let Some(ref tool_calls) = msg.tool_calls {
                let mut has_valid_call = false;

                for tool_call in tool_calls {
                    let fn_name = tool_call.function.name.as_str();
                    match fn_name {
                        "submit_translation" => {
                            // 解析翻译数据
                            match serde_json::from_str::<SubmittedTranslation>(
                                &tool_call.function.arguments,
                            ) {
                                Ok(submitted) => {
                                    let mut merged_count = 0usize;

                                    // 模式 1：整文件 INI 文本提交
                                    if let Some(ref ini_content) = submitted.ini_content {
                                        let ini = translation::str_to_ini(ini_content)?;
                                        for (sec, props) in ini.iter() {
                                            let sec_name = sec.unwrap_or("");
                                            for (k, v) in props.iter() {
                                                result_ini.with_section(Some(sec_name)).set(k, v);
                                                merged_count += 1;
                                            }
                                        }
                                        debug!(
                                            "收到整文件翻译: file={}, sections={}, entries={}",
                                            submitted.file_name,
                                            ini.iter().count(),
                                            merged_count
                                        );
                                        has_valid_call = true;
                                        messages.push(MessageRequest::Tool(
                                            ToolMessageRequest::new(
                                                &format!(
                                                    "已收到 {} 的整文件翻译（{} 条）",
                                                    submitted.file_name, merged_count
                                                ),
                                                &tool_call.id,
                                            ),
                                        ));
                                    }
                                    // 模式 2：按 section 提交
                                    else if let (Some(section), Some(entries)) =
                                        (&submitted.section, &submitted.entries)
                                    {
                                        for entry in entries {
                                            result_ini
                                                .with_section(Some(section.as_str()))
                                                .set(&entry.key, &entry.translation);
                                            merged_count += 1;
                                        }
                                        debug!(
                                            "收到翻译: file={}, section={}, entries={}",
                                            submitted.file_name, section, merged_count
                                        );
                                        has_valid_call = true;
                                        messages.push(MessageRequest::Tool(
                                            ToolMessageRequest::new(
                                                &format!(
                                                    "已收到 {} 的 {} 下 {} 条翻译",
                                                    submitted.file_name, section, merged_count
                                                ),
                                                &tool_call.id,
                                            ),
                                        ));
                                    } else {
                                        // 格式无效：没有 ini_content 也没有 section+entries
                                        warn!(
                                            "翻译提交格式无效: file={}, 缺少 ini_content 或 (section+entries)",
                                            submitted.file_name
                                        );
                                        messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                        &format!(
                                            "格式无效：请提供 ini_content（整文件）或 section+entries（按 section）"
                                        ),
                                        &tool_call.id,
                                    )));
                                    }
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
                        "submit_glossary" => {
                            match serde_json::from_str::<SubmittedGlossaryEntry>(
                                &tool_call.function.arguments,
                            ) {
                                Ok(entry) => {
                                    glossary
                                        .with_section(Some("ai-glossary"))
                                        .set(&entry.term, &entry.translation);
                                    debug!(
                                        "AI 提交术语: {} → {} ({})",
                                        entry.term,
                                        entry.translation,
                                        entry.reason.as_deref().unwrap_or("无理由")
                                    );
                                    has_valid_call = true;
                                    messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                        &format!(
                                            "术语已记录: {} → {}",
                                            entry.term, entry.translation
                                        ),
                                        &tool_call.id,
                                    )));
                                }
                                Err(e) => {
                                    warn!("解析术语数据失败: {}", e);
                                    messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                        &format!("解析术语失败: {}", e),
                                        &tool_call.id,
                                    )));
                                }
                            }
                        }
                        _ => {
                            warn!("LLM 调用了未知函数: {}", fn_name);
                            messages.push(MessageRequest::Tool(ToolMessageRequest::new(
                                &format!("未知函数: {}", fn_name),
                                &tool_call.id,
                            )));
                        }
                    }
                }

                if !has_valid_call {
                    // 所有 tool_call 解析失败，让 LLM 重试
                    continue;
                }

                // 如果 finish_reason 是 Stop，不再追问
                if resp.choices[0].finish_reason == FinishReason::Stop {
                    break;
                }

                // 询问 LLM 是否还有更多翻译
                messages.push(MessageRequest::user(
                        "请继续提交剩余的翻译条目，或调用 stop 完成。如果所有翻译已完成，请只回复'所有翻译已完成'。",
                    ));
                continue;
            }
        }

        // 没有 tool_calls：如果已有 assistant 文本回复，结束；否则是纯文本响应
        if let Some(msg) = msg
            && !msg.content.is_empty()
        {
            messages.push(MessageRequest::Assistant(msg.clone()));
            debug!("LLM 最终回复: {}", msg.content);
        }
    }

    if loop_count >= MAX_LOOPS {
        warn!("LLM 交互达到最大循环次数 ({})，返回已收集的翻译", MAX_LOOPS);
    }

    Ok(result_ini)
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
async fn process_mod(
    client_fa: &FactorioWebClient,
    client_deepseek: &deepseek_api::DeepSeekClient,
    config: &FlowConfig,
    base_locale: &ini::Ini,
    system_prompt: &str,
    glossary: &mut ini::Ini,
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

    // 3.5 检查源语言和目标语言
    let has_source = current_locale.contents.contains_key(SOURCE_LANG);
    let has_target = current_locale.contents.contains_key(TARGET_LANG);

    if !has_source {
        // 没有 en 文件
        if has_target {
            info!("  ↳ 中文优先 mod（无 en 但有 zh-CN），跳过");
        } else {
            info!("  ↳ mod 没有源语言（{}）翻译文件，跳过", SOURCE_LANG);
        }
        return Ok(());
    }

    // 获取源语言文件
    let source_lang_info = &current_locale.contents[SOURCE_LANG];

    // 获取旧的 zh-CN 缓存（用于差异计算和合并）
    // 旧的 zh-CN 翻译（用于上下文参考和合并）
    let old_target_ini_by_file: BTreeMap<String, ini::Ini> = cached_locale
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

    // 4. 构建翻译任务：比较新旧 en 原文，只取新增或变更的条目
    let mut file_diffs: BTreeMap<String, ini::Ini> = BTreeMap::new();
    let mut total_entries = 0usize;

    for (file_name, content) in &source_lang_info.contents {
        let current_ini = translation::str_to_ini(content)?;

        let diff = if let Some(old_source_ini) = old_source_ini_by_file.get(file_name) {
            translation::diff_ini(old_source_ini, &current_ini)
        } else {
            current_ini.clone()
        };

        if diff.is_empty() {
            continue;
        }
        total_entries += diff.iter().flat_map(|(_, props)| props.iter()).count();
        file_diffs.insert(file_name.clone(), diff);
    }

    if file_diffs.is_empty() {
        info!("  ↳ 没有需要翻译的新内容，跳过");
        return Ok(());
    }
    info!(
        "  ↳ 需要翻译 {} 个文件 / {} 个条目 ({} → {})",
        file_diffs.len(),
        total_entries,
        SOURCE_LANG,
        TARGET_LANG
    );

    // 5. 构建增强的上下文参考：如果 mod 自带 zh-CN 翻译，合并到缓存中（自带优先）
    let context_locale: Option<LocaleInfo> =
        if let Some(builtin_target) = current_locale.contents.get(TARGET_LANG) {
            // mod 自带了 zh-CN 翻译 → 合并：自带优先，本地缓存兜底
            let mut merged = cached_locale.clone().unwrap_or_else(|| LocaleInfo {
                contents: indexmap::IndexMap::new(),
                version: String::new(),
            });
            let mut merged_target = LangInfo {
                contents: indexmap::IndexMap::new(),
            };

            // 以自带 zh-CN 文件为基准，用 en 原文作为 key 参考进行合并
            for (file_name, builtin_content) in &builtin_target.contents {
                let builtin_ini = translation::str_to_ini(builtin_content)?;
                let en_ini = source_lang_info
                    .contents
                    .get(file_name)
                    .map(|s| translation::str_to_ini(s))
                    .transpose()?;
                let reference_ini = en_ini.as_ref().unwrap_or(&builtin_ini);
                let old_ini = old_target_ini_by_file.get(file_name);

                let merged_ini = translation::merge_ini(
                    reference_ini,
                    old_ini.unwrap_or(&ini::Ini::new()),
                    &builtin_ini,
                );
                merged_target
                    .contents
                    .insert(file_name.clone(), translation::ini_to_str(&merged_ini)?);
            }

            merged
                .contents
                .insert(TARGET_LANG.to_string(), merged_target);
            Some(merged)
        } else {
            cached_locale.clone()
        };

    let user_prompt = build_user_prompt(base_locale, &file_diffs, context_locale.as_ref())?;

    info!("  ↳ 调用 LLM 进行翻译...");
    let llm_translation =
        call_llm_for_translation(client_deepseek, system_prompt, &user_prompt, glossary).await?;

    if llm_translation.is_empty() {
        warn!("  ↳ LLM 未返回任何翻译结果");
        return Ok(());
    }
    info!(
        "  ↳ LLM 返回了 {} 个 section 的翻译",
        llm_translation.iter().count()
    );

    // 6. 合并翻译：以 en 原文为基准，LLM 翻译优先，旧 zh-CN 缓存兜底
    //    只更新 zh-CN，保留缓存中其他语言的翻译不变
    let mut merged_target_lang = LangInfo {
        contents: indexmap::IndexMap::new(),
    };

    for (file_name, content) in &source_lang_info.contents {
        let reference_ini = translation::str_to_ini(content)?;
        let old_ini = old_target_ini_by_file.get(file_name);

        let merged_ini = translation::merge_ini(
            &reference_ini,
            old_ini.unwrap_or(&ini::Ini::new()),
            &llm_translation,
        );
        let merged_str = translation::ini_to_str(&merged_ini)?;
        merged_target_lang
            .contents
            .insert(file_name.clone(), merged_str);
    }

    // 构建最终 LocaleInfo：保留其他语言不变，更新 zh-CN 和 en 原文
    let mut merged_locale = cached_locale.unwrap_or_else(|| LocaleInfo {
        contents: indexmap::IndexMap::new(),
        version: String::new(),
    });
    merged_locale.version = current_locale.version.clone();
    // 保存 en 原文（供下次 diff 计算使用）
    merged_locale
        .contents
        .insert(SOURCE_LANG.to_string(), source_lang_info.clone());
    // 保存 zh-CN 翻译
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
    limit: Option<u64>,
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
    let base_locale = load_base_locale(&config)?;
    let system_prompt = load_system_prompt(&config.system_prompt_path)?;

    // 加载/创建 AI 术语表（跨模组专有名词翻译，AI 可提交新条目）
    let glossary_path = config.cache_dir.join("_ai_glossary.json");
    let mut ai_glossary: ini::Ini = if glossary_path.exists() {
        std::fs::read_to_string(&glossary_path)
            .ok()
            .and_then(|s| translation::str_to_ini(&s).ok())
            .unwrap_or_default()
    } else {
        ini::Ini::new()
    };

    info!("外部参考文件加载完成");

    // 获取更新的 mod 列表
    let updated_mods = fa_client
        .get_mods_updated_since(effective_since, &config.game_version, Some(100), limit)
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

        // 每次处理前重新合并基础对照表 + AI 术语表（AI 可能在之前的 mod 中新增了术语）
        let mut combined_glossary = base_locale.clone();
        for (sec, props) in ai_glossary.iter() {
            for (k, v) in props.iter() {
                combined_glossary.with_section(sec).set(k, v);
            }
        }

        match process_mod(
            &fa_client,
            &deepseek_client,
            &config,
            &combined_glossary,
            &system_prompt,
            &mut ai_glossary,
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

    // 保存 AI 术语表
    {
        let glossary_str = translation::ini_to_str(&ai_glossary)?;
        std::fs::write(&glossary_path, &glossary_str)
            .with_context(|| format!("无法保存 AI 术语表: {:?}", glossary_path))?;
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
    fn test_find_common_root_prefix() {
        let names = vec![
            "mod_1.0.0/locale/en/base.cfg".to_string(),
            "mod_1.0.0/locale/zh-CN/base.cfg".to_string(),
            "mod_1.0.0/info.json".to_string(),
        ];
        assert_eq!(
            super::find_common_root_prefix(&names).as_deref(),
            Some("mod_1.0.0/")
        );

        // 无公共前缀
        let names2 = vec![
            "locale/en/base.cfg".to_string(),
            "other/info.json".to_string(),
        ];
        assert_eq!(super::find_common_root_prefix(&names2), None);

        // 单文件
        let names3 = vec!["foo/bar.txt".to_string()];
        assert_eq!(
            super::find_common_root_prefix(&names3).as_deref(),
            Some("foo/")
        );
    }
}
