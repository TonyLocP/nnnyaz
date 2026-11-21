# Bash completion for nnnyaz.
#
# Install: source this file from ~/.bashrc, or copy it to
# /usr/share/bash-completion/completions/nnnyaz.

_nnnyaz() {
    local cur prev opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    case "$prev" in
        --cwd-file|--chooser-file|-p)
            compopt -o filenames 2>/dev/null
            return 0
            ;;
    esac

    opts='-h --help -V --version --generate-config --cwd-file --chooser-file -p -0 --null
          -H --hidden -i --file-info -d --detail -U --owner
          -J --no-auto-advance -b --bookmark -Q --no-confirm-quit
          -o --open-on-enter -n --type-to-nav -A --no-autoenter
          -g --regex -z --fuzzy -e --editor -E --internal-editor
          -c --cli-opener -f --history -K --key-collisions
          -u --use-selection -R --no-rollover -s --session
          -S --persistent-session -T --sort -l --scroll -t --lock-after'

    if [[ "$cur" == -* ]]; then
        COMPREPLY=( $(compgen -W "$opts" -- "$cur") )
    else
        compopt -o filenames 2>/dev/null
        COMPREPLY=( $(compgen -f -- "$cur") )
    fi
}

complete -F _nnnyaz nnnyaz
