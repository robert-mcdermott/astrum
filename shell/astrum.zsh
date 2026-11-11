# Astrum shell integration for zsh.
# Marks prompts and command boundaries (OSC 133) and reports the working
# directory (OSC 7) so Astrum can follow what the shell is doing.

[[ -o interactive ]] || return 0
[[ -n "$_ASTRUM_INTEGRATED" ]] && return 0
typeset -g _ASTRUM_INTEGRATED=1
typeset -g _astrum_executing=""
# Keep the mark secret out of the environment of child processes.
typeset -g _astrum_k="$ASTRUM_NONCE"
unset ASTRUM_NONCE

_astrum_precmd() {
  local ret=$?
  if [[ -n "$_astrum_executing" ]]; then
    builtin printf '\e]133;D;k=%s;%s\a' "$_astrum_k" "$ret"
  fi
  _astrum_executing=""
  builtin printf '\e]7;file://%s%s\a' "${HOST}" "${PWD//\%/%25}"
  builtin printf '\e]133;A;k=%s\a' "$_astrum_k"
}

_astrum_preexec() {
  _astrum_executing=1
  builtin printf '\e]133;C;k=%s;cmd=%s\a' "$_astrum_k" "${1//[[:cntrl:]]/ }"
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _astrum_precmd
add-zsh-hook preexec _astrum_preexec
