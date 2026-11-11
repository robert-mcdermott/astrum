# Astrum shell integration for bash, loaded with --rcfile.
# bash ignores --rcfile for login shells, so the login startup files are
# sourced here by hand before the hooks are installed.

if [ -r /etc/profile ]; then . /etc/profile; fi
for _astrum_f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
  if [ -r "$_astrum_f" ]; then . "$_astrum_f"; break; fi
done
unset _astrum_f

case "$-" in *i*) ;; *) return 0 ;; esac
[ -n "$_ASTRUM_INTEGRATED" ] && return 0
_ASTRUM_INTEGRATED=1
_astrum_executing=""
_astrum_at_prompt=""
_astrum_ret=0
_astrum_k="$ASTRUM_NONCE"
unset ASTRUM_NONCE

_astrum_pc_start() {
  _astrum_ret=$?
  if [ -n "$_astrum_executing" ]; then
    builtin printf '\e]133;D;k=%s;%s\a' "$_astrum_k" "$_astrum_ret"
  fi
  _astrum_executing=""
  return $_astrum_ret
}

_astrum_pc_end() {
  builtin printf '\e]7;file://%s%s\a' "$HOSTNAME" "${PWD//%/%25}"
  builtin printf '\e]133;A;k=%s\a' "$_astrum_k"
  _astrum_at_prompt=1
}

_astrum_preexec() {
  [ -n "$COMP_LINE" ] && return
  # The prompt command starting again with no command in between (Enter on
  # an empty line, an interrupted line): nothing was run, so nothing starts.
  if [ "$BASH_COMMAND" = "_astrum_pc_start" ]; then
    _astrum_at_prompt=""
    return
  fi
  [ -z "$_astrum_at_prompt" ] && return
  _astrum_at_prompt=""
  _astrum_executing=1
  local cmd
  cmd=$(HISTTIMEFORMAT='' builtin history 1)
  if [[ "$cmd" =~ ^[[:space:]]*[0-9]+[[:space:]]*(.*)$ ]]; then cmd="${BASH_REMATCH[1]}"; fi
  builtin printf '\e]133;C;k=%s;cmd=%s\a' "$_astrum_k" "${cmd//[[:cntrl:]]/ }"
}

trap '_astrum_preexec' DEBUG
PROMPT_COMMAND="_astrum_pc_start${PROMPT_COMMAND:+; $PROMPT_COMMAND}; _astrum_pc_end"
