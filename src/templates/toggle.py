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
    mod_list_json_path = Path(__file__).parent.parent.parent.parent / 'mod-list.json'
    print(f"正在读取 {mod_list_json_path}...")
    with open(mod_list_json_path, 'r', encoding='utf-8') as f:
        mod_list = json.load(f)

    for mod in mod_list["mods"]:
        mod_name = mod['name']
        enabled = mod['enabled']
        disabled_name = f"{mod_name}.disabled"
        enabled_name = f"{mod_name}.cfg"
        print(f"正在处理 {mod_name}...")
        if enabled:
            # 如果mod启用，确保文件名是enabled_name
            if os.path.exists(disabled_name):
                os.rename(disabled_name, enabled_name)
        else:
            # 如果mod禁用，确保文件名是disabled_name
            if os.path.exists(enabled_name):
                os.rename(enabled_name, disabled_name)

main()
