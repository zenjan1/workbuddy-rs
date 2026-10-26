# WorkBuddy 自主开发区

- `backlog.json` — 开发任务清单(优先级高 → 低,`workbuddy dev` 按 priority 取任务)
- `state.json` — 循环进度(done/failed/历史),自动生成,**不进 git**
- `failures/` — 失败任务的排查报告(验证输出 + 修复尝试)

运行:

```bash
workbuddy dev                 # 跑一个任务(默认验证: 任务 verify 字段或 --verify)
workbuddy dev --tasks 5 --minutes 120
workbuddy dev --dry-run       # 只看计划不执行
workbuddy dev --retry-failed  # 重试失败任务
```

长期驻留(一个月级):用 nohup/setsid 后台运行 `workbuddy dev --minutes 43200`,
或待 watch 子命令(backlog 热加载)完成后改用 `workbuddy watch`。

停止:`kill $(cat .workbuddy/dev.pid)`(若用脚本启动);进度保存在 state.json,重启后从断点继续。
