# xi(羲)—— AI 原生微内核操作系统

> 伏羲,中华人文始祖,创八卦,演万法。xi 取其"以简驭繁、模块生万"之意。
> 本文件是无人值守自进化循环的方向锚。`generate_tasks` 自动读它生成对齐目标的新任务。
> 完成条目时把 `- [ ]` 改 `- [x]`,末尾【日志】追加 `YYYY-MM-DD 完成:<条目>`。

## 终局目标

把 xi 从「带 dev 循环的 CLI」进化成:**一个 AI 原生的微内核操作系统**——

- **核强而简**:内核只做 5 件事(进程调度/模块加载/LLM 运行时/事件总线/文件抽象),其余全是模块。
- **模块化**:所有功能(开发循环/HTTP 服务/TUI/模型管理/记忆/...)都是可增减的独立模块,像 Linux 驱动一样热插拔。
- **一条命令安装**:用户在已有 OS 上输入 `curl -sL https://xi.sh/install | sh` 即装好整个系统。
- **自带 Ollama 式模型管理**:内置模型下载(HuggingFace/ModelScope)、云端提供商(OpenRouter/DeepSeek/...)、预置一个小模型提供内核智能。
- **永久自进化**:dev 循环永不停止,持续从网络先进经验汲取,一直可用、一直进化。
- **服务端在 GitHub**:源码 + 发布 + 模块市场全在 GitHub,开源可审计。

## 架构:内核-驱动分离(比照 Linux)

```
┌─────────────────────────────────────────────────────────────────┐
│  用户空间 (User Space)                                           │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐           │
│  │ xi-dev   │ │ xi-serve │ │ xi-tui   │ │ xi-ollama│  ...      │
│  │(开发循环)│ │(HTTP API)│ │(终端 UI) │ │(模型管理)│           │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘           │
│       │             │             │             │                │
│  ═════╪═════════════╪═════════════╪═════════════╪══════════  │
│       │     模块接口 (Module API)     │                │
│  ═════╪═════════════╪═════════════╪═════════════╪══════════  │
│       │             │             │             │                │
│  ┌────┴─────────────┴─────────────┴─────────────┴─────┐      │
│  │              xi 内核 (Kernel)                        │      │
│  │  ┌─────────┐ ┌─────────┐ ┌────────┐ ┌──────────┐  │      │
│  │  │ 进程调度 │ │ 模块加载 │ │ LLM    │ │ 事件总线  │  │      │
│  │  │ (Task   │ │ (Module │ │ 运行时  │ │ (Event   │  │      │
│  │  │  SCHED) │ │  LOADER)│ │ (LLM   │ │  BUS)    │  │      │
│  │  │         │ │         │ │  RT)   │ │          │  │      │
│  │  └─────────┘ └─────────┘ └────────┘ └──────────┘  │      │
│  │  ┌───────────────────────────────────────────────┐ │      │
│  │  │ 文件抽象 (Workspace FS) + 网络栈 (HTTP/WS)    │ │      │
│  │  └───────────────────────────────────────────────┘ │      │
│  └────────────────────────────────────────────────────┘      │
├─────────────────────────────────────────────────────────────────┤
│  宿主 OS (Linux / macOS / Windows)                               │
└─────────────────────────────────────────────────────────────────┘
```

### 内核(Kernel)—— 只做 5 件事
| 组件 | 职责 | 类比 Linux |
|---|---|---|
| 进程调度 (Task SCHED) | 任务队列/优先级/并发控制/超时 | `schedule()` |
| 模块加载 (Module LOADER) | 发现/加载/卸载/隔离 xi-* 模块 | `insmod`/`rmmod` |
| LLM 运行时 (LLM RT) | 模型注册/推理调度/token 计量/多 provider 路由 | 驱动框架 |
| 事件总线 (Event BUS) | 模块间解耦通信(发布/订阅) | `netlink`/`udev` |
| 文件抽象 (Workspace FS) | 统一命名空间/权限/持久化 | `VFS` |

### 模块(Modules)—— 所有功能
| 模块 | 功能 | 状态 |
|---|---|---|
| xi-dev | 自进化开发循环(规划/执行/验证/提交) | ✅ 内核内置(过渡期) |
| xi-run | 单任务执行引擎 | ✅ 内核内置(过渡期) |
| xi-serve | HTTP API / Webhook | ✅ 内核内置(过渡期) |
| xi-ollama | 模型管理:pull/run/list/serve(本地推理) | 🔲 M3 |
| xi-llm | 云端 LLM 路由(OpenRouter/DeepSeek/...) | 🔲 M3 |
| xi-tui | 终端交互 UI(ratatui) | 🔲 M3 |
| xi-memory | 持久记忆(SQLite) | ✅ 内核内置(过渡期) |
| xi-config | 配置管理(分层/校验/migrate) | ✅ 内核内置(过渡期) |
| xi-install | 自安装/自更新 | 🔲 M2 |
| xi-market | 模块市场(搜索/安装/更新) | 🔲 M4 |
| xi-mcp | MCP 协议(作为 server + client) | 🔲 M5 |
| xi-sandbox | 模块沙箱(资源限制/网络隔离) | 🔲 M4 |

## 模型管理(Ollama 式)

### 预置提供商(开箱即用)
| 提供商 | 类型 | 用途 |
|---|---|---|
| 本地 Ollama 兼容 | 本地推理 | 内核智能(预置 1.5B 小模型) |
| OpenRouter | 云端聚合 | 主力开发(免费+付费模型) |
| DeepSeek | 云端 | 备用(中文强) |
| 火山方舟 | 云端 | 备用 |
| HuggingFace | 模型下载 | `xi model pull` 源 |
| ModelScope | 模型下载 | 国内镜像 |

### 内核智能(预置小模型)
- 预置一个 ≤3B 的小模型(如 Qwen2.5-1.5B-Instruct / Phi-3-mini),提供:
  - 任务分类/优先级判断
  - 简单问答/摘要
  - 模块健康检查
  - 离线可用(不依赖网络)
- 安装时自动下载,`xi model pull qwen2.5:1.5b` 即可

### 模型管理命令
```bash
xi model list              # 列出已下载/可用模型
xi model pull <name>       # 下载模型(HuggingFace/ModelScope)
xi model push <name>       # 推送模型到 HuggingFace
xi model run <name> "prompt"  # 用指定模型推理
xi model serve <name>      # 启动本地推理服务(Ollama 兼容 API)
xi model set default <name>  # 设置默认模型
xi model providers         # 列出/切换云端提供商
xi model rm <name>         # 删除模型
```

## 一键安装

```bash
curl -sL https://xi.sh/install | sh
```

安装流程:
1. 检测 OS( Linux/macOS/Windows )+ 架构(x86_64/aarch64)
2. 下载内核二进制(GitHub Releases)
3. 创建 `~/.xi/` 目录结构
4. 注册 `xi` 命令到 PATH
5. 下载预置小模型(可跳过: `--no-model`)
6. 初始化配置(检测已有 LLM key)
7. 启动内核守护进程(可选: `--daemon`)

## 里程碑(按顺序推进)

### M0 地基:内核核心(已完成大部分)
- [x] 任务执行引擎(run 模式)
- [x] 配置管理(config)
- [x] LLM 客户端(OpenAI 兼容)
- [x] 工作区抽象(workspace)
- [x] 模块发现机制(~/.xi/bin/ 扫描)
- [ ] 事件总线(模块间 pub/sub)
- [ ] 进程调度器(优先级队列/并发控制)
- [ ] 内核日志(ring buffer + 级别过滤)

### M1 模型管理(Ollama 式)
- [ ] `xi model list`:列出已下载模型 + 可用云端模型
- [ ] `xi model pull <name>`:从 HuggingFace/ModelScope 下载 GGUF/Safetensors
- [ ] `xi model run <name> "prompt"`:本地推理(调用 llama.cpp / candle)
- [ ] `xi model serve <name>`:启动本地 HTTP 推理服务(Ollama 兼容 /api/generate)
- [ ] `xi model providers`:云端提供商管理(增删/切换/测试连通)
- [ ] 预置小模型:Qwen2.5-1.5B-Instruct(安装时自动下载)
- [ ] 模型路由:自动选最优模型(本地/云端,按任务复杂度)
- [ ] token 计量:每模型/每 provider 用量统计

### M2 一键安装 + 自更新
- [ ] 安装脚本(install.sh):检测 OS+架构,下载,注册,初始化
- [ ] GitHub Actions:自动构建 release(x86_64/aarch64, Linux/macOS)
- [ ] `xi install --update`:自更新内核(检查 GitHub latest release)
- [ ] `xi install --model <name>`:安装时指定预置模型
- [ ] 安装后自检:`xi health` 验证全链路
- [ ] 卸载:`xi install --uninstall` 清理

### M3 模块系统成熟
- [ ] 模块 API 规范:manifest.toml schema(接口/依赖/权限声明)
- [ ] 模块生命周期:load/unload/reload/health
- [ ] 模块间通信:事件总线(pub/sub,JSON 消息)
- [ ] 模块沙箱:文件访问限制/网络白名单/资源配额
- [ ] 内置功能拆出:serve/tui/memory 拆为独立模块
- [ ] 模块热加载:运行中加载/卸载(不需要重启)
- [ ] `xi module load/unload/status` 管理命令

### M4 模块市场
- [ ] `xi market search <keyword>`:搜索可用模块(GitHub topics)
- [ ] `xi market install <owner/repo>`:安装社区模块
- [ ] `xi market update`:更新所有模块
- [ ] `xi market info <name>`:查看模块详情
- [ ] 模块签名:验证模块完整性(防恶意模块)
- [ ] 模块排行榜:按下载量/评分排序

### M5 对标业界(AI Agent 最佳实践)
- [ ] MCP 协议:xi 作为 MCP server(暴露工具)+ client(调用外部工具)
- [ ] Streaming:LLM 响应流式输出
- [ ] 多文件编辑:单次调用编辑多文件(参考 Aider)
- [ ] 上下文管理:自动摘要/裁剪(参考 Claude Code)
- [ ] 工具调用协议:结构化 tool_use(参考 Anthropic/OpenAI)
- [ ] Git 工作流:自动分支/PR(参考 Claude Code)
- [ ] 会话持久化:保存/恢复(参考 Cursor)
- [ ] 子智能体:嵌套任务分解(参考 CrewAI)

### M6 桌面/移动端
- [ ] TUI 完整:交互式终端(ratatui,参考 Warp/Ghostty)
- [ ] Web 仪表盘:浏览器管理(serve 内嵌)
- [ ] 桌面 App:Tauri 包装(参考 Tauri 生态)
- [ ] 移动 Agent:手机触发/监控(参考 OpenAI 移动 API)

### M7 多节点集群
- [ ] 多机协作:分布式任务调度
- [ ] 模型联邦:多节点共享模型(参考 federated learning)
- [ ] 去中心化市场:IPFS 分发模块
- [ ] 容器化:xi 在 Docker/K8s 中运行

## 学习原则(别闭门造车)

> **核心指令:不要凭空发明功能,必须从 GitHub 等网络的先进开源项目中汲取经验,对标业界最佳实践,不断完善自我,设法达到自我完美。**

### 对标项目
- **Linux / FreeBSD / ZFS** — 内核-驱动分离、模块化、VFS
- **Ollama / llama.cpp / vLLM** — 本地模型推理、模型管理
- **Claude Code / Aider / Continue / Cline / Cursor** — AI Agent 交互
- **LangGraph / CrewAI / AutoGen** — 多智能体编排
- **Docker / Podman / NixOS** — 模块化、可复现、声明式
- **Hurl / mitmproxy / caddy** — 网络调试、反向代理
- **Ratatui / tokio / axum / clap** — Rust 生态

### 生成任务要求
1. 每个任务标注参考来源(项目名+具体机制)
2. 优先移植已验证模式,不从零设计
3. 定期生成"对标差距分析"任务
4. 吸收对标项目新特性(如 MCP、streaming)
5. 每 milestone 完成后生成"自我审视"任务

## 硬约束
- 每个任务必须能被 `cargo test`(或编译)验证,否则不算完成。
- 不泄露/修改密钥,不推送远端(除 github remote),不做安全敏感操作。
- 保持向后兼容;任务失败要回滚,不污染下一个任务。
- 内核代码保持精简(≤5000 行);功能加到模块,不加到内核。
- 模块必须可独立构建、独立测试、独立卸载。

## 日志
<!-- YYYY-MM-DD 完成:<条目> -->
2026-10-26 完成:扩展发现机制 + xi extensions 子命令
2026-10-27 完成:OS 级愿景定义 + 内核-模块架构设计
