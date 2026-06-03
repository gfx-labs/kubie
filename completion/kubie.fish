set -l commands ctx ns exec export edit edit-config info lint delete update generate-completion

complete -c kubie --no-files

# Subcommands.
complete -c kubie -n "not __fish_seen_subcommand_from $commands" -a "$commands"

# ctx: complete with context names.
complete -c kubie -n "__fish_seen_subcommand_from ctx; and not __fish_seen_subcommand_from (kubie ctx 2>/dev/null)" \
    -a '(kubie ctx 2>/dev/null)'
complete -c kubie -n "__fish_seen_subcommand_from ctx" -s n -l namespace -d 'namespace' \
    -xa '(kubie ns 2>/dev/null)'
complete -c kubie -n "__fish_seen_subcommand_from ctx" -s f -l kubeconfig -r -d 'kubeconfig file'
complete -c kubie -n "__fish_seen_subcommand_from ctx" -s r -l recursive -d 'spawn recursive shell'
complete -c kubie -n "__fish_seen_subcommand_from ctx" -l no-sync -d 'skip provider sync'
complete -c kubie -n "__fish_seen_subcommand_from ctx" -l local -d 'local contexts only'
complete -c kubie -n "__fish_seen_subcommand_from ctx" -a '-' -d 'switch to previous context'

# ns: complete with namespace names.
complete -c kubie -n "__fish_seen_subcommand_from ns" -a '(kubie ns 2>/dev/null)'
complete -c kubie -n "__fish_seen_subcommand_from ns" -s r -l recursive -d 'spawn recursive shell'
complete -c kubie -n "__fish_seen_subcommand_from ns" -s u -l unset -d 'unset namespace'
complete -c kubie -n "__fish_seen_subcommand_from ns" -a '-' -d 'switch to previous namespace'

# exec: context then namespace.
complete -c kubie -n "__fish_seen_subcommand_from exec; and __fish_is_nth_token 2" \
    -a '(kubie ctx 2>/dev/null)'
complete -c kubie -n "__fish_seen_subcommand_from exec; and __fish_is_nth_token 3" \
    -a '(kubie ns 2>/dev/null)'
complete -c kubie -n "__fish_seen_subcommand_from exec" -s e -l exit-early -d 'exit on failure'
complete -c kubie -n "__fish_seen_subcommand_from exec" -l no-sync -d 'skip provider sync'
complete -c kubie -n "__fish_seen_subcommand_from exec" -l local -d 'local contexts only'

# export: context then namespace.
complete -c kubie -n "__fish_seen_subcommand_from export; and __fish_is_nth_token 2" \
    -a '(kubie ctx 2>/dev/null)'
complete -c kubie -n "__fish_seen_subcommand_from export; and __fish_is_nth_token 3" \
    -a '(kubie ns 2>/dev/null)'
complete -c kubie -n "__fish_seen_subcommand_from export" -l no-sync -d 'skip provider sync'
complete -c kubie -n "__fish_seen_subcommand_from export" -l local -d 'local contexts only'

# edit/delete: complete with context names.
complete -c kubie -n "__fish_seen_subcommand_from edit delete" -a '(kubie ctx 2>/dev/null)'

# info: subcommands.
complete -c kubie -n "__fish_seen_subcommand_from info" -a "ctx ns depth"

# generate-completion: shell names.
complete -c kubie -n "__fish_seen_subcommand_from generate-completion" -a "bash zsh fish"
