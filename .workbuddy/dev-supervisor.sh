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
