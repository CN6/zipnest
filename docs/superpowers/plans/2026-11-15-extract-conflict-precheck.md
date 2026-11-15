# 计划：解压前先看目标目录 / 复制诊断信息为空 (v0.4.39)

## 问题

1. **「文件已存在」无条件弹出。** 按下解压对话框里的「开始解压」，`extract_dialog`
   直接构造 `ConflictPrompt` 把它挂上去，之后 `conflict_dialog` 只要看到它就把窗口
   画出来。全程没有触碰磁盘 —— 目标目录里一个同名文件都没有，也照弹不误。用户要
   的行为是：**先检查解压目录里有没有同名文件，只有真有冲突时才问「覆盖 / 跳过 /
   自动重命名」。**

2. **「复制诊断信息」复制出空内容。** `apps/zipnest-native/Cargo.toml` 里 eframe 是
   `default-features = false`，feature 列表是 `glow / default_fonts / accesskit /
   links / system_fonts / x11 / wayland` —— **没有 `clipboard`**。eframe 的 `clipboard`
   feature 打开的正是 `egui-winit/clipboard` → `arboard`。没有它，`egui-winit` 的
   `Clipboard::set_text` 只写进进程内的 `String`（`clipboard: String` 兜底字段）：
   应用自己 Ctrl+V 能粘出来，系统剪贴板里什么都没有。用 `strings` 查过
   `D:\ZipNest\zipnest.exe`，确实不含 arboard。用户复制之后去别处粘贴，得到空内容。

   `cargo tree -e features -i egui-winit` 确认：没有任何一条依赖打开了
   `egui-winit feature "clipboard"`。

## 改法

### 1. 冲突预检（新函数，在 archive-core 里，可测）

`crates/archive-core/src/extract.rs` 里已有 `os_path` / `os_exists`（超过 MAX_PATH
要加 `\\?\`），预检必须用同一套，否则长路径下「预检说没有、解压时发现有」。

- 把 `extract_to_disk` 里「选中条目 → `plan_entry_paths` → 目标相对路径」那一段
  抽成一个纯函数 `plan_selection(entries, selection) -> PlannedSelection`，预检和真正
  的解压各调一次，保证两边推出的文件名逐字节一致（自动重命名的 `a (2).txt`、
  `CON_` 这些改写都在这里）。**这一步是关键**：如果预检用自己的算法，改名规则一改
  就会错位，用户会在改名之后仍然撞名。
- 新增 `pub fn conflicts_on_disk(arc: &Archive, opts: &ExtractOptions) -> Result<usize, ZipnestError>`：
  按计划里的每个文件条目算 `dest.join(plan)`，`os_exists` 命中就计数；目录条目不算。
  返回**冲突文件数**。
- 只数文件，不数目录：目录是幂等的（`create_dir_all`），拿它问用户没有意义。

### 2. IPC 层暴露给 UI

`crates/zipnest-ipc/src/service.rs`：

- 抽 `fn extract_plan(&self, id, paths) -> Result<(entries, wanted), IpcError>`，把
  `extract_with_delete` 里「取条目缓存 → `select_entries` 展开目录 → 密码检查」的
  前半段抽出来，两边共用（预检不该复制一份选择逻辑，否则「选了子目录」在预检和
  实际解压里会展开成不同的集合）。
- 新增 `pub fn count_conflicts(&self, id, paths, dest) -> Result<u64, IpcError>`：
  只走条目缓存 + 磁盘查询，**不取归档锁**（提取进行中也能回答，和 `list_children`
  同一套理由）。目的目录不是绝对路径时按 `extract_with_delete` 的规则报 `error.io`。

### 3. UI：只有真冲突才问

`apps/zipnest-native/src/main.rs`，`extract_dialog` 的「开始解压」分支：

```
let req = ExtractRequest { .. on_conflict: String::new(), .. };
match self.svc.count_conflicts(req.archive_id, req.sources.clone(), req.dest.clone()) {
    Ok(0) => { /* 没有同名文件：直接开始，不弹窗 */ self.start_extract(req, pw); }
    Ok(_) => { self.conflict_prompt = Some(ConflictPrompt { req, password }); }
    // 预检出不了结果（不是归档、目的路径非法）不能让解压停在对话框里：
    // 交给 begin_extract 自己报错，那条错误和从前一模一样。
    Err(e) => { close = true; self.show_extract = false; self.error = Some(self.t(&e.key)); }
}
```

没有冲突时的 `on_conflict`：仍然传一个合法策略。选 `rename` —— 预检和真正开始之间
目录可能被别人写入（两个 ZipNest 同时解到同一处），`rename` 在这种情况下不会覆盖
任何东西，而 `overwrite` 会。用户没被问到，就不该有人被替换。

撤销现在写在 `conflict_dialog` 文档里的那句「每次解压都问一次」—— 行为变了，
文档跟着变。

### 4. 复制诊断信息

- `apps/zipnest-native/Cargo.toml`：eframe features 加 `"clipboard"`。
- 这是依赖变更，`Cargo.lock` 会加 arboard + clipboard-win + windows-sys 的相应 feature；
  `emit-portable.ps1` 的第三方许可证表会自动带上它们（它从 Cargo.lock 读）。

## 测试

- `archive-core`：`conflicts_on_disk` 在一个真实 zip 上跑 —— 空目标目录得 0；写进去
  一个同名文件后得 1；目录条目不计；`CON` 这类被改名的条目按**改名后**的名字比对。
  用 `fixtures/plain.zip`（没有的话用 `zip` dev-dependency 现造，archive-core 已有
  `zip = "6"` 的 dev-dependency）。
- 预检与解压同源：把 `plan_selection` 的产出与 `extract_to_disk` 实际写出的路径比对
  （同名文件预检命中 ⇔ 解压时走 `OnConflict` 分支）。
- `zipnest-ipc`：`count_conflicts` 对不存在的归档 id 报 `error.not_an_archive`；
  相对路径报 `error.io`。
- 诊断信息：`render()` 的测试已覆盖内容非空；这次的问题是**剪贴板没接上**，属于依赖
  配置，单元测试碰不到。验证方式写进交付说明：构建后复制、粘贴到记事本。

## 版本

0.4.39 + CHANGELOG（两条用户可感知的修复）+ 版本号四处同步，`check-versions.ps1` 通过。
**不发布** —— 按惯例构建、覆盖 `D:\ZipNest\zipnest.exe`，等用户实际验完再说。
