# Quick Translator

Take a Panel 的 Rust 翻译插件。输入 `tr` 并按 Enter 在 Take a Panel 内打开翻译面板；也可以输入 `tr 要翻译的文字` 预填原文。打开后原文输入框自动获得焦点，按 Enter 翻译、Shift+Enter 换行。面板中可从下拉菜单选择源语言和目标语言；插件设置中可保存默认方向。语言是否支持取决于翻译服务。

## 安装

需要包含插件面板支持的 Take a Panel、Rust 1.92 或更新版本及 `curl`。在本目录执行：

```sh
cargo build --target-dir ../../target
```

在 Take a Panel 的「设置 → 插件」中添加本目录下 `manifest.json` 的绝对路径。Windows 用户先将清单中 `executable` 改为 `../../target/debug/quick-translator.exe`。完成后输入 `tr`，等待「打开翻译面板」出现，再按 Enter。

翻译使用 [MyMemory API](https://mymemory.translated.net/doc/spec.php)，需要联网，并会将输入的文字发送给该服务。单次原文最多 500 UTF-8 字节，服务有使用额度限制。翻译请求在后台执行，失败时面板会显示错误。
