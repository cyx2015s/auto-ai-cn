//! 翻译包打包
//!
//! 将 cache 目录中的所有翻译缓存合并打包为 1 个 Factorio mod zip。
//!
//! 输出结构：
//! ```text
//! <pack_name>_<date>/
//!   locale/zh-CN/<mod_name>.cfg   （每个源 mod 一个文件）
//!   info.json
//! ```
//!
//! 翻译包不声明对源 mod 的依赖——未安装的源 mod 的翻译条目不会被游戏加载。

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context;
use chrono::Utc;
use log::{info, warn};
use serde_json;

use crate::translation;

/// 将 cache 目录中所有翻译缓存打包为 1 个 Factorio mod zip。
///
/// 每个源 mod 的 zh-CN 翻译合并为一个 `.cfg` 文件。
///
/// 如果 `protect` 为 true 且提供了 `base_keys`，检测到 mod 翻译覆盖原版 key 时，
/// 后缀用 `.cfg.disable` 而非 `.cfg`，避免覆盖原版翻译。
pub fn pack_all_to_one_mod(
    cache_dir: &Path,
    output_dir: &Path,
    pack_name: &str,
    protect: bool,
    base_keys: Option<&std::collections::HashMap<String, String>>,
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

    let mut packed_count = 0;

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

        // 合并该 mod 的所有翻译文件为一个 INI
        let mut merged_ini = ini::Ini::new();
        for (_file_name, file_content) in &target_lang.contents {
            let ini = match translation::str_to_ini(file_content) {
                Ok(ini) => ini,
                Err(e) => {
                    warn!("无法解析 {} 的翻译文件: {}", mod_name, e);
                    continue;
                }
            };
            for (section, props) in ini.iter() {
                for (k, v) in props.iter() {
                    merged_ini.with_section(section).set(k, v);
                }
            }
        }

        if merged_ini.is_empty() {
            continue;
        }

        let ini_str = translation::ini_to_str(&merged_ini)?;

        // protect 模式下检查是否覆盖原版 key
        let suffix = if protect && let Some(base_keys) = base_keys {
            let overlaps = has_base_overlap(&merged_ini, base_keys);
            if overlaps {
                info!("  ↳ {} 覆盖原版翻译，标记为 .cfg.disabled", mod_name);
                "cfg.disabled"
            } else {
                "cfg"
            }
        } else {
            "cfg"
        };

        let zip_path = format!("{}/locale/zh-CN/{}.{}", pack_name, mod_name, suffix);
        zip_writer.start_file(&zip_path, options)?;
        zip_writer.write_all(ini_str.as_bytes())?;
        packed_count += 1;
    }

    // toggle.py， 官网不允许传带可执行文件的 mod 包，作此变通
    let zip_path = format!("{}/locale/zh-CN/toggle_py", pack_name);
    const TOGGLE_PY: &'static str = include_str!("templates/toggle.py");
    zip_writer.start_file(&zip_path, options)?;
    zip_writer.write_all(TOGGLE_PY.as_bytes())?;

    // info.json
    let info_json = serde_json::json!({
        "name": pack_name,
        "version": today,
        "title": "切向量的 AI 汉化",
        "factorio_version": "2.1",
        "dependencies": ["base >= 2.1"],
        "author": "tanvec",
            "description": format!("包含 {} 个模组的 AI 中文翻译。在模组官网上查看详细更新信息。手动解压可以按需求启用翻译，请查看 locale/zh-CN/ 下的 toggle_py 文件。默认禁用了所有会覆盖原版游戏词条的 mod 的翻译内容。联机时建议所有玩家都使用解压版本的 mod，并且手动运行 Python 脚本只启用对应翻译。", packed_count)
    });
    let info_path = format!("{}/info.json", pack_name);
    zip_writer.start_file(&info_path, options)?;
    zip_writer.write_all(serde_json::to_string_pretty(&info_json)?.as_bytes())?;

    zip_writer.finish()?;

    info!("翻译包已生成: {:?} ({} 个 mod)", output_path, packed_count);
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

/// 检查 INI 中是否有任何 key 出现在原版 key 集合中
fn has_base_overlap(ini: &ini::Ini, base_keys: &std::collections::HashMap<String, String>) -> bool {
    for (section, props) in ini.iter() {
        if !is_protected(section.unwrap_or("")) {
            continue;
        }
        let sec_prefix = section.map_or_else(String::new, |s| format!("{}.", s));
        for (key, value) in props.iter() {
            if let Some(base_value) = base_keys.get(&format!("{}{}", sec_prefix, key))
                && base_value != value
            {
                log::debug!(
                    "覆盖原版 key: {}{} ({} != {})",
                    sec_prefix,
                    key,
                    value,
                    base_value
                );
                return true;
            }
        }
    }
    false
}
