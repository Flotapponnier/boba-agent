#!/usr/bin/env bash
# Fake `claude -p --input-format stream-json --output-format stream-json` for process tests.
# Behaviour is chosen by FAKE_MODE; FAKE_OUT is a directory for evidence files.
set -u
SID=""
MCP=""
prev=""
for a in "$@"; do
  case "$prev" in
    --session-id|--resume) SID="$a" ;;
    --mcp-config) MCP="$a" ;;
  esac
  prev="$a"
done
out="${FAKE_OUT:-/tmp}"
printf '%s\n' "$@" > "$out/args.txt"
env > "$out/env.txt"
pwd > "$out/cwd.txt"
if [ -n "$MCP" ]; then
  cp "$MCP" "$out/mcp.json"
  ls -l "$MCP" | cut -c1-10 > "$out/mcp.perms"
fi

say() { printf '%s\n' "$1"; }
read -r first || exit 2
say '{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"fake-model","skills":["boba:demo"],"mcp_servers":[{"name":"broken","status":"failed"}]}'
say '{"type":"user","message":{"role":"user","content":"prompt"},"isReplay":true}'

result() { # $1 = text (already JSON-escaped)
  say '{"type":"result","subtype":"success","is_error":false,"result":"'"$1"'","total_cost_usd":0.0123,"num_turns":2,"usage":{"input_tokens":100,"cache_read_input_tokens":900,"output_tokens":50}}'
}
wait_eof() { while read -r _; do :; done; }

case "${FAKE_MODE:-work}" in
  work)
    say '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Writing the feature."},{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"'"$PWD"'/feature.txt","content":"x"}}],"usage":{"input_tokens":10,"output_tokens":5}}}'
    echo "feature" > feature.txt
    say '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"File created"}]}}'
    result "Implemented the feature."
    wait_eof ;;
  noop)
    result "Nothing to do."
    wait_eof ;;
  perm)
    say '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"touch allowed.txt"}}]}}'
    say '{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch allowed.txt"},"tool_use_id":"t1"}}'
    read -r answer
    echo "$answer" > "$out/answer.json"
    case "$answer" in
      *'"allow"'*) touch allowed.txt; say '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}' ;;
      *) say '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"denied","is_error":true}]}}' ;;
    esac
    echo "more" > more.txt
    result "Done."
    wait_eof ;;
  message)
    read -r msg
    say '{"type":"user","message":{"role":"user","content":"msg"},"isReplay":true}'
    echo "$msg" > "$out/message.json"
    echo "x" > from-message.txt
    result "Handled the follow-up."
    wait_eof ;;
  review_ok)
    result 'Looks good.\n\n```boba-verdict\n{\"verdict\": \"changes_requested\", \"feedback\": \"Add a test for x.\"}\n```'
    wait_eof ;;
  review_bad)
    result "Looks good to me."
    wait_eof ;;
  command)
    result 'Sure.\n\n```boba-plan\n{\"reply\": \"Sure.\", \"actions\": [{\"label\": \"Run job\", \"action\": {\"action\": \"run_job\", \"job_id\": \"00000000-0000-0000-0000-0000000000d1\"}}, {\"label\": \"bogus\", \"action\": {\"action\": \"explode\"}}], \"question\": null}\n```'
    wait_eof ;;
  report)
    result "## Report\\n- 3 findings"
    wait_eof ;;
  hang)
    sleep 300 &
    echo $! > "$out/child.pid"
    sleep 300 ;;
  crash)
    echo "boom: something broke" >&2
    exit 3 ;;
  error)
    say '{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login"}'
    exit 1 ;;
esac
