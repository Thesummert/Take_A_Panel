# Take a Panel

一个使用 **Rust + Slint** 编写的跨平台应用启动器。默认暗色、扁平化、圆角无边框界面，按全局快捷键唤出，搜索并启动系统中的应用。

![界面预览（Slint 软件渲染，示例应用数据）](docs/preview.png)

## 运行

安装 Rust 1.92 或更新版本后，在项目目录执行：

```sh
cargo run --release
```

首次运行会显示窗口，之后使用 **Ctrl + Alt + Space** 显示 / 隐藏。macOS 上 Alt 对应 Option，Ctrl 对应 Control。成功启动应用或点击窗口外会隐藏面板，进程继续运行。

```sh
# 自定义全局快捷键
cargo run --release -- --hotkey "Alt+Space"

# 启动后直接在后台等待快捷键
cargo run --release -- --hidden

# 仅扫描、列出应用，可用于排查应用发现问题
cargo run --release -- --list

# 不注册全局快捷键
cargo run --release -- --no-hotkey
```

也可以设置环境变量 `TAKE_A_PANEL_HOTKEY`，命令行 `--hotkey` 的优先级更高。快捷键已占用时会保留窗口并显示错误；可以换一个组合键后重启。当前版本应只运行一个实例。

| 操作 | 功能 |
| --- | --- |
| 输入应用名 / 关键词 | 即时搜索，支持中文、忽略大小写、多词匹配 |
| ↑ / ↓ | 选择应用，列表自动滚动 |
| Enter / 单击应用 | 启动应用 |
| Shift + Enter | 尝试新开应用实例 |
| Esc | 隐藏面板 |
| Ctrl + R / 右下角刷新 | 重新扫描应用 |
| Ctrl + Q / 右上角 × | 退出程序、释放快捷键 |
| 拖动顶部标题区域 | 移动窗口 |

搜索按照名称完全匹配、前缀匹配、包含匹配、关键词匹配排序。应用扫描、图标读取与启动使用后台线程，图标分批更新并缓存在内存中，搜索时复用缓存，空闲时停止轮询。

`Shift + Enter` 在 macOS 上使用 `open -n -a` 请求新实例。Windows、Linux 会再次发起系统启动请求；应用是否允许多个实例或新窗口取决于应用自身。

搜索结果显示应用图标：macOS 使用系统 Workspace 接口，Windows 使用 Shell 图标接口（含 Store 应用），Linux 根据 `.desktop` 的 `Icon` 字段解析绝对路径或图标主题中的 PNG / SVG 等文件。读取失败时回退到名称首字标识。

## 平台支持

| 平台 | 应用发现 | 启动方式 | 全局快捷键 |
| --- | --- | --- | --- |
| macOS | `/Applications`、`/System/Applications`、系统工具目录、`~/Applications` 中的 `.app`；读取 `Info.plist` | 系统 `open -a` | 支持 |
| Windows | 当前用户和全局开始菜单中的 `.lnk`、`.appref-ms`；`Get-StartApps` 补充 Store / UWP 应用 | Windows Shell | 支持 |
| Linux X11 | `$XDG_DATA_HOME/applications`、`$XDG_DATA_DIRS/applications` 中的 `.desktop`，包括标准路径注册的 Flatpak / Snap | `gio launch` | 支持 |
| Linux Wayland | 与 X11 相同 | `gio launch` | 暂未接入 Wayland GlobalShortcuts portal |

Linux 会处理用户条目优先级、`Hidden`、`NoDisplay`、`OnlyShowIn`、`NotShowIn`、`TryExec` 和本地化名称。启动交给 GLib 解析 `.desktop` 的参数、工作目录、终端及 DBus 激活，不经过 shell 拼接。

Wayland、`--no-hotkey` 或快捷键注册失败时，Esc 和启动应用会最小化窗口，点击外部不会自动隐藏，可从任务栏恢复面板。`--hidden` 只有在快捷键成功注册后才生效。

### Linux 系统依赖

Ubuntu / Debian：

```sh
sudo apt install build-essential pkg-config libx11-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libxcb1-dev libwayland-dev libfontconfig1-dev libglib2.0-bin
```

Fedora：

```sh
sudo dnf install gcc-c++ pkgconf-pkg-config libX11-devel libxkbcommon-devel \
  libxkbcommon-x11-devel libxcb-devel wayland-devel fontconfig-devel glib2
```

macOS 需要 Xcode Command Line Tools；Windows 推荐 Rust MSVC 工具链及 Visual Studio C++ Build Tools。Windows 扫描 Store 应用使用系统自带 Windows PowerShell，不需要额外脚本或管理员权限。

## 开发与验证

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

目前已在 macOS 上完成构建与测试。Windows、Linux 的原生窗口焦点和全局快捷键仍需在对应桌面环境下验证。

```text
src/main.rs          窗口生命周期、全局快捷键、后台任务、交互绑定
src/search.rs        搜索与排序
src/icons/           各平台原生图标读取、解码与缩放
src/apps/macos.rs    macOS 应用包发现与启动
src/apps/windows.rs  Windows 开始菜单、Store 应用发现与启动
src/apps/linux.rs    Linux Desktop Entry 发现与启动
ui/launcher.slint    暗色圆角界面
```

暂未实现开机自启、托盘、拼音搜索、使用历史及安装包；应用发现覆盖上述标准注册位置，不扫描整块磁盘中的便携程序或 macOS 自定义应用目录。

技术参考：[Slint 窗口与 Winit 集成](https://docs.slint.dev/latest/docs/rust/slint/winit_030/)、[global-hotkey 平台支持](https://docs.rs/global-hotkey/0.8.0/global_hotkey/)、[Desktop Entry 规范](https://specifications.freedesktop.org/desktop-entry/latest-single/)、[Microsoft Get-StartApps](https://learn.microsoft.com/en-us/powershell/module/startlayout/get-startapps)。

## 许可证

GPL-3.0-only，见 [LICENSE](LICENSE)。
