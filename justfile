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

# The service runs %h/.cargo/bin/turnpike (both hosts), so install must land
# there — a --root elsewhere builds a binary systemd never executes.
deploy:
    cargo install --path .
    systemctl --user restart turnpike.service
    systemctl --user status turnpike.service --no-pager --lines=20
