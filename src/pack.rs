//! 翻译包打包
//!
//! 将 cache 目录中的所有翻译缓存合并打包为 1 个 Factorio mod zip。
//!
//! 输出结构：
//! ```text
//! <pack_name>_<date>/
//!   locale/zh-CN/<mod_name>.cfg                   （无原版冲突的翻译，默认启用）
//!   locale/zh-CN/<mod_name>.top.cfg               （仅当同文件内存在顶层键与 section 同名冲突时拆出）
//!   locale/zh-CN/<mod_name>.override.cfg.disabled （覆盖原版词条的翻译，默认禁用）
//!   locale/zh-CN/<mod_name>.top.override.cfg.disabled
//!   locale/zh-CN/toggle_py
//!   control.lua
//!   info.json
//! ```
//!
//! 为避免未安装的 mod 覆盖原版游戏词条，protect 模式下按词条级拆分为默认启用的安全文件与默认禁用的 `.override.cfg.disabled`。

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::Utc;
use log::{debug, info, warn};
use serde_json;

use crate::translation;

fn merge_lang_files(lang_info: &translation::LangInfo, mod_name: &str) -> ini::Ini {
    let mut merged = ini::Ini::new();
    for (_file_name, file_content) in &lang_info.contents {
        let ini = match translation::str_to_ini(file_content) {
            Ok(ini) => ini,
            Err(e) => {
                warn!("无法解析 {} 的翻译文件: {}", mod_name, e);
                continue;
            }
        };
        for (section, props) in ini.iter() {
            for (k, v) in props.iter() {
                merged.with_section(section).set(k, v);
            }
        }
    }
    merged
}

fn ini_has_entries(ini: &ini::Ini) -> bool {
    ini.iter().any(|(_, props)| !props.is_empty())
}

fn write_ini_entry(
    zip_writer: &mut zip::ZipWriter<std::fs::File>,
    options: zip::write::SimpleFileOptions,
    zip_path: &str,
    ini: &ini::Ini,
) -> anyhow::Result<bool> {
    if !ini_has_entries(ini) {
        return Ok(false);
    }
    let ini_str = translation::ini_to_str(ini)?;
    zip_writer.start_file(zip_path, options)?;
    zip_writer.write_all(ini_str.as_bytes())?;
    Ok(true)
}

/// 若同一个 `Ini` 中同时存在顶层键（`general_section`）和与之同名的 `[section]`，
/// Factorio 单文件解析器会拒绝或错乱（见 https://forums.factorio.com/viewtopic.php?p=703719#p703719）。
/// 本函数将顶层键拆入独立的 `(top_ini, sectioned_ini)`，避免丢弃任何顶层翻译。
pub fn split_top_level_if_colliding(mut ini: ini::Ini) -> (Option<ini::Ini>, Option<ini::Ini>) {
    let has_collision = if let Some(gen_props) = ini.section(None::<String>)
        && !gen_props.is_empty()
    {
        ini.sections()
            .flatten()
            .any(|sec_name| gen_props.get(sec_name).is_some())
    } else {
        false
    };

    if !has_collision {
        let sec_opt = if ini_has_entries(&ini) {
            Some(ini)
        } else {
            None
        };
        return (None, sec_opt);
    }

    let mut top_ini = ini::Ini::new();
    if let Some(gen_props) = ini.delete(None::<String>) {
        for (k, v) in gen_props.iter() {
            top_ini.with_general_section().set(k, v);
        }
    }

    let top_opt = if ini_has_entries(&top_ini) {
        Some(top_ini)
    } else {
        None
    };
    let sec_opt = if ini_has_entries(&ini) {
        Some(ini)
    } else {
        None
    };
    (top_opt, sec_opt)
}

fn write_mod_ini_split(
    zip_writer: &mut zip::ZipWriter<std::fs::File>,
    options: zip::write::SimpleFileOptions,
    pack_name: &str,
    mod_name: &str,
    ini: ini::Ini,
    is_override: bool,
) -> anyhow::Result<bool> {
    let suffix = if is_override {
        "override.cfg.disabled"
    } else {
        "cfg"
    };
    let (top_opt, sec_opt) = split_top_level_if_colliding(ini);
    let mut wrote = false;
    if let Some(sec_ini) = sec_opt {
        let path = format!("{}/locale/zh-CN/{}.{}", pack_name, mod_name, suffix);
        wrote |= write_ini_entry(zip_writer, options, &path, &sec_ini)?;
    }
    if let Some(top_ini) = top_opt {
        let path = format!("{}/locale/zh-CN/{}.top.{}", pack_name, mod_name, suffix);
        wrote |= write_ini_entry(zip_writer, options, &path, &top_ini)?;
    }
    Ok(wrote)
}

/// 将 cache 目录中所有翻译缓存打包为 1 个 Factorio mod zip。
///
/// 如果 `protect` 为 true 且提供了 `base_keys`：
/// - 与原版译文完全一致，或英文原文与原版英文一致的冗余词条会被省略（避免 AI 标点差异导致误覆盖）；
/// - 真正修改了原版含义的词条（含 `item-name`/`item-description` 对原版 `entity`/`equipment` 的跨节回退劫持）
///   会拆出至 `<mod_name>.override.cfg.disabled`（默认禁用），其余独有词条保留在 `<mod_name>.cfg`（默认启用）。
pub fn pack_all_to_one_mod(
    cache_dir: &Path,
    output_dir: &Path,
    pack_name: &str,
    protect: bool,
    base_keys: Option<&HashMap<String, String>>,
    base_en_keys: Option<&HashMap<String, String>>,
) -> anyhow::Result<PathBuf> {
    // 扫描缓存目录
    let mut cache_files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(cache_dir)
        .with_context(|| format!("无法读取缓存目录: {}", cache_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !path.is_file() || !name.ends_with(".json") || name.starts_with('_') {
            continue;
        }
        cache_files.push(path);
    }

    if cache_files.is_empty() {
        anyhow::bail!("缓存目录中没有翻译缓存文件");
    }

    std::fs::create_dir_all(output_dir)?;

    let today = Utc::now().format("%Y.%m.%d").to_string();
    let zip_name = format!("{}_{}", pack_name, today);
    let output_path = output_dir.join(format!("{}.zip", zip_name));

    let file = std::fs::File::create(&output_path)?;
    let mut zip_writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let pb = crate::progress::new_bar();
    pb.set_length(cache_files.len() as u64);

    let mut packed_count = 0usize;
    let mut override_mod_count = 0usize;

    for cache_path in &cache_files {
        let mod_name = cache_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        let content = match std::fs::read_to_string(cache_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("无法读取缓存 {}: {}", mod_name, e);
                continue;
            }
        };

        let locale_info: translation::LocaleInfo = match serde_json::from_str(&content) {
            Ok(info) => info,
            Err(e) => {
                warn!("无法解析缓存 {}: {}", mod_name, e);
                continue;
            }
        };

        let Some(target_lang) = locale_info.contents.get("zh-CN") else {
            continue;
        };

        let mut merged_ini = merge_lang_files(target_lang, mod_name);
        if merged_ini.is_empty() || !ini_has_entries(&merged_ini) {
            continue;
        }

        // 防御性修正：避免打包后的 locale-identifier=en 覆盖 flib 等模组的语言识别
        if merged_ini
            .section(None::<String>)
            .and_then(|s| s.get("locale-identifier"))
            == Some("en")
        {
            merged_ini
                .with_general_section()
                .set("locale-identifier", "zh-CN");
        }

        let mod_en_ini = locale_info
            .contents
            .get("en")
            .map(|en_lang| merge_lang_files(en_lang, mod_name));

        let (safe_ini, override_ini) = if protect && let Some(base_keys) = base_keys {
            split_ini_by_base_overlap(
                &merged_ini,
                mod_en_ini.as_ref(),
                base_keys,
                base_en_keys,
            )
        } else {
            (merged_ini, ini::Ini::new())
        };

        let wrote_safe =
            write_mod_ini_split(&mut zip_writer, options, pack_name, mod_name, safe_ini, false)?;
        let wrote_override = write_mod_ini_split(
            &mut zip_writer,
            options,
            pack_name,
            mod_name,
            override_ini,
            true,
        )?;

        if wrote_override {
            override_mod_count += 1;
            debug!("  ↳ {} 的原版覆盖词条已拆出至 .override.cfg.disabled", mod_name);
        }

        if wrote_safe || wrote_override {
            packed_count += 1;
        }
        pb.set_message(format!("打包: {}", mod_name));
        pb.inc(1);
    }

    // toggle.py， 官网不允许传带可执行文件的 mod 包，作此变通
    let toggle_path = format!("{}/locale/zh-CN/toggle_py", pack_name);
    const TOGGLE_PY: &str = include_str!("templates/toggle.py");
    zip_writer.start_file(&toggle_path, options)?;
    zip_writer.write_all(TOGGLE_PY.as_bytes())?;

    // control.lua 启动提示
    let control_path = format!("{}/control.lua", pack_name);
    const CONTROL_LUA: &str = include_str!("templates/control.lua");
    zip_writer.start_file(&control_path, options)?;
    zip_writer.write_all(CONTROL_LUA.as_bytes())?;

    // info.json
    let info_json = serde_json::json!({
        "name": pack_name,
        "version": today,
        "title": "切向量的 AI 汉化",
        "factorio_version": "2.1",
        "dependencies": ["base >= 2.1"],
        "author": "tanvec",
        "description": format!("包含 {} 个模组的 AI 中文翻译。与原版词条冲突的翻译默认拆分为 .override.cfg.disabled 禁用，其余词条开箱即用。手动解压后可运行 locale/zh-CN/toggle_py（或使用 gavin-server）按 mod-list.json 精准启用对应模组及覆盖词条。详细信息请参见 github 链接 https://github.com/cyx2015s/auto-ai-cn", packed_count)
    });
    let info_path = format!("{}/info.json", pack_name);
    zip_writer.start_file(&info_path, options)?;
    zip_writer.write_all(serde_json::to_string_pretty(&info_json)?.as_bytes())?;

    zip_writer.finish()?;

    info!(
        "翻译包已生成: {:?} ({} 个 mod，其中 {} 个拆出原版覆盖文件)",
        output_path, packed_count, override_mod_count
    );
    Ok(output_path)
}

const BASE_GAME_PROTECTED_SECTIONS: &[&str] = &[
    "gui-explore-mods-sort-attribute-name",
    "entity-status",
    "gui-select-upgrade-planner",
    "gui-advert-switch2",
    "gui-train",
    "gui-network-selector",
    "fluid-name",
    "difficulty",
    "description",
    "richness",
    "item-description",
    "gui-technologies-list",
    "gui-control-behavior",
    "collector-context",
    "gui-current-research",
    "gui-production",
    "technology-description",
    "gui-map-editor-clone-editor",
    "gui-edit-label",
    "gui-arithmetic",
    "gui-tips-and-tricks",
    "gui-achievements",
    "gui-train-wait-condition-description",
    "noise-property",
    "tips-and-tricks-item-name",
    "gui-interface-settings-description",
    "gui-space-platform",
    "gui-explore-mods-filter-description",
    "gui-migrated-content",
    "gui-undelete-space-platforms",
    "asteroid-chunk-name",
    "space-platforms",
    "gui-permissions-names",
    "gui-ending-screen",
    "achievement-name",
    "programmable-speaker-note",
    "item-name",
    "color-capital",
    "shortcut",
    "tips-and-tricks-item-description",
    "gui-side-menu",
    "gui-train-stop",
    "gui-player-management",
    "gui-sound-settings",
    "gui-map-editor-instructions",
    "gui-multiplayer-connect",
    "gui-technology-preview",
    "gui-remote-view",
    "gui-map-generator",
    "http-error",
    "gui-alert",
    "gui-rocket-silo",
    "gui-save-scenario",
    "equipment-name",
    "gui-technology",
    "gui-interrupts",
    "gui-load-game",
    "gui-downloading-mods",
    "gui-logistic",
    "save-map-failed",
    "recipe-description",
    "recipe-name",
    "mod-name",
    "surface-name",
    "gui-technology-queue",
    "gui-create-account",
    "cant-build-reason",
    "gui-update",
    "permissions-help",
    "gui-plant-entity",
    "controls",
    "gui-advert",
    "character-corpse",
    "gui-crafting-queue",
    "technology-trigger",
    "command-help",
    "gui-map-editor-title",
    "inventory-full-message",
    "noise-expression",
    "gui-orbital-request",
    "gui-graphics-settings",
    "factoriopedia-description",
    "gui-players",
    "gui-new-game",
    "gui-additional-entity-settings",
    "gui-map-editor-tile-editor",
    "gui-space-platforms",
    "entity-name",
    "gui-roboport",
    "error",
    "gui-space",
    "agricultural-tower-gui",
    "quality-name",
    "virtual-signal-name",
    "tutorial-gui",
    "tips-and-tricks-simulation",
    "gui-infinity-pipe",
    "gui-display-panel",
    "gui-assembling-machine",
    "gui-alert-tooltip",
    "gui-building-statistics",
    "airborne-pollutant-name",
    "gui-about",
    "gui-game-finished",
    "gui-user-login",
    "gui-explore-mods",
    "fuel-category-name",
    "gui-browse-games",
    "spidertron-status",
    "gui-mods",
    "tooltip-category",
    "gui-redo-confirmation",
    "gui-chartbundle-upload-screen",
    "tile-description",
    "gui-mining-drill",
    "gui-bonus",
    "chat-icon-select-list-gui",
    "gui-electric-energy-interface",
    "gui-train-state",
    "gui-control-behavior-modes",
    "story",
    "gui-text-tags",
    "controls-description",
    "map-gen-preset-name",
    "invalid-map-version",
    "gui-multiplayer-lobby",
    "gui-admin-player",
    "gui-graphics-settings-description",
    "gui-map-editor-time-editor",
    "gui-decider",
    "size",
    "gui-sync-mods-with-save",
    "gui-map-info",
    "gui-logistic-section",
    "space-location-name",
    "clone-area-errors",
    "gui-map-editor-force-editor",
    "gui-lab",
    "tile-name",
    "space-location-description",
    "gui-heat-interface",
    "item-limitation",
    "achievement-description",
    "gui-inserter",
    "gui-turret",
    "gui-map-editor-settings",
    "alerts-config-gui",
    "gui-constant",
    "gui-character",
    "technology-name",
    "map-gen-preset-description",
    "gui-undo-confirmation",
    "control-keys",
    "quality-description",
    "gui-lamp",
    "gui-redo-tooltip",
    "gui-resource-entity",
    "gui-asteroid-collector",
    "gui-map-view-settings",
    "gui-other-settings-description",
    "gui-manage-mods",
    "json-parse-error",
    "gui-edit-pin",
    "gui-speed-panel",
    "controller",
    "gui-fluidbox",
    "gui-blueprint-library",
    "gui-set-email",
    "gui-space-locations",
    "gui-map-generator-errors",
    "gui-map-editor-settings-categories",
    "gui-power-switch",
    "gui-load-scenario",
    "gui-map-editor-script-editor",
    "gui-undo-action",
    "gui-mod-info",
    "gui-the-rest-settings",
    "gui-tag-edit",
    "color",
    "modifier-description",
    "gui-map-editor-surface-editor",
    "gui-goal-description",
    "gui-interface-settings",
    "gui-splitter",
    "gui-control-settings",
    "fluid-description",
    "surface-property-unit",
    "gui-deconstruction",
    "ini-parse-error",
    "gui-starmap",
    "gui-other-settings",
    "gui-undo-tooltip",
    "factoriopedia",
    "config-help",
    "entity-type",
    "gui-permissions",
    "description-rail",
    "gui-redo-action",
    "virtual-signal-description",
    "multiplayer",
    "surface-property-name",
    "gui-linked-container",
    "gui-selector",
    "achievement-progress",
    "quality-tooltip",
    "command-output",
    "gui-save-game",
    "gui-map-editor-entity-editor",
    "programmable-speaker-instrument",
    "config-output",
    "permissions-command-output",
    "gui-update-mods",
    "gui-menu",
    "gui-map-editor",
    "gui-package-list",
    "mod-description",
    "autoplace-control-names",
    "gui-server-config",
    "gui-electric-network",
    "inventory-restriction",
    "gui-upgrade",
    "gui-blueprint-book",
    "gui-map-editor-menu",
    "airborne-pollutant-name-with-amount",
    "gui-blueprint",
    "gui-map-editor-map-settings-editor",
    "deconstruction-tile-mode",
    "gui-feedback",
    "item-group-name",
    "gui-map-editor-lua-snippet-editor",
    "gui-car",
    "gui-trains",
    "decorative-name",
    "gui-new-space-platform",
    "gui-blueprint-parametrisation",
    "gui-explore-mods-filter-name",
    "chartbundle-state",
    "gui-quick-panel",
    "gui-auth-server",
    "gui-mod-settings",
    "gui-kills",
    "gui-infinity-container",
    "gui",
    "gui-mod-load-error",
    "gui-control-behavior-modes-guis",
    "gui-hotkey-suggestions",
    "gui-programmable-speaker",
    "gui-mod-startup-settings-mismatch",
    "entity-description",
    "gui-undo-redo-wire-type",
    "gui-rename",
    "gui-requester",
    "ammo-category-name",
    "damage-type-name",
    "frequency",
    "lua-profiler",
    "gui-map-editor-force-data-editor",
    "gui-sound-settings-description",
    "gui-surface-list",
    "gui-map-editor-tool",
    "graphics-errors",
    "",
];

fn is_protected(s: &str) -> bool {
    BASE_GAME_PROTECTED_SECTIONS.contains(&s)
}

fn lookup_base_match<'a>(
    section: Option<&str>,
    key: &str,
    base_keys: &'a HashMap<String, String>,
) -> Option<(String, &'a String)> {
    let sec_prefix = section.map_or_else(String::new, |s| format!("{}.", s));
    let direct_key = format!("{}{}", sec_prefix, key);
    if let Some(val) = base_keys.get(&direct_key) {
        return Some((direct_key, val));
    }

    let fallback_secs: &[&str] = match section {
        Some("item-name") => &["entity-name", "equipment-name"],
        Some("item-description") => &["entity-description", "equipment-description"],
        _ => return None,
    };
    for fb in fallback_secs {
        let fb_key = format!("{}.{}", fb, key);
        if let Some(val) = base_keys.get(&fb_key) {
            return Some((fb_key, val));
        }
    }
    None
}

/// 将模组译文拆分为 `(safe_ini, override_ini)`：
/// - 若与原版译文相同，或模组英文原文与原版英文完全一致，直接省略；
/// - 若确实覆写了原版词条（含 `item-name` / `item-description` 对 `entity` / `equipment` 的跨节回退劫持），
///   放入 `override_ini`；
/// - 其余模组独有词条保留在 `safe_ini`。
pub fn split_ini_by_base_overlap(
    merged_ini: &ini::Ini,
    mod_en_ini: Option<&ini::Ini>,
    base_keys: &HashMap<String, String>,
    base_en_keys: Option<&HashMap<String, String>>,
) -> (ini::Ini, ini::Ini) {
    let mut safe_ini = ini::Ini::new();
    let mut override_ini = ini::Ini::new();

    for (section, props) in merged_ini.iter() {
        let protected = is_protected(section.unwrap_or(""));
        let sec_prefix = section.map_or_else(String::new, |s| format!("{}.", s));
        for (key, value) in props.iter() {
            if protected && let Some((matched_key, base_zh)) = lookup_base_match(section, key, base_keys) {
                if base_zh == value {
                    continue;
                }
                let direct_key = format!("{}{}", sec_prefix, key);
                let mod_en_val = mod_en_ini
                    .and_then(|en| en.section(section))
                    .and_then(|p| p.get(key));
                let base_en_val = base_en_keys.and_then(|m| {
                    m.get(&direct_key)
                        .or_else(|| m.get(&matched_key))
                        .map(String::as_str)
                });
                if mod_en_val.is_some() && mod_en_val == base_en_val {
                    continue;
                }
                debug!(
                    "覆盖原版 key: {}{} ({} != {}, 匹配原版 {})",
                    sec_prefix, key, value, base_zh, matched_key
                );
                override_ini.with_section(section).set(key, value);
                continue;
            }
            safe_ini.with_section(section).set(key, value);
        }
    }

    (safe_ini, override_ini)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_top_level_if_colliding() {
        let mut ini = ini::Ini::new();
        ini.with_general_section()
            .set("companion-menu", "僚机控制面板")
            .set("companion", "保镖僚机");
        ini.with_section(Some("companion-menu"))
            .set("title", "僚机控制面板");

        let (top_opt, sec_opt) = split_top_level_if_colliding(ini);
        let top = top_opt.expect("colliding top-level keys should be split out");
        let sec = sec_opt.expect("sectioned keys should remain");

        assert_eq!(
            top.section(None::<String>).unwrap().get("companion-menu"),
            Some("僚机控制面板")
        );
        assert_eq!(
            top.section(None::<String>).unwrap().get("companion"),
            Some("保镖僚机")
        );
        assert_eq!(top.sections().flatten().count(), 0);

        assert!(sec.section(None::<String>).is_none_or(|p| p.is_empty()));
        assert_eq!(
            sec.section(Some("companion-menu")).unwrap().get("title"),
            Some("僚机控制面板")
        );
    }

    #[test]
    fn test_split_top_level_no_collision() {
        let mut ini = ini::Ini::new();
        ini.with_general_section().set("top-only", "顶层键");
        ini.with_section(Some("other-sec")).set("k", "v");

        let (top_opt, sec_opt) = split_top_level_if_colliding(ini);
        assert!(top_opt.is_none());
        let sec = sec_opt.unwrap();
        assert_eq!(
            sec.section(None::<String>).unwrap().get("top-only"),
            Some("顶层键")
        );
        assert_eq!(sec.section(Some("other-sec")).unwrap().get("k"), Some("v"));
    }

    #[test]
    fn test_split_ini_by_base_overlap() {
        let mut base_zh = HashMap::new();
        base_zh.insert("entity-name.transport-belt".to_string(), "基础传送带".to_string());
        base_zh.insert(
            "entity-description.electromagnetic-plant".to_string(),
            "电磁研究与应用的核心设施。".to_string(),
        );
        base_zh.insert("entity-name.beacon".to_string(), "插件效果分享塔".to_string());

        let mut base_en = HashMap::new();
        base_en.insert("entity-name.transport-belt".to_string(), "Transport belt".to_string());
        base_en.insert(
            "entity-description.electromagnetic-plant".to_string(),
            "Central facility for electromagnetic research.".to_string(),
        );
        base_en.insert("entity-name.beacon".to_string(), "Beacon".to_string());

        let mut mod_zh = ini::Ini::new();
        mod_zh
            .with_section(Some("item-name"))
            // 跨节回退劫持，且英文已修改
            .set("transport-belt", "标准传送带")
            // 模组独有词条
            .set("custom-widget", "自定义零件");
        mod_zh
            .with_section(Some("entity-description"))
            // 复制原版英文，仅少个句号 -> 应被省略
            .set("electromagnetic-plant", "电磁研究与应用的核心设施");
        mod_zh
            .with_section(Some("entity-name"))
            // 实际修改原版含义 -> 进入 override
            .set("beacon", "基础插件塔");

        let mut mod_en = ini::Ini::new();
        mod_en
            .with_section(Some("item-name"))
            .set("transport-belt", "Standard transport belt")
            .set("custom-widget", "Custom widget");
        mod_en
            .with_section(Some("entity-description"))
            .set(
                "electromagnetic-plant",
                "Central facility for electromagnetic research.",
            );
        mod_en
            .with_section(Some("entity-name"))
            .set("beacon", "Basic beacon");

        let (safe, over) =
            split_ini_by_base_overlap(&mod_zh, Some(&mod_en), &base_zh, Some(&base_en));

        assert_eq!(
            safe.section(Some("item-name")).unwrap().get("custom-widget"),
            Some("自定义零件")
        );
        assert_eq!(
            safe.section(Some("item-name")).unwrap().get("transport-belt"),
            None
        );
        // verbatim vanilla copy is omitted from both
        assert!(safe.section(Some("entity-description")).is_none());
        assert!(over.section(Some("entity-description")).is_none());

        assert_eq!(
            over.section(Some("item-name")).unwrap().get("transport-belt"),
            Some("标准传送带")
        );
        assert_eq!(
            over.section(Some("entity-name")).unwrap().get("beacon"),
            Some("基础插件塔")
        );
    }
}
