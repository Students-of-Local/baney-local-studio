#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: LOCAL_STUDIO_SMOKE_MODEL=<provider>/<model> $0 <linux app> <eukhe> <new scratch dir>"; exit 2; }
app=$(realpath "$1") eukhe=$(realpath "$2") dir=$3
model=${LOCAL_STUDIO_SMOKE_MODEL:?} base=${LOCAL_STUDIO_SMOKE_BASE_URL:-http://127.0.0.1:11434/v1}
umask 077
mkdir "$dir" && mkdir "$dir/home" "$dir/agent" || exit 2
dir=$(realpath "$dir")
echo '{}' > "$dir/agent/auth.json"
echo "{\"defaultProvider\":\"${model%%/*}\",\"defaultModel\":\"${model#*/}\",\"defaultThinkingLevel\":\"off\",\"retry\":{\"provider\":{\"streamIdleTimeoutMs\":900000}}}" > "$dir/agent/settings.json"
echo "{\"providers\":{\"${model%%/*}\":{\"baseUrl\":\"$base\",\"api\":\"openai-completions\",\"apiKey\":\"none\",\"models\":[{\"id\":\"${model#*/}\",\"reasoning\":false,\"input\":[\"text\"],\"contextWindow\":32768,\"maxTokens\":8192,\"cost\":{\"input\":1,\"output\":1,\"cacheRead\":0,\"cacheWrite\":0}}]}}}" > "$dir/agent/models.json"
stop() {
  for pid in $(pgrep -u "$(id -u)" eukhe); do
    tr '\0' '\n' 2>/dev/null < "/proc/$pid/environ" | grep -qxF "EUKHE_DAEMON_SOCKET=$dir/d.sock" && kill "$pid"
  done
}
trap stop EXIT
env HOME="$dir/home" SHELL=/bin/bash LOCAL_STUDIO_DATA_DIR="$dir/data" EUKHE_CODING_AGENT_DIR="$dir/agent" \
  EUKHE_DAEMON_SOCKET="$dir/d.sock" EUKHE_BIN="$eukhe" LOCAL_STUDIO_SMOKE_MODEL="$model" GDK_BACKEND=x11 GSK_RENDERER=cairo \
  timeout 600 xvfb-run -a -s '-screen 0 1600x1000x24' dbus-run-session -- "$app" --smoke > "$dir/log" 2>&1
code=$?
steps=(
  'ok   a rejected send came back before what was typed since, text and files'
  '     handed back true, taken back true, left a start true true'
  '     forked true true, followed true'
)
for step in "${steps[@]}"; do
  grep -qxF "$step" "$dir/log" || { grep -E '^(FAIL|     )' "$dir/log"; echo "missing: $step"; exit 1; }
done
grep -q '^PASS linux smoke' "$dir/log" || { grep '^FAIL' "$dir/log"; echo "no PASS line"; exit 1; }
[ $code -eq 0 ] || { echo "the smoke exited $code"; exit 1; }
echo ok
