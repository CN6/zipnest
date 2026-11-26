# 给新会话说的话（复制下面「提示词」整段即可）

> 已按你 2026-11-27 的最终拍板更新：
> - 不做「最近打开」；
> - **压缩/解压多线程不做了**（解压实测已够快：506 MB、3:1 压缩比的包，
>   ZIP 约 116 MB/s、7z 约 176 MB/s；多文件打包本来就是多线程）；
> - 其余按建议文档。
> 详细依据见 `docs/proposals/2026-11-27-improvement-proposals.md`（A0 节保留
> 了实测数据，并注明"已取消，不要重提"）。

---

## 提示词（整段复制）

```text
仓库：D:\claude\zipnest（ZipNest，Windows 解压缩软件，Rust + egui/eframe 原生界面，
7z.dll 动态加载，v0.5.2 已发布）。先读 AGENTS.md（不变量和发布配方都在里面），
再读 docs/proposals/2026-11-27-improvement-proposals.md（已和用户对齐的建议全集）。

不要做的事（用户已拍板，勿重提）：
- 不做「最近打开」。
- 不做压缩/解压多线程改造（多文件打包本来就多线程；解压实测已够快，
  见文档 A0 节，那里有实测数据）。不要为了它改 IInArchive、不要换 Deflate 引擎。
- 不做托盘、任务中心、内嵌预览窗格、Mica/亚克力背景、自绘标题栏、
  自动编码探测、把 .msi/.chm/.cab 设成默认打开方式。

本轮任务，按优先级：

【P1 第一批（S 或 M，感知强、风险低）】
1) 测试压缩包（CRC 校验，S）：IInArchive::Extract 的 testMode 第一参已有
   （vtables.rs:72），extract.rs:1318 现在硬编码 0，加开关复用现有进度/取消/
   配额/结果窗；工具栏+右键可进入；加密包先要密码；分卷缺卷逐条列出。
2) 行右键菜单（B1）+ 键盘导航（B2）：方向键/Home/End/PgUp/PgDn/Enter/Shift 扩选，
   焦点环用主题蓝 1px、仅键盘操作后出现并滚入视野；行用 Sense::CLICK 自持
   cursor，别跟 egui 焦点系统打架；菜单项：打开 / 解压选中项到… / 复制路径。
3) 进度显示当前文件（B3，S）：JobEvent::Progress 加 current_path（引擎侧
   ExtractProgress.current_path 已有，只是没穿过 IPC）。
4) 空状态引导（B4，S）：更醒目的"拖到这里" + 首次启动一行"已设为默认解压软件"。
5) 完成反馈（B5，S）：窗口未聚焦时 ViewportCommand::RequestUserAttention 闪任务栏；
   成功用状态栏 3 秒非模态提示；失败仍弹现有错误窗。
6) 经典右键免对话框动词第一步（A2，M）：解压到当前文件夹 / 解压到 <包名>\
   / 添加到 <文件名>.zip；守住 v0.4.39 的"真撞名才问"；必须给 ExtractStats
   增记"冲突改名"（现在 renamed 只记名字清洗，OnConflict::Rename 的改名对用户
   不可见——这违反 Nothing is ever lost，是唯一必须动引擎的地方）+ IPC 事件
   + 结果窗逐条列出。Win11 一级菜单动词放第二批。

【P2 第二批】
7) 向已有 ZIP/7z 追加/删除（A3，L）：现有 create_archive 永远新建（create.rs:861
   的 UpdateItems 不打开 IInArchive）；7-Zip 客户端做法是打开旧包当 IInArchive
   再 UpdateItems。首版只 ZIP/7z、不做分卷/SFX/TAR，临时文件+原子替换，
   失败保留原包。删除要确认。
8) 开放更多可解格式（A4，M）+ 每格式关联矩阵（B6）：引擎能解 .zst/.tzst、Cab、
   Arj、Lzh、Xar、Cpio、Rpm、Deb、Msi、Wim、Dmg、Chm、SquashFS、VHD/VHDX、UDF、
   APFS、Ext 等；四份硬编码白名单挡着（shell.rs:12、main.rs:5488、打开对话框过滤、
   创建 chips）。注意：.msi/.chm/.cab 只能进"打开方式"，绝不设默认；识别靠引擎
   签名不靠扩展名；每格式勾选数据模型 assoc_extensions 已有、UI 缺。
9) MOTW（A5，S）+ 视需要拖出/压缩率列（A6，L/M）。

【P3 看反馈再定】
10) 批量解压（A7，M）；旧编码文件名（A8，M，主要非中文系统/老包）；
    外观打磨 B7/B8/B9。

硬约束（AGENTS.md 已有，重申）：密码只在内存；任何一次运行报成功就必须每个
选中条目都写了（failed 非空即失败，逐条列出）；错误只走 i18n key 且中英双份；
前端不直接碰文件系统；创建/设置是主窗口的页面不是独立窗口；7z.dll 原样分发、
可被用户替换；RAR 只解不建；无遥测、无功能门；CHANGELOG 只写用户能感知的
变化（纯内部改动放提交说明）。

流程：非平凡工作先写计划到 docs/superpowers/plans/，再动手；每个任务先写测试；
`cargo test --workspace` 全绿 + `cargo clippy --workspace --all-targets` 零警告
才算完成；每个任务一个英文提交。用 pwsh 7（不要 5.1），中文输出先设
[Console]::OutputEncoding=[Text.UTF8Encoding]::new()。UI 改动用
scripts\dev-run.ps1 / ui-shot.ps1 做真实截图验证，不能只靠"应该没问题"。
发布照 AGENTS.md 的「Shipping a release」配方，产物只上传 GitHub、本地不留。

建议从这个顺序开工：A1 测试压缩包 → B1+B2 → B3 → B4 → B5 → A2 第一步。
每完成一项跑全量测试并在 CHANGELOG 记用户能感知的那句话。
```

---

## 提醒（不用复制进提示词）

- **多线程已彻底取消**：文档 A0 节改成了"已取消，不要重提"的墓碑条目，只留
  实测数字；优先级路由里的第零批已删除。`AGENTS.md` 的 Speed 条保留的是更正
  后的事实（多文件并行、单文件串行），不是计划，正常读即可。
- 今晚所有基准脚本、临时样本（约 1.7 GB）、示例程序都已删除；
  `crates/archive-core/src/archive.rs` 里临时放开的 `raw()` 已还原为
  `pub(crate)`。仓库里只剩 `AGENTS.md` 的 Speed 条更正和 `docs/proposals/`
  两个文档，均未提交。
- 顺带发现一个本机环境问题（不影响新会话开工前的阅读，但构建会撞上）：
  Git 的安装把 `D:\Git\usr\bin` 写进了系统 PATH，里面的 coreutils `link.exe`
  会抢在 MSVC 链接器前面，导致 `cargo build` 报 `link: extra operand`。
  v0.5.2 以前的 release 构建显然是在没这个问题的环境里跑的。新会话如果遇到
  同样的报错，把 `D:\Git\usr\bin`（和 `ucrt64`）从 PATH 里剔除再构建即可；
  这是环境问题，不是仓库问题。