# Keep the C compiler available to scripts and build tools.
function cc --description 'CC coding agent in interactive shells'
    if status is-interactive
        set -l cc_data "$HOME/.local/share"
        if set -q XDG_DATA_HOME; and test -n "$XDG_DATA_HOME"
            set cc_data "$XDG_DATA_HOME"
        end
        command "$cc_data/codex-plus/bin/cc" $argv
    else
        command cc $argv
    end
end
