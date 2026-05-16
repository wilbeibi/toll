deploy:
    cargo build --release
    cp target/release/toll ~/.local/bin/toll.new && mv ~/.local/bin/toll.new ~/.local/bin/toll
    systemctl --user restart toll.service
    systemctl --user status toll.service --no-pager --lines=20
