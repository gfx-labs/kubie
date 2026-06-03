#compdef kubie

_kubie_contexts() {
    local -a contexts
    contexts=(${(f)"$(kubie ctx 2>/dev/null)"})
    _describe 'context' contexts
}

_kubie_namespaces() {
    local -a namespaces
    namespaces=(${(f)"$(kubie ns 2>/dev/null)"})
    _describe 'namespace' namespaces
}

_kubie() {
    local -a commands
    commands=(
        'ctx:spawn a shell in a context'
        'ns:switch namespace'
        'exec:execute a command in a context'
        'export:export kubeconfig path'
        'edit:edit a context'
        'edit-config:edit kubie config'
        'info:show current context/namespace/depth'
        'lint:check kubeconfig files for issues'
        'delete:delete a context'
        'update:check for updates'
        'generate-completion:generate shell completions'
    )

    _arguments -C \
        '1:command:->command' \
        '*::arg:->args'

    case "$state" in
        command)
            _describe 'command' commands
            ;;
        args)
            case "$words[1]" in
                ctx)
                    _arguments \
                        '-n[namespace]:namespace:_kubie_namespaces' \
                        '--namespace[namespace]:namespace:_kubie_namespaces' \
                        '-f[kubeconfig file]:file:_files' \
                        '--kubeconfig[kubeconfig file]:file:_files' \
                        '-r[recursive]' \
                        '--recursive[recursive]' \
                        '--no-sync[skip provider sync]' \
                        '--local[local contexts only]' \
                        '1:context:_kubie_contexts'
                    ;;
                ns)
                    _arguments \
                        '-r[recursive]' \
                        '--recursive[recursive]' \
                        '-u[unset namespace]' \
                        '--unset[unset namespace]' \
                        '1:namespace:_kubie_namespaces'
                    ;;
                exec)
                    _arguments \
                        '-e[exit early on failure]' \
                        '--exit-early[exit early on failure]' \
                        '--context-headers[print context headers]' \
                        '--no-sync[skip provider sync]' \
                        '--local[local contexts only]' \
                        '1:context:_kubie_contexts' \
                        '2:namespace:_kubie_namespaces' \
                        '*:command:_command_names'
                    ;;
                export)
                    _arguments \
                        '--no-sync[skip provider sync]' \
                        '--local[local contexts only]' \
                        '1:context:_kubie_contexts' \
                        '2:namespace:_kubie_namespaces'
                    ;;
                edit)
                    _arguments '1:context:_kubie_contexts'
                    ;;
                delete)
                    _arguments '1:context:_kubie_contexts'
                    ;;
                info)
                    _arguments '1:kind:(ctx ns depth)'
                    ;;
                generate-completion)
                    _arguments '1:shell:(bash zsh fish)'
                    ;;
            esac
            ;;
    esac
}

_kubie "$@"
