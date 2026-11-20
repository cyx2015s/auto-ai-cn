#! python3

"""
自动根据mod-list.json的内容调整locale文件夹下zh-CN中每个文件名的后缀
应该放置在zh-CN文件夹中，运行后会自动修改zh-CN文件夹下的每个文件名
mods/tanvec-ai-cn/zh-CN/...cfg
mods/mod-list.json
"""

import json
import os
from pathlib import Path


def main():
    mod_list_json_path = Path(__file__).parent.parent.parent.parent / "mod-list.json"
    print(f"正在读取 {mod_list_json_path}...")
    with open(mod_list_json_path, "r", encoding="utf-8") as f:
        mod_list = json.load(f)

    enabled_map = {
        mod["name"]: mod.get("enabled", True)
        for mod in mod_list.get("mods", [])
        if "name" in mod
    }

    for file in Path(__file__).parent.iterdir():
        if not file.is_file():
            continue
        if file.name.endswith(".cfg.disabled"):
            base_cfg_name = file.name[:-9]  # 去掉 ".disabled"
        elif file.name.endswith(".cfg"):
            base_cfg_name = file.name
        else:
            continue

        # 支持 <mod_name>[.top][.override].cfg[.disabled]
        mod_name = base_cfg_name.split(".")[0]
        enabled_name = base_cfg_name
        disabled_name = f"{base_cfg_name}.disabled"

        print(f"正在处理 {file.name}，对应mod名称为 {mod_name}...")
        if mod_name in enabled_map:
            enabled = enabled_map[mod_name]
            print(f"找到mod {mod_name}，enabled={enabled}")
            target_name = enabled_name if enabled else disabled_name
            if file.name != target_name:
                new_path = file.parent / target_name
                print(f"重命名 {file.name} 为 {target_name}")
                os.replace(file, new_path)
        else:
            if file.name != disabled_name:
                new_path = file.parent / disabled_name
                print(f"重命名 {file.name} 为 {disabled_name}")
                os.replace(file, new_path)


main()
