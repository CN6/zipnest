# NSIS 离线/镜像安装（GitHub 不可达时用）

`tauri build` 需要从 GitHub 下载 NSIS 工具链；这台机器访问不了 GitHub。
以下方法通过 ghfast 镜像 + SHA1 校验装好，构建器会直接复用（不再下载）。

## 适用版本

tauri-cli 2.12.0（`pnpm tauri build` 时的内嵌 bundler 规格，取自
`crates/tauri-bundler/src/bundle/windows/nsis/mod.rs` 对应 tag）：

| 文件 | URL（镜像前缀 `https://ghfast.top/` + 原始 URL） | SHA1 |
|---|---|---|
| nsis-3.11.zip | `github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip` | `EF7FF767E5CBD9EDD22ADD3A32C9B8F4500BB10D` |
| nsis_tauri_utils.dll (v0.5.3) | `github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll` | `75197FEE3C6A814FE035788D1C34EAD39349B860` |

## 摆放（Windows）

```powershell
$base = "$env:LOCALAPPDATA\tauri"
New-Item -ItemType Directory -Force $base | Out-Null
# 1) 下载两个文件到 $base，用 Get-FileHash -Algorithm SHA1 校验上表
# 2) 解压 nsis-3.11.zip 到 $base，把解压出的 nsis-3.11 目录改名为 NSIS
# 3) 把 nsis_tauri_utils.dll 放到：
#    $base\NSIS\Plugins\x86-unicode\additional\nsis_tauri_utils.dll
```

构建器（`get_and_extract_nsis`）检查 `NSIS_REQUIRED_FILES` 是否齐全 +
`nsis_tauri_utils.dll` 的 SHA1，齐全则跳过下载。

## 其它可用镜像

`ghfast.top`、`ghproxy.net`、`ghps.cc`（均能转发 raw.githubusercontent /
github release）。SourceForge 对脚本返回验证页，不推荐。

## 改版本时

tauri-cli 升级后内嵌 bundler 的 URL/SHA1 可能变。做法：在
`https://docs.rs/crate/tauri-bundler/<对应版本>/source/src/bundle/windows/nsis/mod.rs`
（或对应 GitHub tag 的 `crates/tauri-bundler/src/bundle/windows/nsis/mod.rs`）
里查 `NSIS_URL` / `NSIS_SHA1` / `NSIS_TAURI_UTILS_URL` / `NSIS_TAURI_UTILS_SHA1` /
`nsis-3.x` 解压目录名。
