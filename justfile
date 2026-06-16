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
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    CARGO_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    if [[ "$VERSION" != "$CARGO_VERSION" ]]; then
        echo "Error: requested tag v${VERSION}, but Cargo.toml version is ${CARGO_VERSION}." >&2
        echo "Bump Cargo.toml/Cargo.lock or run: just tag ${CARGO_VERSION}" >&2
        exit 1
    fi
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "Error: worktree has uncommitted changes; commit or stash before tagging." >&2
        exit 1
    fi
    if git rev-parse -q --verify "refs/tags/v${VERSION}" >/dev/null; then
        echo "Error: local tag v${VERSION} already exists." >&2
        exit 1
    fi
    if git ls-remote --exit-code --tags origin "refs/tags/v${VERSION}" >/dev/null 2>&1; then
        echo "Error: remote tag v${VERSION} already exists." >&2
        exit 1
    fi
    echo "Creating tag v${VERSION}..."
    git tag -a "v${VERSION}" -m "Release v${VERSION}"
    git push origin "v${VERSION}"
    echo "Tag v${VERSION} pushed. GitHub Actions will handle the release."

# Bump Cargo.toml/Cargo.lock, commit, tag, and push.
bump level="patch":
    #!/usr/bin/env bash
    set -euo pipefail
    LEVEL="{{ level }}"
    case "$LEVEL" in
        patch|minor|major) ;;
        *)
            echo "Error: bump level must be patch, minor, or major." >&2
            exit 1
            ;;
    esac
    command -v svu >/dev/null || {
        echo "Error: svu is required. Install it first." >&2
        exit 1
    }
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "Error: worktree has uncommitted changes; commit or stash before bumping." >&2
        exit 1
    fi
    TAG="$(svu "$LEVEL")"
    VERSION="${TAG#v}"
    CURRENT="$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')"
    if [[ "$VERSION" == "$CURRENT" ]]; then
        echo "Cargo.toml already at ${VERSION}"
    else
        perl -0pi -e "s/^version = \"\\Q${CURRENT}\\E\"/version = \"${VERSION}\"/m" Cargo.toml
        cargo update -p spot-defy
        git add Cargo.toml Cargo.lock
        git commit -m "chore: release ${VERSION}"
    fi
    git push
    just tag "$VERSION"

# Bump patch version, commit, tag, and push.
bump-patch:
    just bump patch

# Bump minor version, commit, tag, and push.
bump-minor:
    just bump minor

# Bump major version, commit, tag, and push.
bump-major:
    just bump major

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
