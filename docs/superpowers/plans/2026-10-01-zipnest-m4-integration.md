# ZipNest M4 — 集成与交付（计划）

目标（spec §11）：设置 / 关联 / 右键 / 托盘 + i18n 全量 + 双架构打包 + 验收测试。

## 硬约束（沿用 AGENTS.md）

- 注册表一律 **HKCU per-user**（本机机器级注册表 ACL 被清、UAC 关闭；spec §10）。
- 不引入不必要的重依赖：注册表写入走 `reg.exe`（std::process），键值生成保持**纯函数**以便单测。
- 错误跨层只走 `IpcError { key }`；每个 key 中英双语。
- 前端不碰文件系统；一切经 IPC。
- 每任务：`cargo test --workspace` 绿 + `pnpm test` 绿 + clippy 零警告 + 英文 commit。

## 任务

1. **设置持久化 + IPC**（本计划先做）
   - `zipnest-ipc/src/settings.rs`：`Settings`（language / default_extract_dir / overwrite_policy / associate / context_menu / limits），`SettingsStore`（路径注入，load/save/`set(patch)`），默认值 + 损坏 JSON 回退。
   - `IpcService`：`settings_get()` / `settings_set(patch)`。
   - Tauri commands `settings_get` / `settings_set` + capabilities。
   - 测试：默认值、往返写读、缺字段合并、坏 JSON 回退、未知字段忽略。

2. **Shell 集成（HKCU）**
   - `zipnest-ipc/src/shell.rs`：
     - 纯函数 `assoc_ops(exe)` / `context_menu_ops(exe)` → `Vec<RegOp{key,name,value,kind}>`（扩展名 → ProgID → open 命令 + DefaultIcon；目录/背景右键“用 ZipNest 解压到…”）。
     - `apply(ops)`：Windows 下拼 `reg.exe add/delete` 执行（非 Windows 为空操作）。
   - `IpcService`：`shell_register(enable_flags)` 生成并应用，返回结果。
   - Tauri command `shell_register`。
   - 测试：键路径/值生成快照（x64 与含空格路径）、开关集合。

3. **托盘 + 单实例 + 外部调起**
   - Tauri v2 tray（打开主界面 / 设置 / 退出）；`tauri-plugin-single-instance` 把第二次启动的文件参数 → `open_file_request` 事件。
   - 双击关联文件启动时解析 `argv` → `open_file_request`。

4. **设置 UI + 关联管理 UI**
   - 设置对话框（语言、默认解压目录、覆盖策略、关联开关、右键开关、限额）。
   - 预留 `settings_get/set`、`shell_register` 绑定。

5. **i18n 全量核对**：`i18n/index.test.ts` 已做双语完整性校验；补齐 M4 新增 key。

6. **双架构打包**：`tauri.conf.json` bundle（NSIS，x64 + ARM64 各一）、`resources` 带 `engines/7z.dll`、`licenses/`、WebView2 Evergreen 引导；便携 zip。

7. **验收测试 + 手工清单**：Win11 右键菜单、关联双击、托盘、ARM64 实机（spec §8）。

## 状态

- [ ] 1 设置持久化
- [ ] 2 Shell 集成
- [ ] 3 托盘 / 单实例
- [ ] 4 设置 UI
- [ ] 5 i18n 全量
- [ ] 6 打包
- [ ] 7 验收
