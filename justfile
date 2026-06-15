set shell := ["bash", "-cu"]

# Show available recipes
default:
    @just --list

# Build debug binary
build:
    cargo build

# Build release binary
release:
    cargo build --release --locked

# Format code
fmt:
    cargo fmt --all

# Run clippy linter
lint:
    cargo clippy --all --benches --tests --examples --all-features -- -D warnings

# Run unit and integration tests
test:
    RUST_BACKTRACE=1 cargo test --all-features --locked

# Run dependency policy checks
deny:
    cargo deny check

# Run full local CI
check: fmt lint test deny

# Show current package version
version:
    @grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/'

# Create and push an annotated release tag. GitHub Actions builds the release.
tag version:
    @echo "Creating tag v{{ version }}..."
    git tag -a "v{{ version }}" -m "Release v{{ version }}"
    git push origin "v{{ version }}"
    @echo "Tag v{{ version }} pushed. GitHub Actions will handle the release."

# CI helper wrappers. Implementation lives in ci.just so shell logic stays centralized.
ci-package-artifacts:
    just --justfile "{{ justfile_directory() }}/ci.just" package-artifacts

ci-generate-checksums:
    just --justfile "{{ justfile_directory() }}/ci.just" generate-checksums

_release-banner message:
    #!/usr/bin/env bash
    set -euo pipefail
    printf '\n==> %s\n' '{{ message }}'

# Download checksums from a completed GitHub release.
release-fetch-checksums version="":
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    if [[ -z "$VERSION" ]]; then
        VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    fi
    just _release-banner "Fetching release checksums for ${VERSION}"
    just --justfile "{{ justfile_directory() }}/ci.just" download-checksums "$VERSION"

# Update ../homebrew-tap/Casks/spot-defy.rb from a completed GitHub release.
tap-update version="" tap_dir="../homebrew-tap":
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    if [[ -z "$VERSION" ]]; then
        VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    fi
    just release-fetch-checksums "$VERSION"
    just _release-banner "Updating Homebrew cask for ${VERSION}"
    just --justfile "{{ justfile_directory() }}/ci.just" publish-cask-local "$VERSION" "{{ tap_dir }}" false

# Update ../homebrew-tap/Casks/spot-defy.rb and push the tap commit.
tap-update-push version="" tap_dir="../homebrew-tap":
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    if [[ -z "$VERSION" ]]; then
        VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    fi
    just release-fetch-checksums "$VERSION"
    just _release-banner "Updating and pushing Homebrew cask for ${VERSION}"
    just --justfile "{{ justfile_directory() }}/ci.just" publish-cask-local "$VERSION" "{{ tap_dir }}" true

# Build, tag, release in CI, then update the tap after the GitHub release exists.
release-sync version="" tap_dir="../homebrew-tap":
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    if [[ -z "$VERSION" ]]; then
        VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    fi
    just check
    just tag "$VERSION"
    echo "Wait for the GitHub release workflow to finish, then run:"
    echo "  just tap-update $VERSION {{ tap_dir }}"
