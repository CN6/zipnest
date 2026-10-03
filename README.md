# ZipNest 解压缩

> 免费、开源、无广告、**原生界面**（无 WebView2、无浏览器内核）的 Windows 解压缩软件，基于官方 7-Zip 引擎（`7z.dll` 动态加载）。

ZipNest 是开箱即用的压缩包管理工具：**双击压缩包直接打开**，解压默认在压缩包所在目录下新建一个同名文件夹，文件不会散落到当前目录，跟 WinRAR / 7-Zip 的习惯一致。所有功能免费，**没有任何功能被锁定**，如果你觉得好用，可以扫码请我们喝杯咖啡。

## 下载安装

到 [Releases 页面](https://github.com/CN6/zipnest/releases) 下载最新版安装包：

- `ZipNest_x64-setup.exe` —— 安装版（推荐，双击安装即可）
- `ZipNest_x64_portable.zip` —— 便携版（解压即用，不用安装）

> 仅支持 **Windows 10 / 11**（x64）。**无需任何运行时依赖**：原生界面，不装浏览器内核，后台零残留进程。

## 功能

**解压**：ZIP、7z、RAR、TAR、GZ、BZ2、XZ、ISO 全支持，可浏览归档内容、按需解压部分文件、拖拽打开。

**创建**：ZIP / 7Z / TAR（含 .tar.gz / .tar.bz2 / .tar.xz），可设压缩等级与方法。

**安全**：AES-256 加密、7z 文件名加密、分卷压缩、7z 自解压（SFX）打包。

**集成**：
- 把 ZipNest 设为压缩包默认打开程序（设置里一键开启）
- 文件夹右键菜单一键解压
- 解压默认解到 `<压缩包所在目录>\<压缩包名>\`，自动建好文件夹，文件不散落

**其它**：内置文本/图片/十六进制预览、中英双语、随窗口自动放大。

## 使用方式

打开软件 → 打开或拖入一个压缩包 → 左侧浏览文件 → 选中条目右侧预览 → 一键解压。就这么简单。

## 支持我们

ZipNest 完全免费。如果你觉得它有用，打开软件 **设置 →「支持我们」**，扫微信 / 支付宝收款码即可。每份支持都会让我们有动力继续维护。

## 开发

```powershell
cargo test --workspace            # Rust 单元/集成测试
cargo run -p zipnest-native       # 开发运行（原生界面）
cargo build -p zipnest-native --release
```

## 许可

- 本项目代码：**MIT**（见 `LICENSE`）
- `7z.dll` 为官方未修改的 7-Zip 二进制（LGPL），运行时动态加载、不做静态链接；其许可随安装包附带（`licenses/7zip.txt`）
- `vendor/7zip-sdk` 头文件来自 7-Zip SDK（LGPL），见 https://www.7-zip.org/
