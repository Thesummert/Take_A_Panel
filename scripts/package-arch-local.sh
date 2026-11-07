#!/usr/bin/env bash
set -euo pipefail

# Build the current working tree, including uncommitted changes, without Git
# fetches or Cargo network access. Existing makepkg directories are untouched.
project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="${1:-$project_dir/dist}"
mkdir -p "$output_dir" "$project_dir/target"
output_dir="$(cd "$output_dir" && pwd)"
stage="$(mktemp -d "${TMPDIR:-/tmp}/take-a-panel-package.XXXXXXXX")"
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/workspace" "$stage/package"
cp -a "$project_dir/Cargo.toml" "$project_dir/Cargo.lock" "$project_dir/build.rs" \
    "$project_dir/src" "$project_dir/ui" "$stage/workspace/"
cp "$project_dir/packaging/arch/take-a-panel.desktop" "$stage/package/"
export TAKE_A_PANEL_PACKAGE_SOURCE="$stage/workspace"
export CARGO_TARGET_DIR="$project_dir/target"
export CARGO_NET_OFFLINE=true
export PKGDEST="$output_dir"
cat > "$stage/package/PKGBUILD" <<'PKGBUILD'
pkgname=take-a-panel-local
pkgver=0.1.0
pkgrel=1
pkgdesc='A cross-platform application launcher built with Rust and Slint (local build)'
arch=('x86_64' 'aarch64')
url='https://github.com/Thesummert/Take_A_Panel'
license=('GPL-3.0-only')
options=('!debug')
depends=('fontconfig' 'gcc-libs' 'glib2' 'glibc' 'libx11' 'libxcb' 'libxkbcommon' 'libxkbcommon-x11' 'wayland' 'xdg-utils')
makedepends=('cargo' 'pkgconf')
optdepends=('xdg-desktop-portal: native file and directory dialogs'
            'kdialog: fallback file and directory dialogs'
            'zenity: fallback file and directory dialogs'
            'yad: fallback file and directory dialogs')
provides=('take-a-panel')
conflicts=('take-a-panel' 'take-a-panel-git')
source=('take-a-panel.desktop')
sha256sums=('SKIP')

pkgver() {
  sed -n 's/^version = "\([^"]*\)"/\1/p' "$TAKE_A_PANEL_PACKAGE_SOURCE/Cargo.toml" | head -n 1
}

build() {
  cd "$TAKE_A_PANEL_PACKAGE_SOURCE"
  cargo build --offline --locked --release --bin take-a-panel
}

package() {
  install -Dm755 "$CARGO_TARGET_DIR/release/take-a-panel" "$pkgdir/usr/bin/take-a-panel"
  install -Dm644 "$srcdir/take-a-panel.desktop" "$pkgdir/usr/share/applications/take-a-panel.desktop"
  install -Dm644 "$TAKE_A_PANEL_PACKAGE_SOURCE/ui/assets/tray-p.svg" "$pkgdir/usr/share/icons/hicolor/scalable/apps/take-a-panel.svg"
}
PKGBUILD
cd "$stage/package"
makepkg --noconfirm --force
printf '\n本地软件包已生成在 %s\n' "$output_dir"
