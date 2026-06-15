# Shared release helpers used by CI packaging and local post-release publishing.

package-artifacts:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="${GITHUB_REF_NAME#v}"
    cd artifacts

    package_tar() {
        local source_dir="$1"
        local archive_name="$2"

        chmod +x "${source_dir}/spot-defy"
        tar -czvf "${archive_name}" \
            -C "${source_dir}" spot-defy \
            -C "${GITHUB_WORKSPACE}" README.md LICENSE \
            -C "${GITHUB_WORKSPACE}" docs/tmux.md
    }

    package_tar spot-defy-macos-arm64 "spot-defy_${VERSION}_Darwin_arm64.tar.gz"
    package_tar spot-defy-macos-x86_64 "spot-defy_${VERSION}_Darwin_x86_64.tar.gz"

generate-checksums:
    #!/usr/bin/env bash
    set -euo pipefail
    cd artifacts
    shasum -a 256 *.tar.gz > checksums.txt

download-checksums version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    WORKDIR="dist/release-sync/${VERSION}"
    mkdir -p "${WORKDIR}"
    gh release download "v${VERSION}" \
        --pattern "checksums.txt" \
        --clobber \
        --dir "${WORKDIR}"

publish-cask-local version tap_dir="../homebrew-tap" push="false":
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{ version }}"
    TAP_DIR="{{ tap_dir }}"
    PUSH="{{ push }}"
    WORKDIR="dist/release-sync/${VERSION}"
    CHECKSUMS_FILE="${WORKDIR}/checksums.txt"

    : "${VERSION:?VERSION is required}"
    [ -f "${CHECKSUMS_FILE}" ] || { echo "Missing ${CHECKSUMS_FILE}. Run download-checksums ${VERSION} first."; exit 1; }
    [ -d "${TAP_DIR}" ] || { echo "Missing tap directory: ${TAP_DIR}"; exit 1; }
    mkdir -p "${TAP_DIR}/Casks"

    lookup_hash() {
        awk -v a="$1" 'substr($2, length($2)-length(a)+1) == a { print $1; exit }' "${CHECKSUMS_FILE}"
    }

    required_hash_for() {
        local hash
        hash=$(lookup_hash "$1")
        [[ -n "${hash}" ]] || {
            echo "Missing checksum for $1 in ${CHECKSUMS_FILE}" >&2
            exit 1
        }
        printf '%s' "${hash}"
    }

    DARWIN_ARM64_SHA=$(required_hash_for "Darwin_arm64.tar.gz")
    DARWIN_X86_64_SHA=$(required_hash_for "Darwin_x86_64.tar.gz")

    CASK_FILE="${WORKDIR}/spot-defy.rb"
    cat > "${CASK_FILE}" << CASK
    # This file is auto-generated. DO NOT EDIT.
    cask "spot-defy" do
      arch arm: "arm64", intel: "x86_64"
      os macos: "Darwin"

      version "${VERSION}"
      sha256 arm:   "${DARWIN_ARM64_SHA}",
             intel: "${DARWIN_X86_64_SHA}"

      url "https://github.com/blacktop/spot-defy/releases/download/v#{version}/spot-defy_#{version}_#{os}_#{arch}.tar.gz"
      name "spot-defy"
      desc "Privacy-first Spotify player TUI with embedded streaming"
      homepage "https://github.com/blacktop/spot-defy"

      binary "spot-defy"

      on_macos do
        postflight do
          Dir.glob("#{staged_path}/**/spot-defy").each do |f|
            system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", f]
          end
        end
      end

      caveats do
        <<~EOS
          spot-defy requires a Spotify Premium account.
          Run spot-defy auth login once before launching the TUI.
        EOS
      end
    end
    CASK

    cp "${CASK_FILE}" "${TAP_DIR}/Casks/spot-defy.rb"

    cd "${TAP_DIR}"
    git add Casks/spot-defy.rb
    if git diff --cached --quiet; then
        echo "No cask changes to commit."
        exit 0
    fi

    git commit -m "Update spot-defy to ${VERSION}"

    if [[ "${PUSH}" == "true" ]]; then
        git push
    else
        echo "Committed cask update locally. Run 'git -C ${TAP_DIR} push' to publish it."
    fi
