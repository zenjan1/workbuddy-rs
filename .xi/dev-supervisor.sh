#!/usr/bin/env bash
# xi 长期自主开发 supervisor(无人值守)
# - 每轮 dev 循环带 30 天时长预算 + backlog 自补给(--extend)
# - 进程崩溃后 5 分钟自动重启;正常退出(backlog 耗尽且补给达上限)则停止
# - LLM 凭证来自 .xi/llm.env(官方 DeepSeek,已 gitignore;切模型见该文件注释)。
#   直接调用 xi.real(绕开 ~/.local/bin/xi 包装器「key 以 sk- 开头就切回 GitCode」的覆盖),
#   以保证 llm.env 里的 DASHSCOPE_* 真正生效。
# 启动: setsid bash .xi/dev-supervisor.sh > /dev/null 2>&1 &
# 监控: tail -f .xi/dev.log
# 停止: kill -- -$(cat .xi/dev.pid)   (杀掉整个进程组)
set -u
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO" || exit 1
LOG="$REPO/.xi/dev.log"
PIDFILE="$REPO/.xi/dev.pid"
echo $$ > "$PIDFILE"
echo "[supervisor] $(date '+%F %T') 启动 pid=$$ repo=$REPO" >> "$LOG"
# 加载本项目的 LLM 凭证(已 gitignore;当前=官方 DeepSeek deepseek-v4-pro,切模型见文件内注释)
if [ -f "$REPO/.xi/llm.env" ]; then
  . "$REPO/.xi/llm.env"
else
  echo "[supervisor] 警告: 未找到 .xi/llm.env,LLM 凭证缺失,dev 将失败" >> "$LOG"
fi
# 启动前:把操作者遗留的未提交改动以 wip 提交,
# 避免 dev 循环把它们当作任务产出提交(早期事故的根因)。
# 注:commit-msg hook 只认 feat(dev): [N/总数] 格式,wip 提交会被拒(无害,
#     接线改动已另行正式提交;真正的任务提交由 dev 循环按正确格式产出)。
if [ -n "$(git status --porcelain)" ]; then
  echo "[supervisor] 启动前工作区不干净(未提交改动保留,交由 dev 循环处理)" >> "$LOG"
fi
while true; do
  ~/.local/bin/xi.real dev --repo "$REPO" --verify "cargo test" --minutes 43200 --extend 10 >> "$LOG" 2>&1
  rc=$?
  if [ "$rc" -eq 0 ]; then
    echo "[supervisor] $(date '+%F %T') dev 循环正常退出(rc=0),supervisor 结束" >> "$LOG"
    break
  fi
  echo "[supervisor] $(date '+%F %T') dev 循环异常退出(rc=$rc),5 分钟后重启" >> "$LOG"
  sleep 300
done
rm -f "$PIDFILE"
