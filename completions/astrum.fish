# fish completion for astrum. Install with:
#   astrum completions fish > ~/.config/fish/completions/astrum.fish

function __astrum_sessions
    astrum ls 2>/dev/null | awk '$3 ~ /^tabs?$/ { print $1 }'
end

complete -c astrum -f
complete -c astrum -s s -l session -x -a '(__astrum_sessions)' -d 'The session to act on'
complete -c astrum -s h -l help -d 'Show help'
complete -c astrum -s V -l version -d 'Show the version'

complete -c astrum -n __fish_use_subcommand -a attach -d 'Attach to the session, starting it if needed'
complete -c astrum -n __fish_use_subcommand -a ls -d 'List running sessions, and stopped ones that would come back'
complete -c astrum -n __fish_use_subcommand -a kill -d 'Stop a session and close its shells'
complete -c astrum -n __fish_use_subcommand -a config -d 'Show where the config file is'
complete -c astrum -n __fish_use_subcommand -a themes -d 'List the color themes'
complete -c astrum -n __fish_use_subcommand -a keys -d 'List what the keys do'
complete -c astrum -n __fish_use_subcommand -a reload -d 'Have a running session read its settings again'
complete -c astrum -n __fish_use_subcommand -a service -d 'Run the session as a login service'
complete -c astrum -n __fish_use_subcommand -a completions -d 'Print a completion script for a shell'
complete -c astrum -n __fish_use_subcommand -a man -d 'Print the manual page'
complete -c astrum -n __fish_use_subcommand -a help -d 'Show help'

complete -c astrum -n '__fish_seen_subcommand_from kill' -l all -d 'Stop every session'
complete -c astrum -n '__fish_seen_subcommand_from kill' -l forget -d 'Also discard the saved tabs'
complete -c astrum -n '__fish_seen_subcommand_from config' -l init -d 'Write a commented starter file'
complete -c astrum -n '__fish_seen_subcommand_from service; and not __fish_seen_subcommand_from install uninstall status' -a 'install uninstall status'
complete -c astrum -n '__fish_seen_subcommand_from service; and __fish_seen_subcommand_from install' -l print -d 'Only show what would be written'
complete -c astrum -n '__fish_seen_subcommand_from completions; and not __fish_seen_subcommand_from bash zsh fish' -a 'bash zsh fish'
