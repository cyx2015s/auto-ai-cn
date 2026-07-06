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

    for file in Path(__file__).parent.iterdir():
        if file.is_file() and (file.suffix == ".cfg" or file.suffix == ".disabled"):
            # print(f"正在处理 {file.name}...")
            mod_name = file.stem
            if mod_name.endswith(".cfg"):
                mod_name = mod_name[:-4]  # 去掉 .cfg 后缀
            mod_info = next(
                (mod for mod in mod_list["mods"] if mod["name"] == mod_name), None
            )
            if mod_info is not None:
                print(f"找到mod {mod_name}，enabled={mod_info['enabled']}")
                enabled = mod_info["enabled"]
                disabled_name = f"{mod_name}.cfg.disabled"
                enabled_name = f"{mod_name}.cfg"
                if enabled:
                    # 如果mod启用，确保文件名是enabled_name
                    if file.name != enabled_name:
                        new_path = file.parent / enabled_name
                        print(f"重命名 {file.name} 为 {enabled_name}")
                        os.rename(file, new_path)
                else:
                    # 如果mod禁用，确保文件名是disabled_name
                    if file.name != disabled_name:
                        new_path = file.parent / disabled_name
                        print(f"重命名 {file.name} 为 {disabled_name}")
                        os.rename(file, new_path)
            else:
                if file.name.endswith(".cfg.disabled"):
                    # 如果文件名已经是.disabled，保持不变
                    continue
                else:
                    print(f"重命名 {file.name} 为 {file.stem}.cfg.disabled")
                    os.rename(file, file.with_suffix(".cfg.disabled"))


main()
