# Fish wrapper for nnnyaz with cd-on-quit.
#
# Install:
#     cp wrappers/nnnyaz.fish ~/.config/fish/functions/nnnyaz.fish
#
# This also defines `f` as a short alias. Keep `y` for yazi.

function nnnyaz --description 'nnnyaz with cd-on-quit' --wraps=nnnyaz
    set -l cwd_file (mktemp)
    or return

    command nnnyaz --cwd-file $cwd_file $argv
    set -l exit_status $status

    if test -s $cwd_file
        cd (cat $cwd_file)
    end
    rm -f $cwd_file
    return $exit_status
end

function f --description 'nnnyaz with cd-on-quit' --wraps=nnnyaz
    nnnyaz $argv
end
