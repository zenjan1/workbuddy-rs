#!/usr/bin/env bash
# workbuddy 长期自主开发 supervisor
# - 每轮 dev 循环带 30 天时长预算 + backlog 自补给(--extend)
# - 进程崩溃后 5 分钟自动重启;正常退出(backlog 耗尽且补给达上限)则停止
# 启动: setsid bash .workbuddy/dev-supervisor.sh > /dev/null 2>&1 &
# 监控: tail -f .workbuddy/dev.log
# 停止: kill -- -$(cat .workbuddy/dev.pid)   (杀掉整个进程组)
set -u
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO" || exit 1
LOG="$REPO/.workbuddy/dev.log"
PIDFILE="$REPO/.workbuddy/dev.pid"
echo $$ > "$PIDFILE"
echo "[supervisor] $(date '+%F %T') 启动 pid=$$ repo=$REPO" >> "$LOG"
# 启动前:把操作者遗留的未提交改动以 wip 提交,
# 避免 dev 循环把它们当作任务产出提交(早期事故的根因)。
if [ -n "$(git status --porcelain)" ]; then
  git add -A
  git commit -m "wip: uncommitted changes before dev loop" >> "$LOG" 2>&1
  echo "[supervisor] 启动前工作区不干净,已以 wip 提交" >> "$LOG"
fi
while true; do
  ~/.local/bin/workbuddy dev --repo "$REPO" --verify "cargo test" --minutes 43200 --extend 10 >> "$LOG" 2>&1
  rc=$?
  if [ "$rc" -eq 0 ]; then
    echo "[supervisor] $(date '+%F %T') dev 循环正常退出(rc=0),supervisor 结束" >> "$LOG"
    break
  fi
  echo "[supervisor] $(date '+%F %T') dev 循环异常退出(rc=$rc),5 分钟后重启" >> "$LOG"
  sleep 300
done
rm -f "$PIDFILE"
