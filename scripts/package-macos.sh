#!/bin/bash
set -euo pipefail

usage() {
    cat <<'HELP'
用法: ./scripts/package-macos.sh [选项]
  --target <目标>   aarch64-apple-darwin、x86_64-apple-darwin 或 universal
                   默认使用 Cargo 的本机构建目标
  --output <目录>   输出目录，默认项目内的 dist
  --identity <标识> codesign 签名标识，默认 -（本地临时签名）
  --help           显示帮助

可用 MACOS_SIGNING_IDENTITY 和 MACOSX_DEPLOYMENT_TARGET 设置签名标识及最低系统版本。
HELP
}

fail() { printf '打包失败: %s\n' "$*" >&2; exit 1; }

project_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="$project_dir/dist"
build_target=""
signing_identity="${MACOS_SIGNING_IDENTITY:--}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target|--output|--identity)
            [[ $# -ge 2 && -n "$2" ]] || fail "$1 需要一个值"
            case "$1" in
                --target) build_target="$2" ;;
                --output) output_dir="$2" ;;
                --identity) signing_identity="$2" ;;
            esac
            shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) fail "未知参数 $1；使用 --help 查看帮助" ;;
    esac
done
[[ "$(uname -s)" == Darwin ]] || fail "请在 macOS 上运行此脚本"
case "$build_target" in
    ""|aarch64-apple-darwin|x86_64-apple-darwin|universal) ;;
    *) fail "不支持的 macOS 构建目标: $build_target" ;;
esac
for tool in cargo plutil codesign ditto lipo; do
    command -v "$tool" >/dev/null || fail "未找到 $tool"
done

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
[[ "$MACOSX_DEPLOYMENT_TARGET" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || fail "最低系统版本格式错误"
metadata="$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$project_dir/Cargo.toml")"
version="$(printf '%s' "$metadata" | plutil -extract packages.0.version raw -o - -)"
target_dir="$(printf '%s' "$metadata" | plutil -extract target_directory raw -o - -)"
mkdir -p "$target_dir" "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
stage="$(mktemp -d "$target_dir/macos-package.XXXXXX")"
trap 'rm -rf "$stage"' EXIT

build_binary() {
    local target="$1" log="$stage/build-${1:-native}.json" entry executable=""
    local args=(build --locked --release --bin take-a-panel --manifest-path "$project_dir/Cargo.toml" --message-format=json-render-diagnostics)
    if [[ -n "$target" ]]; then args+=(--target "$target"); fi
    cargo "${args[@]}" >"$log" || fail "Cargo 构建失败: ${target:-native}"
    # Cargo reports the actual binary path, including custom target directories
    # and targets from .cargo/config.toml. Parse only executable artifacts.
    while IFS= read -r entry; do
        case "$entry" in
            *'"executable":null'*) continue ;;
            *'"executable":'*)
                if [[ "$(printf '%s' "$entry" | plutil -extract target.name raw -o - -)" == take-a-panel ]]; then
                    executable="$(printf '%s' "$entry" | plutil -extract executable raw -o - -)"
                fi ;;
        esac
    done <"$log"
    [[ -n "$executable" && -x "$executable" ]] || fail "Cargo 未生成可执行文件"
    printf '%s\n' "$executable"
}

app="$stage/Take a Panel.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
if [[ "$build_target" == universal ]]; then
    arm_binary="$(build_binary aarch64-apple-darwin)"
    intel_binary="$(build_binary x86_64-apple-darwin)"
    lipo -create "$arm_binary" "$intel_binary" -output "$app/Contents/MacOS/take-a-panel"
else
    binary="$(build_binary "$build_target")"
    install -m 755 "$binary" "$app/Contents/MacOS/take-a-panel"
fi
chmod 755 "$app/Contents/MacOS/take-a-panel"
architecture="$(lipo -archs "$app/Contents/MacOS/take-a-panel")"
case "$architecture" in
    arm64|x86_64) ;;
    *arm64*x86_64*|*x86_64*arm64*) architecture=universal ;;
    *) fail "构建产物不是支持的 macOS 架构: $architecture" ;;
esac

cp "$project_dir/packaging/macos/Info.plist" "$app/Contents/Info.plist"
bundle_version="${version%%[-+]*}"
plutil -replace CFBundleShortVersionString -string "$bundle_version" "$app/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$bundle_version" "$app/Contents/Info.plist"
plutil -replace LSMinimumSystemVersion -string "$MACOSX_DEPLOYMENT_TARGET" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
cp "$project_dir/packaging/macos/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
cp "$project_dir/LICENSE" "$app/Contents/Resources/LICENSE"

sign_args=(--force --sign "$signing_identity")
if [[ "$signing_identity" == - ]]; then
    sign_args+=(--timestamp=none)
else
    sign_args+=(--options runtime --timestamp)
fi
codesign "${sign_args[@]}" "$app"
codesign --verify --strict --verbose=2 "$app"

archive="Take-a-Panel-$version-macos-$architecture.zip"
ditto -c -k --sequesterRsrc --keepParent "$app" "$stage/$archive"
# Build and verify everything before replacing any previously generated bundle.
if [[ -e "$output_dir/Take a Panel.app" ]]; then
    mv "$output_dir/Take a Panel.app" "$stage/previous.app"
fi
if ! mv "$app" "$output_dir/Take a Panel.app"; then
    if [[ -e "$stage/previous.app" ]]; then
        mv "$stage/previous.app" "$output_dir/Take a Panel.app"
    fi
    fail "无法写入应用输出目录"
fi
mv -f "$stage/$archive" "$output_dir/$archive"
printf '\n应用: %s\n压缩包: %s\n' "$output_dir/Take a Panel.app" "$output_dir/$archive"
