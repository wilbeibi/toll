check:
    cargo fmt --all --check
    cargo clippy --all-targets -- -D warnings
    cargo test

smoke:
    target/release/toll config --format json
    target/release/toll stats
    target/release/toll tail -n 20

restart:
    systemctl --user restart toll.service
    systemctl --user status toll.service --no-pager --lines=20

deploy:
    cargo install --path . --root ~/.local
    systemctl --user restart toll.service
    systemctl --user status toll.service --no-pager --lines=20
