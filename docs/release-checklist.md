# ZipNest 发布检查清单（Release Checklist）

> 每次准备发布前逐项执行。任何一项失败 → 停止发布，修复后重跑。

## 0. 版本号一致（必须）

- [ ] `apps/zipnest-native/src/main.rs` 的 `CURRENT_VERSION`（自动更新比较靠它）
- [ ] `apps/zipnest-native/src/main.rs` 的 `UPDATE_UA`
- [ ] `packaging/installer.nsi` 的 `VERSION` 与 `OutFile`
- [ ] `packaging/win11-shell/AppxManifest.xml` 的 `Version`（如 `0.3.0.0`）
- [ ] `CHANGELOG.md` 已补本次版本条目

三处必须一致；不一致会导致应用自己判断"有新版本"或安装包文件名为旧版。

## 1. 构建产物正确（必须）

- [ ] `cargo build -p zipnest-native --release` 成功
- [ ] 发布 exe 为 **GUI 子系统**（不是 console，否则双击弹黑色命令框）
  - 检查：PE OptionalHeader.Subsystem == 2（GUI）
- [ ] exe 已内嵌图标（`ExtractAssociatedIcon` 能读出非空图标）
- [ ] emit 便携目录 `dist-portable\ZipNest\zipnest.exe` 为最新 release exe
- [ ] 跑 `packaging\win11-shell\stage-portable.ps1`，确认 `dist-portable\ZipNest\zipnest_shell.dll` 与 `Win11Shell\`（含签名 `ZipNestShell.msix`）为最新（NSIS 打包要用）
  - 默认用**自签名证书**（免费；安装器会把公钥 `ZipNestCodesign.cer` 导入本机信任库再安装，无需开发者模式）。如需"受信任签名"（消除 SmartScreen 未知发布者）：`stage-portable.ps1 -PfxPath <cert>.pfx -PfxPassword <pwd>`（CA 证书 / SignPath Foundation）

## 2. 关联/快捷方式（严重，历史教训）

- [ ] 注册表 `HKCU\Software\Classes\ZipNest.<ext>\shell\open\command` 指向**发布路径**（安装目录或便携目录），且命令带正确引号：`"<exe>" "%1"`
- [ ] **绝不**指向 `target\debug\...` 或 `target\release\...`（开发脚本已拒绝，发布时要抽查）
- [ ] v0.4.8 起还有三处必须存在（否则"装完不是默认"会复发）：
  - [ ] 9 个扩展名的默认值指向 `ZipNest.<ext>`：`HKCU\Software\Classes\.zip` 等
  - [ ] `HKCU\Software\RegisteredApplications\ZipNest` → `Software\ZipNest\Capabilities`，
        且 `...\Capabilities\FileAssociations\.<ext>` 齐全（决定 ZipNest 是否出现在系统「默认应用」里）
  - [ ] `HKCU\Software\Classes\Applications\zipnest.exe\shell\open\command`（"打开方式"里的名字与命令）
- [ ] 安装包内含 `zipnest.exe --register-integration` 调用（v0.4.8 起安装即注册，`installer.nsi` 内搜该字符串）
- [ ] **安装程序绝不能等待 `zipnest.exe`**（v0.4.8 用 `nsExec::ExecToLog` 等它，在「旧 exe 没被替换掉」的机器上卡死在「创建快捷方式」）：
      只允许 `Exec`（启动即返回）；调用前用 `GetDLLVersion` 挡掉旧版本；每个步骤都要有自己的
      `DetailPrint`，这样用户截一张详情图就能指出卡在哪一步
- [ ] 安装包内包含桌面快捷方式（`$DESKTOP\ZipNest.lnk`）
- [ ] 卸载时删除桌面快捷方式

## 3. 双击冒烟（必须，真机）

- [ ] 用**发布 exe** 双击/命令行打开一个 `.zip` → 正常打开、无黑色命令框
- [ ] 打开 `plain.zip` 列表正常
- [ ] **新机默认关联冒烟**（v0.4.8 起必做）：
  1. `zipnest.exe --unregister-shell` 清空关联（模拟新装机器）
  2. `zipnest.exe --register-integration`（或直接启动一次程序，走首次启动路径）
  3. `cmd /c start "" x.7z`（选一个**没有 UserChoice** 的扩展名）→ 应启动 `zipnest.exe`
  4. 只启动程序（不带参数）再查一次：`%APPDATA%\ZipNest\settings.json` 的 `integration_applied` 必须变成 `true`
- [ ] 右键文件夹 → ZipNest → 打开正常（若系统策略拒绝 `*\shell`，属已知降级，不算失败）
- [ ] Win11：安装后重启资源管理器，**新菜单**出现「添加到压缩包…」（文件/文件夹/空白处），点击打开新建向导并预填路径；卸载后条目消失

## 4. 自动更新

- [ ] 发布版本号 > 上一个大版本
- [ ] 旧版启动后能收到更新提示（版本比较正常）

## 5. Git / 发布

- [ ] `cargo test --workspace` 全绿
- [ ] clippy 无本项目警告
- [ ] 提交全部改动，推送 `master`
- [ ] `gh release create vX.Y.Z` 挂上 `ZipNest_X.Y.Z_x64-setup.exe` 和便携 zip
- [ ] Release notes 含 CHANGELOG 本次条目
- [ ] 桌面安装包更新为最新版（清理旧版）

## 6. 已知环境限制（不算失败）

- 本机系统策略拒绝 `HKCU\Software\Classes\*\shell`（所有文件右键）与
  `Directory\Background\shell`（空白处右键）→ 产品显示中文警告并回滚开关。
- 这属于加固策略，不是发布缺陷。
- **已经被用户指定给别的程序的扩展名无法由程序改回**：`HKCU\...\FileExts\.<ext>\UserChoice`
  带 Deny-SetValue ACE，写入 ProgId 报"不允许所请求的注册表访问权"，`reg delete /f`
  也报拒绝访问（v0.4.8 实测）。凡此类扩展名，设置页会列出并给出「打开系统默认应用」
  按钮 —— 那是唯一合法路径（绝不伪造 hash）。
- 本机 shell 探测 API 不可靠：`assoc .ext` 报"File association not found"、
  `AssocQueryStringW(ASSOCSTR_EXECUTABLE)` 对 `.txt` 也返回 `0x80070483`。
  校验一律以注册表本身 + `start` 实测为准。

---

## 本次经验教训（2026-10-02）

**事故**：v0.2.1/0.2.2/0.2.3 发布后，用户双击压缩包报"无法访问此界面"并弹黑色命令框。

**根因**：开发冒烟时用 debug exe 打开了关联开关，`shell_register` 把
`target\debug\zipnest.exe`（console 子系统）写进了正式关联命令。发布版安装包
与注册表关联路径不一致，且 debug exe 存在控制台。

**防复发**：
1. 代码层：`shell_register` 拒绝任何含 `/target/` 的 exe 路径（`error.shell.dev_path`）
2. 流程层：本清单第 0/1/2/3 项强制检查