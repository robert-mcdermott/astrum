# bash completion for astrum. Install with:
#   astrum completions bash > ~/.local/share/bash-completion/completions/astrum

__astrum_sessions() {
    astrum ls 2>/dev/null | awk '$3 ~ /^tabs?$/ { print $1 }'
}

_astrum() {
    local cur prev cmd sub i
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    COMPREPLY=()

    case "$prev" in
        -s|--session)
            COMPREPLY=( $(compgen -W "$(__astrum_sessions)" -- "$cur") )
            return
            ;;
    esac

    # The command typed so far, and the word after it.
    cmd=""
    sub=""
    for ((i = 1; i < COMP_CWORD; i++)); do
        case "${COMP_WORDS[i]}" in
            -s|--session) ((i++)) ;;
            -*) ;;
            *)
                if [ -z "$cmd" ]; then cmd="${COMP_WORDS[i]}"; else sub="${COMP_WORDS[i]}"; break; fi
                ;;
        esac
    done

    case "$cmd" in
        "")
            COMPREPLY=( $(compgen -W "attach ls kill config themes keys reload service completions man help -s --session -h --help -V --version" -- "$cur") )
            ;;
        kill)
            COMPREPLY=( $(compgen -W "--all --forget -s --session" -- "$cur") )
            ;;
        config)
            COMPREPLY=( $(compgen -W "--init" -- "$cur") )
            ;;
        service)
            if [ -z "$sub" ]; then
                COMPREPLY=( $(compgen -W "install uninstall status" -- "$cur") )
            elif [ "$sub" = install ]; then
                COMPREPLY=( $(compgen -W "--print -s --session" -- "$cur") )
            else
                COMPREPLY=( $(compgen -W "-s --session" -- "$cur") )
            fi
            ;;
        completions)
            [ -z "$sub" ] && COMPREPLY=( $(compgen -W "bash zsh fish" -- "$cur") )
            ;;
        attach|reload)
            COMPREPLY=( $(compgen -W "-s --session" -- "$cur") )
            ;;
    esac
}

complete -F _astrum astrum
