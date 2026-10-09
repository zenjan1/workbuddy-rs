# WorkBuddy (Rust 复刻)

> WorkBuddy 的非官方 Rust 复刻版:一个**多专家 AI 智能体工作台**。
> 复刻其核心范式 —— "说出目标 → 规划师拆解 → 多专家智能体并行执行 → 终审综合交付"。
> 与原项目(腾讯 WorkBuddy 桌面客户端)无任何关联,独立实现。

## 它做什么

给它一个任务目标,它会:

1. **规划(Planner)**:调用 LLM 生成结构化 JSON 计划 —— 拆成 2~8 个步骤,每步指派一个专家角色 + skill,并声明步骤间依赖;
2. **执行(Executor)**:按拓扑序**并行**执行(无依赖的步骤同时跑,受并行度限制);
   - `llm` 步骤:把 专家系统提示 + skill 指令 + 上游步骤产出 作为上下文,调用 LLM;
   - `shell` 步骤:在本地运行命令(60s 超时),捕获输出;
   - 某步失败时,其全部下游自动标记为跳过;
3. **交付**:终审专家综合所有产出,生成最终报告;全过程持久化到工作区,可随时回溯。

## 内置专家

| 角色 | id | 职责 | Skills |
|---|---|---|---|
| 统筹规划师 | `planner` | 任务拆解与验收 | task-breakdown, acceptance |
| 调研专家 | `researcher` | 资料调研与核验 | deep-research, source-verification |
| 内容撰稿 | `writer` | 报告/文档写作 | report-writing, doc-structuring |
| 工程师 | `coder` | 代码与工程 | code-writing, code-review |
| 数据分析师 | `analyst` | 数据洞察 | data-analysis, insight |
| 质量审核 | `reviewer` | 事实核查与终审 | fact-check, final-review |

## 安装

### 依赖

- Rust 工具链(rustc 1.75+)
- 一个 OpenAI 兼容的 LLM API key(默认走阿里云百炼 DashScope)

### 构建

```bash
cargo build --release
# 二进制位于 target/release/workbuddy
# 可选:安装到 PATH
install -m755 target/release/workbuddy ~/.local/bin/workbuddy
```

### 配置

优先级:**命令行 > 环境变量 > 配置文件(`workbuddy.toml` / `~/.config/workbuddy/config.toml`)**。

| 环境变量 | 说明 | 默认 |
|---|---|---|
| `DASHSCOPE_API_KEY` 或 `OPENAI_API_KEY` | API key | 无(必填,或加 `--mock`) |
| `DASHSCOPE_BASE_URL` | OpenAI 兼容端点 | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `DASHSCOPE_MODEL` | 模型名 | `qwen-plus` |

配置文件示例 `~/.config/workbuddy/config.toml`:

```toml
model = "qwen-plus"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
max_parallel = 4
workspace_dir = "/home/a/.local/share/workbuddy"
request_timeout_secs = 120
```

## 使用

```bash
# 执行一个任务(真实 LLM)
workbuddy run "调研一下 Rust 与 Go 在并发模型上的差异,写一份对比简报"

# 离线演练(不调用 API,用内置 Mock,验证全链路)
workbuddy run --mock "随便一个任务"

# 指定模型 / 并行度
workbuddy run --model qwen-max --parallel 2 "..."

# 列出历史任务
workbuddy list

# 查看某次任务的计划、步骤产出与报告
workbuddy show <任务ID>

# 查看内置专家目录
workbuddy agents

# 查看当前生效配置(API key 已脱敏)
workbuddy config
```

## 工作区

每次 `run` 在 `workspace_dir`(默认 `~/.local/share/workbuddy`)下生成一个任务目录:

```
<run-id>/
├── meta.json      # 任务元数据 + 每步结果
├── plan.json      # 规划器产出的原始计划
├── report.md      # 最终报告(含计划、最终产出、执行摘要)
└── outputs/
    ├── 1.md       # 各步骤完整产出
    ├── 2.md
    └── ...
```

## 架构

```
src/
├── main.rs       # CLI(clap 子命令:run/list/show/agents/config)
├── config.rs     # 配置加载(环境变量 > 配置文件 > 默认)
├── llm.rs        # Llm trait:OpenAI 兼容客户端 + 离线 Mock
├── agents.rs     # 专家目录(角色 + skills,静态数据)
├── planner.rs    # 规划提示词 + 计划解析入口
├── plan.rs       # Plan/Step 结构、JSON 解析与校验、拓扑序
├── executor.rs   # 拓扑并行执行、失败传播(跳过下游)、shell 步骤
├── workspace.rs  # 任务工作区持久化(meta/plan/report/outputs)
└── util.rs       # 字符安全截断等
```

## 测试

```bash
cargo test        # 19 个单元测试:计划解析/校验、Mock 端到端、shell 步骤、工作区生命周期
```

## 已知限制

- 无联网检索能力(调研类 skill 依赖模型自身知识);
- 无桌面 GUI / IM 集成(原版的微信/Slack 接入未复刻);
- 计划为单次生成,不支持多轮修订对话;
- shell 步骤在用户工作目录下直接执行,请信任你交给它的目标。

## 自主开发模式(dev)

启用开发模式进行快速验证：

```bash
workbuddy dev --tasks 5 --minutes 30
```

### 参数说明
- `--tasks N`：指定生成任务数量（N为正整数）
- `--minutes M`：设置每个任务的持续时间（M为1-60的整数）

### backlog 文件位置
系统自动生成的待办事项存储于：
`.workbuddy/backlog.json`

### 验证机制
1. **格式校验**：JSON文件需包含 `tasks` 数组和 `timestamp` 字段
2. **时间戳验证**：自动校验文件创建时间与系统时间差值在±5分钟内
3. **数据完整性**：每个任务对象必须包含 `id`、`content` 和 `duration` 属性
