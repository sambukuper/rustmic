# Installs the latest rustmic release for the current user on Linux.
set -e

repo=sambukuper/rustmic
url=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | grep browser_download_url | grep linux-x64 | cut -d '"' -f 4 | head -n 1)
if [ -z "$url" ]; then
    echo "no linux asset found in the latest release" >&2
    exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading ${url##*/}..."
curl -fsSL "$url" | tar -xz -C "$tmp"

mkdir -p "$HOME/.local/bin"
install -m 0755 "$tmp/rustmic" "$HOME/.local/bin/rustmic"
[ -f "$tmp/rustmic-gui" ] && install -m 0755 "$tmp/rustmic-gui" "$HOME/.local/bin/rustmic-gui"

echo ""
echo "rustmic installed to $HOME/.local/bin"
echo "Make sure \$HOME/.local/bin is in your PATH, then: rustmic virtual-mic --help"
