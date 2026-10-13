# workbuddy VISION —— 目标:完整的自进化多智能体工作台

> 本文件是无人值守自进化循环的方向锚。`generate_tasks` 会自动读它来生成对齐目标的新任务。
> 完成一个条目时,把 `- [ ]` 改成 `- [x]`,并在文件末尾【日志】追加一行 `YYYY-MM-DD 完成:<条目>`。

## 终局目标
把 workbuddy 从「带 dev 循环的 CLI」进化成:**能自己规划/执行/验证/改进、可通过交互客户端使用、成本可控、可长期无人值守自我进化的多智能体工作台**。

## 路线图(按里程碑,顺序即大方向)

### M0 地基:循环可靠性与可观测性
- [ ] 每 run 记录 token/字符用量并在 show 显示
- [ ] `status` 子命令:supervisor 是否在跑 + 已完成/失败数 + 最近日志(无人值守命脉)
- [ ] `health` 子命令:探测 LLM 端点连通性
- [ ] `selfcheck` 子命令:cargo test/clippy 一键自检
- [ ] `undo` 子命令:安全撤销上次 dev 提交
- [ ] task_id 用标题替代索引(向后兼容)
- [ ] 集成测试:CLI 离线全链路(MockLlm)
- [ ] 每任务 LLM 调用总预算(防失控烧 token)

### M1 核心使用功能
- [ ] `stats` 子命令:运行历史统计
- [ ] 持久记忆子系统(memory add/list/get)
- [ ] `run` 模式接入 edit/shell/read 工具
- [ ] 成本估算与显示(config 单价)
- [ ] 预算保险丝:超 max_budget 自动停(500 元保险)
- [ ] `config set` 子命令
- [ ] `list --since` 按日期过滤 / `show --json` 完整实现
- [ ] `agents.toml` 扩展自定义专家
- [ ] `dev --retry-failed` / backlog `skip` 字段
- [ ] `watch` 热加载 / `bench` 延迟基准

### M2 客户端与接入层
- [ ] `serve`:HTTP 只读接口(/health、/runs、/runs/{id})
- [ ] `serve`:POST /run 触发运行
- [ ] TUI 骨架(ratatui 渲染 backlog 列表)
- [ ] TUI:触发 run/dev + 实时日志
- [ ] Web 仪表盘(serve 内嵌静态页)
- [ ] 事件/Webhook:任务完成/失败回调

### M3 真正的自进化
- [ ] 自评审:提交前用 reviewer 审自己 diff
- [ ] 路线图自动推进:完成 milestone 时更新本文件状态
- [ ] 质量门:feature 任务必须带测试
- [ ] `dev --parallel`(git worktree 并行)

## 硬约束
- 每个任务必须能被 `cargo test`(或编译)验证,否则不算完成。
- 不泄露/修改密钥,不推送远端,不做安全敏感操作。
- 保持向后兼容;任务失败要回滚,不污染下一个任务。

## 日志
<!-- YYYY-MM-DD 完成:<条目> -->
