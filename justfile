default: check

check: fmt test clippy

@fmt:
    if command -v distill >/dev/null 2>&1 && distill --plan cargo fmt -- --all --check >/dev/null 2>&1; then distill cargo fmt -- --all --check; else cargo fmt --all -- --check; fi

@test:
    if command -v distill >/dev/null 2>&1 && distill --plan cargo test -- --locked --all-targets >/dev/null 2>&1; then distill cargo test -- --locked --all-targets; else cargo test --locked --all-targets; fi

@clippy:
    if command -v distill >/dev/null 2>&1; then distill cargo clippy -- --locked --all-targets -- -D warnings; else cargo clippy --locked --all-targets -- -D warnings; fi

@build:
    if command -v distill >/dev/null 2>&1; then distill cargo build -- --locked; else cargo build --locked; fi

@package:
    if command -v distill >/dev/null 2>&1 && distill --plan cargo package -- --locked --allow-dirty >/dev/null 2>&1; then distill cargo package -- --locked --allow-dirty; else cargo package --locked --allow-dirty; fi

@install:
    if command -v distill >/dev/null 2>&1 && distill --plan cargo install -- --path . --locked >/dev/null 2>&1; then distill cargo install -- --path . --locked; else cargo install --path . --locked; fi