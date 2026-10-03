# apps/zipnest — 已退役的 Tauri 实现

这是 ZipNest 早期的 Tauri + React 界面，**已由 [`apps/zipnest-native`](../zipnest-native)（egui 原生界面）取代**，不再随安装包发布。

保留在仓库中仅供历史参考，**不要在此基础上开发新功能**。

## 为什么退役

Tauri 在 Windows 上依赖 WebView2，与 ZipNest「原生界面、无需任何运行时依赖」的定位冲突。原生版基于 `egui`，无浏览器内核、无 WebView2。

## 相关

- 现行界面：`apps/zipnest-native`
- 迁移计划：`docs/superpowers/plans/2026-10-02-zipnest-m5-native-ui.md`
