#!/bin/sh
# A stand-in MCP server for the tests, over its input and output: one message
# per line in, a canned answer out. Plain sh and sed, so it runs wherever the
# tests do. Each line received is added to the file $MCP_LOG, if it is set.
#
#   MCP_NAME      what it calls itself (stand-in)
#   MCP_BROKEN=1  prints something that is not the protocol, and ends
#   MCP_TOKEN     the token it expects in $MCP_TOKEN_GIVEN, if set
log=${MCP_LOG:-/dev/null}
name=${MCP_NAME:-stand-in}
echo "$name: starting" >&2
if [ "${MCP_BROKEN:-}" = 1 ]; then
  echo "this is not the protocol"
  echo "$name: no configuration found" >&2
  exit 3
fi
if [ -n "${MCP_TOKEN:-}" ] && [ "${MCP_TOKEN_GIVEN:-}" != "$MCP_TOKEN" ]; then
  echo "$name: the token is missing or wrong" >&2
  exit 4
fi
grown="$log.grown"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/^{"id":\([0-9][0-9]*\),.*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"%s","version":"1.0"},"instructions":"Stand-in instructions: be brief."}}\n' "$id" "$name" ;;
    *'"method":"tools/list"'*)
      extra=
      if [ -e "$grown" ]; then extra=',{"name":"later","description":"Came later.","inputSchema":{"type":"object"}}'; fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Say the words back.","inputSchema":{"type":"object","properties":{"words":{"type":"string"}},"required":["words"]}},{"name":"boom","description":"Always fails.","inputSchema":{"type":"object"}},{"name":"slow","description":"Takes three seconds.","inputSchema":{"type":"object"}},{"name":"grow","description":"Adds a tool.","inputSchema":{"type":"object"}},{"name":"quit","description":"Ends the server.","inputSchema":{"type":"object"}}%s]}}\n' "$id" "$extra" ;;
    *'"method":"tools/call"'*'"name":"echo"'*)
      words=$(printf '%s' "$line" | sed -n 's/.*"words":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo: %s"}]}}\n' "$id" "$words" ;;
    *'"method":"tools/call"'*'"name":"slow"'*)
      sleep 3
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"slow: done"}]}}\n' "$id" ;;
    *'"method":"tools/call"'*'"name":"grow"'*)
      touch "$grown"
      printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"grown"}]}}\n' "$id" ;;
    *'"method":"tools/call"'*'"name":"quit"'*)
      echo "$name: asked to quit" >&2
      exit 0 ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"it went wrong"}],"isError":true}}\n' "$id" ;;
    '{"id":'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"no such method"}}\n' "$id" ;;
  esac
done
echo "$name: input closed" >&2
