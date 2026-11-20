-- 启动提示配置：当前启用通用版提示，后续部署在线网页定制工具后可直接切换为 WEB_TOOL_NOTICE
local GENERAL_PACK_NOTICE =
  "[tanvec-ai-cn] 你当前使用的是通用汉化包：为避免未安装模组篡改原版文本，与原版词条冲突的翻译（*.override.cfg.disabled）默认禁用。"
  .. "如需完整启用当前存档模组（含覆盖词条），可在 mods 目录解压后运行 locale/zh-CN/toggle_py，或使用 gavin-server 生成专属精简汉化包。"

-- 预留：在线网页生成工具上线后，将 ACTIVE_NOTICE 切换为下面这条即可
-- local WEB_TOOL_NOTICE =
--   "[tanvec-ai-cn] 你目前使用的是通用版本，为避免词条互相覆盖，部分重名译名已默认禁用。"
--   .. "请访问 <在线网页地址> 上传你的 mod-list.json，即可下载完美覆盖你当前 mod 包的专属翻译。"

local ACTIVE_NOTICE = GENERAL_PACK_NOTICE

script.on_event(defines.events.on_player_created, function(event)
  local player = game.get_player(event.player_index)
  if player and player.valid then
    player.print(ACTIVE_NOTICE)
  end
end)

script.on_configuration_changed(function(data)
  if data and data.mod_changes and data.mod_changes[script.mod_name] then
    game.print(ACTIVE_NOTICE)
  end
end)
