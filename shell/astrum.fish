# Astrum shell integration for fish.

status is-interactive; or exit 0
set -q _ASTRUM_INTEGRATED; and exit 0
set -g _ASTRUM_INTEGRATED 1
set -g _astrum_k "$ASTRUM_NONCE"
set -e ASTRUM_NONCE

function _astrum_preexec --on-event fish_preexec
    printf '\e]133;C;k=%s;cmd=%s\a' "$_astrum_k" (string replace -ra '[[:cntrl:]]' ' ' -- "$argv[1]")
end

function _astrum_postexec --on-event fish_postexec
    printf '\e]133;D;k=%s;%s\a' "$_astrum_k" $status
end

function _astrum_prompt --on-event fish_prompt
    printf '\e]7;file://%s%s\a' (hostname) (string replace -a '%' '%25' -- "$PWD")
    printf '\e]133;A;k=%s\a' "$_astrum_k"
end
