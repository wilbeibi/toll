deploy:
    cargo install --path . --root ~/.local
    systemctl --user restart toll.service
    systemctl --user status toll.service --no-pager --lines=20
