mkdir -p ~/.config/fish/functions
echo 'function circolate --description "Run the circolate dev build"
    set -l bin /run/media/cashmere/Creationism/Circolate/target/release/circolate
    if not test -x $bin
        echo "circolate: not built. Run: cd (dirname $bin)/../.. && cargo build --release" >&2
        return 127
    end
    command $bin $argv
end' > ~/.config/fish/functions/circolate.fish
