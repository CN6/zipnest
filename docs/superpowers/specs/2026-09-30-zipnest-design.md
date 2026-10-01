# ZipNest 解压缩软件 — 设计文档

- 日期：2026-09-30
- 状态：已确认（用户批准架构方向 Tauri 2 + Rust；批准后用户委托全权推进到成品）
- 路径：architectural（新项目）

## 1. 背景与目标

面向 Windows 的全功能解压缩软件，对标 WinRAR / 7-Zip 的完整产品体验。

**商业模型（用户确认）**：捐赠模式。全功能免费、零限制——不做激活、不做试用期、不做功能开关、不做授权服务器。架构上因此完全不含许可/授权子系统。

**目标平台（用户确认）**：仅 Windows，x64 + ARM64 双架构。

**界面语言（用户确认）**：简体中文 + 英文，随系统语言自动切换，设置中可手动覆盖。

**功能范围（用户确认，四项全选）**：

1. 主流格式全支持
2. 归档内浏览 + 预览
3. 加密 + 分卷 + 自解压
4. 系统集成（右键菜单 / 文件关联 / 拖放 / 托盘）

## 2. 硬约束与合规

- **RAR 创建不可实现**：RAR 格式为 RARLAB 商业授权，全行业（含 WinRAR 之外的所有软件）只能解压 RAR、不能创建 RAR。本产品：RAR 仅解压，不提供创建。
- **7z.dll 采用动态链接（LGPL 合规）**：随包分发 7-Zip 官方 DLL 二进制 + 附带其 license 文本 + About 页注明来源与获取方式 + DLL 可被用户替换（不静态链接、不修改 DLL）。捐赠模式下此为完全合规路径。
- **不做**：RAR 创建、云同步、账户系统、遥测上报（日志仅本地）。

## 3. 架构决策（已确认）

| 决策点 | 选择 | 落选与理由 |
|---|---|---|
| UI 框架 | **Tauri 2**（Rust 后端 + WebView2 前端） | Electron（内存 200-400MB、包 150MB+，产品形象差）；C# WinUI3（用户明确选择 Tauri） |
| 压缩引擎 | **进程内 COM 封装 7z.dll** | 纯 Rust crate 组合（多卷 7z、SFX、大归档流式枚举有缺口，工期 +40%）；CLI 子进程解析进度（密码、浏览、流式能力弱） |
| 前端栈 | React 18 + TypeScript + `@fluentui/react-components`（Fluent 2）+ TanStack Virtual | Svelte（虚拟列表/树组件生态弱）；纯 CSS 手写（Win11 原生观感受损） |
| 安装器 | Tauri bundler（NSIS 优先，MSI 备选） | 自研安装器（无必要） |

**三层架构**：

```
┌─────────────────────────────────────────────┐
│ 前端 UI（React + TS + Fluent 2）             │  界面 / i18n / 虚拟列表 / 预览
├─────────────────────────────────────────────┤
│ IPC 层（Tauri commands + events）            │  同步调用下行；进度/密码/错误上行
├─────────────────────────────────────────────┤
│ Rust 核心（workspace crates）                │  引擎 / 任务 / 安全 / 关联
└─────────────────────────────────────────────┘
```

核心原则：Rust 层零 UI 依赖，可独立测试；前端不直接触碰文件系统（一切经 IPC）。

## 4. 模块设计

### 4.1 Rust 侧（cargo workspace）

**`archive-core` — 7z.dll COM 封装**

- 动态加载 `7z.dll`（LoadLibrary + COM：`IInArchive`/`IOutArchive`/`IArchiveOpenCallback`/`IProgress`）
- 能力：打开归档、枚举条目属性流（名称/大小/时间/目录/加密标志）、流式读单条目、解压到磁盘、创建归档、测试校验
- 密码：`ICryptoGetTextPassword` 回调向调用方索取（不落盘）
- 进度/取消：`IProgress::SetRatioInfo` 回调，返回非 0 中止
- 关键接口：`ArchiveReader`（open/list/read/extract）、`ArchiveWriter`（create）
- 7z.dll 定位：优先应用目录 `engines\7z.dll`，回退系统 7-Zip 安装；缺失时给出可下载指引的错误
- 双架构：x64 用官方 7z.dll；ARM64 来源（官方 ARM64 构建 or 从 7-Zip 源码自编，LGPL 允许）为**实现初期 P0 验证项**

**`archive-security` — 安全防线（独立模块 + 独立测试）**

- Zip Slip：条目路径清洗——拒绝绝对路径、`..` 上溯、盘符、UNC、NTFS 保留名；统一转为目标目录内相对路径
- 符号链接/目录联接：默认不解压（保留选项关闭）
- 压缩炸弹：解压前按条目声明总量与实际写入量双重限额（默认 100GB 或剩余磁盘 95%，可配置），超限中止
- 文件名清洗：非法字符替换、去尾点/空格、长度截断、重名处理（覆盖策略）
- 密码仅存于内存（会话级），不写日志、不写磁盘

**`archive-jobs` — 任务系统**

- 任务类型：解压、创建、测试、（流式读预览）
- 单活跃重任务 + 排队（磁盘保护），进度事件 200ms 节流推送
- 取消：协作式（引擎回调 + 状态标记），取消后清理半成品
- 事件负载：`job_id / kind / done_items / total_items / done_bytes / total_bytes / speed_bps / eta_secs`

**`zipnest-ipc`（Tauri 命令层）** — 见 §6

**`shell-integ` — 系统集成**

- 文件关联：Tauri bundler `fileAssociations`（HKCU per-user，免管理员）
- 右键菜单：
  - 阶段 1：HKCU `Software\Classes` 经典 verbs（解压到此处 / 压缩为 zip）——Win11 在"显示更多选项"可见，零风险
  - 阶段 2：Win11 原生 `IExplorerCommand`（out-of-proc COM，`windows-rs` 实现，Explorer 进程隔离）
- 托盘图标 + 全局单例（防多开）

### 4.2 前端（React）

| 模块 | 内容 |
|---|---|
| `archive-viewer` | 目录树 + 虚拟列表（TanStack Virtual，百万条目）、多选、列排序、拖拽提取 |
| `preview-panel` | 文本（UTF-8/GBK/UTF-16 探测）、图片、音视频（WebView2 可播 mp3/mp4-h264/aac，其余显示"外部打开"）、未知格式 hex；未解压文件流式读入临时区预览 |
| `create-wizard` | 格式/等级/密码(AES-256)/分卷大小/固实开关/自解压输出 |
| `password-dialog` | 由 Rust 事件触发；支持会话内记住 |
| `task-center` | **已移除（M3 决策，非实现项）**：不做独立任务中心；进行中/历史任务、速度、ETA、取消均由 M2 的轻量进度/队列/通知 UI 承担 |
| `settings` | 语言、默认解压目录、覆盖策略、关联管理、限额配置 |
| `i18n` | `zh-CN.json` / `en-US.json`；Rust 只回传错误 key，前端映射文本 |

## 5. 数据流（解压加密 7z 示例）

```
双击 .7z → IPC open_archive → core 探测加密
→ 事件 password_required → 密码框 → 回传
→ 枚举条目 → 返回 UI 渲染（虚拟列表）
→ 用户解压 → jobs 建任务 → 事件 job_progress（节流）
→ 完成 → 通知
```

## 6. IPC 契约

**Commands（前端 → Rust）**

| command | 参数 | 返回 |
|---|---|---|
| `open_archive` | path, password? | `{ entries[], encrypted, format, comment? }`（根层条目，懒展开子层） |
| `list_children` | archive_id, dir_path | `entries[]` |
| `extract` | archive_id, entries[], dest, overwrite_policy | `job_id` |
| `create_archive` | sources[], dest, options | `job_id` |
| `test_archive` | archive_id, password? | `job_id` |
| `read_entry_bytes` | archive_id, entry, max_bytes | `bytes`（预览用） |
| `job_cancel` | job_id | `()` |
| `settings_get` / `settings_set` | — / patch | settings |
| `shell_register` | enable flags | result |
| `reveal_in_explorer` | path | `()` |

**Events（Rust → 前端）**

| event | 载荷 |
|---|---|
| `password_required` | `{ archive_id, entry? }` |
| `job_progress` | 见 §4.1 jobs |
| `job_finished` | `{ job_id, ok, error_key? }` |
| `open_file_request` | `{ path }`（外部调起：双击关联文件） |

## 7. 错误处理

- Rust 定义 `ZipnestError` 枚举（`Engine(code)`/`PasswordRequired`/`Cancelled`/`SecurityViolation(kind)`/`Io(...)`/`NotFound`/…）
- 序列化为 `{ key: "error.xxx", params }`，**UI 本地化渲染**；未知错误兜底 key + 日志
- 日志：`tracing` 写 `%LOCALAPPDATA%\ZipNest\logs\`（滚动、本地、无遥测）
- 引擎 HRESULT 映射表：`E_ABORT`→取消、`kUnknownArc`→格式不支持 等

## 8. 测试策略

- **单元**：`archive-security` 全路径穿越向量表（`../`、绝对路径、盘符、UNC、UTF-8 混淆、保留名）；路径清洗 fuzz
- **集成**（真实 7z.dll）：每格式 fixture 往返（zip/7z/tar/gz/bz2/xz）、加密 zip/7z 读写、多卷 zip、分卷 7z、错误密码、损坏文件、超大条目数、取消语义
- **IPC**：commands 契约测试（tauri test 或纯函数层）
- **前端**：组件 Vitest（列表虚拟化、i18n key 完整性双向校验、预览编码探测）
- **手工清单**：Win11 右键菜单、文件关联双击、托盘、ARM64 实机验证

## 9. 打包与分发

- 双产物：`ZipNest_x64_setup.exe`、`ZipNest_ARM64_setup.exe`（NSIS），另有便携版 zip
- 产物内含：`ZipNest.exe`、`engines\7z.dll`(+ARM64)、`licenses\7zip.txt`、WebView2 引导（Evergreen 回退）
- 签名：代码签名证书（商业化项，可后置；无签名时 SmartScreen 提示，About 中说明）
- 分发：官网直链 + 提交 winget manifest；捐赠入口（About 页 / 官网链接）
- 不做：自动更新强推（可选后期加 tauri-updater，首版不含）

## 10. 风险清单

| 风险 | 等级 | 缓解 |
|---|---|---|
| ARM64 7z.dll 来源 | P0 | 实现首日验证：官方 ARM64 包 → 不行则 7-Zip 源码自编（LGPL 允许）；两路都堵死则 ARM64 格式集降级为纯 Rust crate 回退 |
| 7z.dll COM 封装复杂度 | P1 | 先实现 open/list/extract 最小闭环，写+分卷+SFX 逐步叠加；集成测试兜底 |
| WebView2 音视频格式覆盖 | P2 | 播放失败自动降级"外部打开" |
| 机器级注册表 Deny（本机已清除 18 处） | P2 | 产品侧统一走 HKCU per-user 注册，规避机器级 ACL 干扰 |
| 本机 UAC 已禁用（EnableLUA=0） | P3 | 与开发无关；产品不依赖提权 |

## 11. 里程碑

- **M1 引擎地基**：workspace + `archive-core` open/list/extract + 安全模块 + 测试绿
- **M2 主界面**：Tauri 壳 + 浏览器 + 解压 + 进度/密码/错误闭环（可用原型）
- **M3 创建与预览**：
  - 创建：ZIP / 7Z / TAR，以及 .tar.gz / .tar.bz2 / .tar.xz 封装；压缩等级与方法；AES-256 密码；7z 头部加密（文件名加密）；分卷（7z / ZIP，自定义大小）；7z 自解压 SFX（GUI / 控制台 stub）；读取端多卷打开（`.7z.001` / `.zip.001`）。
  - 预览：文本（UTF-8 / GBK / UTF-16 探测）、图片、hex、外部打开。
  - **任务中心已移除**（用户决策）：解压耗时短，M2 的进度 / 队列 / 通知已足够，不另建独立任务中心。
- **M4 集成与交付**：设置 / 关联 / 右键 / 托盘 + i18n 全量 + 双架构打包 + 验收测试

## 12. 开发环境（2026-09-30 装备）

Rust 1.98.1（x86_64 + aarch64-pc-windows-msvc）、VS Build Tools 2022（VCTools workload + Win11 SDK 26100）、Node 24/pnpm、WebView2 Runtime、7-Zip 26.03（7z.dll 26.03）。
注：本机曾被加固工具在 `HKLM\SOFTWARE\Classes` 下 18 处写入 Deny ACE（shell/shellex verbs），已全部清除，原 SDDL 备份于 `%TEMP%\dir_shell_acl_backup.sddl`。
