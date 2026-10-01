# ZipNest 验收清单（M4 交付）

自动门禁：`cargo test --workspace` 全绿（当前 96 passed / 1 ignored）、
`cargo clippy --workspace --all-targets -- -D warnings` 零警告、
`pnpm test` 19 passed、`pnpm build` 通过。每项改动后重跑。

## A. 核心功能（回归）

- [ ] 打开 ZIP / 7Z / RAR / TAR / GZ / BZ2 / XZ / ISO，浏览子目录
- [ ] 加密归档：无密码 → 密码框；错误密码 → 报错；正确密码 → 可读
- [ ] 解压（含覆盖策略、进度条、取消、完成后“在资源管理器中显示”）
- [ ] 创建 ZIP / 7Z / TAR(.gz/.bz2/.xz)、压缩等级/方法
- [ ] 7Z 密码 + 文件名加密（错误密码打不开）
- [ ] 分卷创建 + 用本应用打开 `.001` 并解压
- [ ] SFX（自解压 exe）生成、`7z l` 可列、防呆（锁 7z、禁分卷）
- [ ] 预览：文本 / 图片 / hex / 外部打开
- [ ] 拖拽打开归档

## B. 设置与集成

- [x] 设置页：语言切换即时生效且重启后保持（实机验证：切 en-US → 保存 → 重开仍为 English；修复了语言下拉 key 映射 bug）
- [x] 默认解压目录、覆盖策略、预览上限持久化（settings.json 字段读写正确）
- [x] 文件关联开关 → `.zip`/`.7z` 默认打开程序设为 ZipNest（HKCU 默认值验证通过）
- [x] 右键菜单开关 → 文件夹右键出现 ZipNest（`Directory\shell` 写入成功）
- [x] 被系统拒绝的组件显示中文原因，且开关回滚（`*\shell` 被拒 → 警告「系统拒绝了“所有文件”右键菜单」显示；修复了部分拒绝时语言不生效的问题）
- [x] 托盘：已按用户决策移除（关窗即退出，无常驻图标）——不再属于交付项
- [x] 关联文件双击 / `zipnest.exe x.7z` → 打开该归档（实机验证：第二实例带 out.zip 启动 → 主实例自动打开并显示 `src/`）

## C. 交付物

- [ ] `dist-portable/ZipNest/zipnest.exe` 独立运行（自带 `engines/7z.dll`）
- [ ] `ZipNest_0.1.0_x64_portable.zip` 解压即用
- [ ] NSIS 安装器（需可访问 GitHub 的环境补跑 `pnpm tauri build`）
- [ ] ARM64 构建（独立环境 + ARM64 `7z.dll`；spec §10 P0 风险）

## D. 已知限制（如实告知用户）

1. 本机 `HKCU\Software\Classes\*\shell`（所有文件右键）与
   `Directory\Background\shell`（空白处右键）被系统 ACL 拒绝 → 这两项开
   关会提示“系统拒绝”并保持关闭；文件关联与文件夹右键可用。
2. 托盘菜单文字暂为固定中文，不随应用内语言切换。
3. 安装器需要下载 NSIS（GitHub），当前网络不可达时先发便携版。
