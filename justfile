default: check

check: fmt test clippy

fmt:
    cargo fmt --all -- --check

test:
    cargo test --locked --all-targets

clippy:
    if command -v distill >/dev/null 2>&1; then distill cargo clippy -- --locked --all-targets -- -D warnings; else cargo clippy --locked --all-targets -- -D warnings; fi

build:
    if command -v distill >/dev/null 2>&1; then distill cargo build -- --locked; else cargo build --locked; fi

package:
    cargo package --locked --allow-dirty