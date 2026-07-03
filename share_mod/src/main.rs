use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ModListJson {
    pub mods: Vec<ModInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModInfo {
    pub name: String,
    pub enabled: bool,
    pub version: Option<String>,
}

fn main() {
    let mod_list = PathBuf::from("mod-list.json");
    let mod_list = std::fs::read_to_string(mod_list).expect("无法读取 mod-list.json 文件");
    let mod_list: ModListJson =
        serde_json::from_str(&mod_list).expect("无法解析 mod-list.json 文件");

    for mod_info in &mod_list.mods {
        println!(
            "Mod Name: {}, Enabled: {}, Version: {}",
            mod_info.name,
            mod_info.enabled,
            mod_info.version.as_deref().unwrap_or("N/A")
        );
    }
}
