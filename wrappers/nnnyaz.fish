# Fish wrapper for nnnyaz with cd-on-quit.
#
# Install:
#     cp wrappers/nnnyaz.fish ~/.config/fish/functions/nnnyaz.fish
#
# This also defines `f` as a short alias. Keep `y` for yazi.

function nnnyaz --description 'nnnyaz with cd-on-quit' --wraps=nnnyaz
    set -l cwd_file (mktemp)
    or return

    # tmux does not forward graphics protocols unless passthrough is enabled.
    # Enable it for this pane only, and restore the previous setting on exit.
    set -l tmux_passthrough 0
    if set -q TMUX
        set -l current (tmux display -p '#{allow-passthrough}' 2>/dev/null)
        if test "$current" = off
            if tmux set -p allow-passthrough on 2>/dev/null
                set tmux_passthrough 1
            end
        end
    end

    command nnnyaz --cwd-file $cwd_file $argv
    set -l exit_status $status

    if test $tmux_passthrough -eq 1
        tmux set -pu allow-passthrough 2>/dev/null
    end

    if test -s $cwd_file
        cd (cat $cwd_file)
    end
    rm -f $cwd_file
    return $exit_status
end

function f --description 'nnnyaz with cd-on-quit' --wraps=nnnyaz
    nnnyaz $argv
end
