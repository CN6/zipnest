# ZipNest M6 — Windows 11 新版右键菜单（IExplorerCommand 稀疏包）

## 目标

让 ZipNest 出现在 **Windows 11 新版右键菜单**（非"显示更多选项"）中：右键任意文件 /
文件夹 / 文件夹背景 → **添加到压缩包…** → 启动 `zipnest.exe --add "<path>"`，打开
新建向导并预填路径。

状态：**已实现并实机验证通过**。

## 硬约束（用户明确要求）

- **不改机器级设置**：不手动写 HKLM、不装受信任证书、不动经典菜单开关。
- 只按用户注册（`Add-AppxPackage -Register`，无 `-AllUsers`），可一键 `Remove-AppxPackage`。
- 复用现有 `--add` 参数，不改已有功能。

## 关键结论（微软 Learn + 真实项目 grepWin/dnGrep/NanaZip/Bandizip/Devolutions 实证）

- Win11 新菜单 = 实现 `IExplorerCommand` 的原生 COM DLL + MSIX 清单声明。
- `desktop5:ItemType` 的 `Type` 支持 `*`（文件）、`Directory`（文件夹）、
  `Directory\Background`（文件夹背景）。三个上下文统一用 desktop5。
- **稀疏包（packaging with external location）**：`.msix` / 包目录内只放
  `AppxManifest.xml` + `Assets\*`；`zipnest.exe` 与 `zipnest_shell.dll` 放在安装目录，
  注册时用外部位置连接。`com:Class Path="zipnest_shell.dll"` 相对外部位置解析。
- **本机（Win11 build 28020）无法用 `Add-AppxPackage -Path x.msix -AllowUnsigned`
  注册未签名包**（报 `0x80073D2C`：发布者未签名/不受信任）。开发者模式下可用
  **`Add-AppxPackage -Register AppxManifest.xml -ExternalLocation <安装目录>`** 注册
  松散清单，无需证书。卸载：`Remove-AppxPackage`。

## 实现

- `apps/zipnest-shell`（cdylib，产出 `zipnest_shell.dll`）：
  实现 `IExplorerCommand` + `IAgileObject` + `IClassFactory` +
  `DllGetClassObject` / `DllCanUnloadNow`。
  - `GetTitle`：按 `settings.json` 的 language 或系统 UI 语言返回中/英标题。
  - `GetState`：**仅当本 DLL 同目录存在 `zipnest.exe` 时**返回 `ECS_ENABLED`，否则
    `ECS_HIDDEN`（DLL 与 exe 同在安装目录）。
  - `Invoke`：取 `IShellItemArray` 全部路径 → `CreateProcessW` 启动
    `zipnest.exe --add "p1" "p2"`（MSVC 引号规则）。
  - 所有 COM 入口 `catch_unwind`，不向 Explorer/代理进程抛出 panic。
- `packaging/win11-shell/`：`AppxManifest.xml`（Identity / Properties /
  Application + com:Extension + desktop4/5:Extension + rescap:runFullTrust、
  uap10:AllowExternalContent）、`Assets\*`、`build-package.ps1`（makeappx）、
  `install.ps1` / `uninstall.ps1`（按用户注册/卸载）。

## 根因（本次修复）

`exe_path()` 把**目录**又取了一次 `parent()`，于是把
`<安装目录>\zipnest_shell.dll` 解析成 `<安装目录的上一级>\zipnest.exe`；
`GetState` 找不到 exe → 返回 `ECS_HIDDEN`，命令被系统丢弃（表现为菜单里不出现，
并在 WER 里留下 `MoAppHang` / `dllhost.exe` 记录）。修复：`module_path()` 返回 DLL
全路径，`exe_from_dll()` 只取一次 `parent()`。

## 验证（实机）

- `cargo test -p zipnest-shell --release` 全绿（5 项）。
- 新菜单中出现「添加到压缩包…」：对 `.zip` / 文件夹 / 文件夹背景均验证通过。
- 点击「添加到压缩包…」→ ZipNest 新建压缩包向导打开，`要添加的文件` 已预填该路径。
- 无 HKLM 手写项、无证书、无经典菜单开关；`Get-AppxPackage ZipNest.Shell` 可见；
  `uninstall.ps1` 可一键移除。

## 后续（未做）

- 把 `install.ps1 -InstallDir $INSTDIR` 接进 `packaging/installer.nsi`（安装时注册、
  卸载时移除），并随版本发布（升 `CURRENT_VERSION` 于 `apps/zipnest-native/src/main.rs`）。
- 如需给普通用户（无开发者模式）用，后续考虑正式签名包 + `Add-AppxPackage -Path`。
