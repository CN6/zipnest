# ZipNest M5 — 原生 UI（去除 WebView2）

## 目标

把 Tauri + React + WebView2 的前端替换为 **Rust 原生 egui UI**，同一进程直调
`zipnest-ipc` 服务层（引擎/安全/设置/Shell 集成全部复用）。结果：

- 零 WebView2 / 零 msedgewebview2.exe 残留进程
- 内存占用大幅下降（无浏览器内核）
- 安装包更小

## 架构

```
apps/zipnest-native (egui/eframe, 新二进制)
  └── 直调 zipnest-ipc::IpcService
        ├── open_archive / list_children / read_entry_bytes
        ├── extract / create_archive / cancel
        ├── settings_get / settings_set
        └── shell_register（WindowsRegistry）
```

`IpcService` 的 emit 回调把 job 事件转发给 UI（egui 侧用 Arc<Mutex<Vec<JobEvent>>> 缓冲，
每帧消费）。不再需要 Tauri command / serde DTO 桥。

## 依赖

- `eframe` / `egui`（原生 UI）
- `rfd`（原生文件/文件夹选择对话框）
- 复用现有 crates：`zipnest-ipc`、`archive-core`（间接）

## 任务

1. **脚手架**：workspace 加 `apps/zipnest-native`，eframe 窗口，中文默认字体
   （Windows 加载 `msyh.ttc` / 雅黑），菜单栏框架
2. **浏览**：打开归档 → 列表（名称/大小/修改时间/加密）→ 目录导航 → 单选/多选
3. **解压**：选中项 → 解压对话框（默认目标=压缩包所在目录）→ 进度条/取消 → 完成提示
4. **创建**：向导（格式/等级/方法/密码/文件名加密/分卷/SFX）→ 进度
5. **预览**：文本 / 十六进制 / 图片（选中一个文件时）
6. **设置**：语言 / 默认解压目录 / 覆盖策略 / 预览上限 / 关联与右键开关（调 shell_register）
7. **捐赠**：状态栏「支持我们」→ 弹窗显示文案 + 微信/支付宝二维码
8. **i18n**：zh/en 两套文案（从现有 JSON 迁移进 Rust）
9. **收尾**：删除/停用 Tauri 版，打包脚本改指向原生版

## 验证

- `cargo test --workspace` 保持全绿（引擎层未动）
- `cargo build --release` 产出原生 exe
- 实机冒烟：开归档/解压/创建/设置/捐赠
