_kubie() {
    local cur prev
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    case "${COMP_CWORD}" in
        1)
            COMPREPLY=($(compgen -W "ctx ns exec export edit edit-config info lint delete update generate-completion" -- "$cur"))
            ;;
        2)
            case "${prev}" in
                ctx|edit|delete)
                    COMPREPLY=($(compgen -W "$(kubie ctx 2>/dev/null)" -- "$cur"))
                    ;;
                ns)
                    COMPREPLY=($(compgen -W "$(kubie ns 2>/dev/null)" -- "$cur"))
                    ;;
                exec|export)
                    COMPREPLY=($(compgen -W "$(kubie ctx 2>/dev/null)" -- "$cur"))
                    ;;
                info)
                    COMPREPLY=($(compgen -W "ctx ns depth" -- "$cur"))
                    ;;
                generate-completion)
                    COMPREPLY=($(compgen -W "bash zsh fish" -- "$cur"))
                    ;;
            esac
            ;;
        3)
            local subcmd="${COMP_WORDS[1]}"
            case "${subcmd}" in
                exec|export)
                    COMPREPLY=($(compgen -W "$(kubie ns 2>/dev/null)" -- "$cur"))
                    ;;
            esac
            ;;
    esac
}

complete -F _kubie kubie
