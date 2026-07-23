check:
    cargo fmt --all --check
    cargo clippy --all-targets -- -D warnings
    cargo test

smoke:
    target/release/turnpike config --format json
    target/release/turnpike stats
    target/release/turnpike tail -n 20

restart:
    systemctl --user restart turnpike.service
    systemctl --user status turnpike.service --no-pager --lines=20

deploy:
    cargo install --path . --root ~/.local
    systemctl --user restart turnpike.service
    systemctl --user status turnpike.service --no-pager --lines=20
