#!/usr/bin/env bash
# Install the latest published release's editor/server binaries, without addons.
set -euo pipefail

die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

if [[ $# -gt 0 ]]; then
    if [[ $# -eq 1 && ( $1 == --help || $1 == -h ) ]]; then
        printf '%s\n' \
            'Install the latest stable Focus Editor release for this Linux architecture.' \
            'Destination: $HOME/local/share/focus-editor/bin (no leading dot).' \
            'Installs focus-editor and focus-editor-server only; addons are untouched.' \
            'Requires: bash, curl, jq, tar, sha256sum, and standard Linux utilities.'
        exit 0
    fi
    die 'Usage: install-release.sh [--help]'
fi

[[ $(uname -s) == Linux ]] || die 'Release binary installation currently supports Linux only.'
case "$(uname -m)" in
    x86_64 | amd64) platform=linux-amd64 ;;
    aarch64 | arm64) platform=linux-arm64 ;;
    *) die "Unsupported architecture: $(uname -m). Available: amd64 and arm64." ;;
esac

for command in curl jq tar sha256sum install mktemp awk tr mv mkdir rm; do
    command -v "$command" >/dev/null 2>&1 || die "Required command not found: $command"
done
: "${HOME:?HOME must be set}"
destination="$HOME/local/share/focus-editor/bin"
repository='https://github.com/nfranchet/NewCode'
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/focus-editor-release.XXXXXX")
install_stage=''
cleanup() {
    rm -rf -- "$work_dir"
    if [[ -n $install_stage ]]; then rm -rf -- "$install_stage"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

download() {
    curl --fail --silent --show-error --location \
        --proto '=https' --proto-redir '=https' \
        --retry 3 --connect-timeout 15 --output "$2" "$1"
}

printf 'Finding the latest release for %s…\n' "$platform"
download 'https://api.github.com/repos/nfranchet/NewCode/releases/latest' "$work_dir/release.json"
tag=$(jq -er '.tag_name | select(type == "string" and length > 0)' "$work_dir/release.json") \
    || die 'GitHub did not return a valid latest release.'
safe_tag=$(printf '%s' "$tag" | LC_ALL=C tr -c 'A-Za-z0-9._-' '-')
archive="focus-editor-${safe_tag}-${platform}.tar.gz"
asset_url() {
    jq -er --arg name "$1" \
        '[.assets[] | select(.name == $name)] | select(length == 1) | .[0].browser_download_url | select(type == "string" and length > 0)' \
        "$work_dir/release.json"
}
archive_url=$(asset_url "$archive") \
    || die "Latest release $tag has no $platform binary archive. Its release build may still be running: $repository/releases/tag/$tag"
checksums_url=$(asset_url SHA256SUMS) \
    || die "Latest release $tag has no SHA256SUMS. Wait for its release build to finish."

printf 'Downloading %s…\n' "$archive"
download "$archive_url" "$work_dir/$archive"
download "$checksums_url" "$work_dir/SHA256SUMS"
awk -v name="$archive" '
    $2 == name || $2 == "*" name { print; count++ }
    END { if (count != 1) exit 1 }
' "$work_dir/SHA256SUMS" > "$work_dir/selected-checksum" \
    || die "SHA256SUMS must contain exactly one checksum for $archive."
(cd "$work_dir" && sha256sum --check --status selected-checksum) \
    || die 'Archive checksum verification failed; existing binaries were not changed.'

mkdir "$work_dir/unpacked"
tar --extract --gzip --file "$work_dir/$archive" --directory "$work_dir/unpacked" \
    --no-same-owner --no-same-permissions focus-editor focus-editor-server \
    || die 'Release archive does not contain both required binaries.'
for binary in focus-editor focus-editor-server; do
    [[ -f "$work_dir/unpacked/$binary" && ! -L "$work_dir/unpacked/$binary" ]] \
        || die "Release archive does not contain a regular file for $binary."
done

mkdir -p -- "$destination"
install_stage=$(mktemp -d "$destination/.install.XXXXXX")
for binary in focus-editor focus-editor-server; do
    install -m 755 -- "$work_dir/unpacked/$binary" "$install_stage/$binary"
done
for binary in focus-editor focus-editor-server; do
    # Rename on the destination filesystem, allowing updates to running binaries.
    mv -fT -- "$install_stage/$binary" "$destination/$binary"
done

printf 'Installed %s to %s\n' "$tag" "$destination"
printf 'Add this directory to PATH if needed:\n  export PATH="$HOME/local/share/focus-editor/bin:$PATH"\n'
