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

# 自主开发循环(在 git 仓库里持续改代码,详见下文 dev 一节)
workbuddy dev --tasks 5 --verify "cargo test"

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
├── main.rs       # CLI(clap 子命令:run/list/show/agents/config/dev)
├── config.rs     # 配置加载(环境变量 > 配置文件 > 默认)
├── llm.rs        # Llm trait:OpenAI 兼容客户端 + 离线 Mock
├── agents.rs     # 专家目录(角色 + skills,静态数据)
├── planner.rs    # 规划提示词(run/dev 两套)+ 幻觉容忍修复
├── plan.rs       # Plan/Step 结构、JSON 解析与校验、拓扑序
├── executor.rs   # 拓扑并行执行、失败传播(跳过下游)、shell/read/edit 步骤
├── edits.rs      # edit 步骤的 edits JSON 解析/应用 + shell 安全护栏
├── dev.rs        # 自主开发循环:backlog/state/git 提交/自修复/预算/自补给
├── workspace.rs  # 任务工作区持久化(meta/plan/report/outputs)
└── util.rs       # 字符安全截断等
```

## 测试

```bash
cargo test        # 33 个单元测试:计划解析/校验、Mock 端到端、dev 循环离线集成等
```

## 已知限制

- 无联网检索能力(调研类 skill 依赖模型自身知识);
- 无桌面 GUI / IM 集成(原版的微信/Slack 接入未复刻);
- 计划为单次生成,不支持多轮修订对话;
- shell 步骤在用户工作目录下直接执行,请信任你交给它的目标。

## 自主开发模式(dev)

让 WorkBuddy 在**一个 git 仓库**里长期自主开发:从任务清单(backlog)取任务 →
LLM 规划(read/edit/shell 步骤)→ 修改代码 → 运行验证命令 → 失败自动修复(≤3 次)→
git 提交 → 下一个任务。支持中断恢复与长期运行(月级)。

```bash
cd your-repo            # dev 模式在"当前目录"(或 --repo 指定)的 git 仓库中操作

workbuddy dev --tasks 1 --verify "cargo test"   # 跑 1 个任务,验证命令 cargo test
workbuddy dev --minutes 43200 --extend 10       # 跑满 30 天;backlog 耗尽时每轮自动生成 10 个新任务
workbuddy dev --mock                              # 离线演练(内置 Mock LLM,不消耗 API)
```

### 参数

| 参数 | 说明 |
|---|---|
| `--backlog <path>` | 任务清单文件(默认 `<repo>/.workbuddy/backlog.json`) |
| `--verify <cmd>` | 全局验证命令(任务未指定自己的 `verify` 时使用) |
| `--tasks N` | 本次最多完成 N 个任务 |
| `--minutes M` | 本次最多运行 M 分钟 |
| `--repo <dir>` | 操作仓库目录(默认当前目录) |
| `--extend N` | backlog 耗尽时,每轮调用 LLM 生成 N 个新任务并追加进 backlog(可重复补给,支撑月级运行) |
| `--mock` | 使用内置 Mock LLM(离线演练) |

### backlog 格式(`<repo>/.workbuddy/backlog.json`)

```json
{
  "tasks": [
    {
      "title": "为 util::clip 补充边界单元测试",
      "description": "在 src/util.rs 测试模块新增:空字符串、恰好 max、超过 max。",
      "verify": "cargo test -- clip",
      "priority": 29
    }
  ]
}
```

- `verify` 缺省时使用 `--verify` 的值;
- `priority` 越大越先执行。

### 完成判定与可靠性

- 一个任务**只有同时满足"验证命令通过"且"产生了真实 git 提交"才计为完成**
  (防止"验证本来就绿、实际零改动"的假成功);
- 失败任务自动 `git checkout` + `git clean` 回滚脏改动,不污染下一个任务;
- 规划器对 LLM 幻觉有容忍修复:未知专家回退、read 缺 path 从 prompt 提取、
  shell 缺 command 从 prompt 提取或降级、文件路径按 basename 唯一匹配兜底;
- 进度持久化在 `.workbuddy/state.json`(不进 git),中断后重启自动从断点继续;
- shell 步骤有安全护栏:拒绝 `git push`/`sudo`/`rm -rf /` 等危险命令。

### 长期运行(supervisor)

`.workbuddy/dev-supervisor.sh` 提供带崩溃自动重启的后台运行:

```bash
setsid bash .workbuddy/dev-supervisor.sh > /dev/null 2>&1 &   # 启动(30 天预算 + 自补给)
tail -f .workbuddy/dev.log                                     # 监控
kill -- -$(cat .workbuddy/dev.pid)                             # 停止(杀整个进程组)
```

> 注意:dev 模式会**直接修改仓库代码并产生本地提交**。启动前请确认仓库状态干净、
> 你信任该仓库被持续修改;它只创建本地 commit,从不 push。
