//! Tiny i18n: zh/en string tables, Chinese font loading, and image decode.

use eframe::egui;
use std::collections::HashMap;
use std::ops::Deref;

pub fn os_language() -> &'static str {
    // Windows returns e.g. "zh-CN" in GetUserDefaultUILanguage via env? Not set;
    // fall back to zh by default for this user, overridden by settings later.
    "zh-CN"
}

/// Decode a JPEG/PNG into an egui color image. Used for QR codes.
pub fn load_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, &img))
}

pub fn tr(lang: &str, key: &str) -> String {
    let table: &HashMap<&'static str, &'static str> = if lang == "en-US" { EN.deref() } else { ZH.deref() };
    table.get(key).copied().unwrap_or(key).to_string()
}

macro_rules! table {
    ($($k:literal => $v:literal),* $(,)?) => {{
        let mut m: HashMap<&'static str, &'static str> = HashMap::new();
        $( m.insert($k, $v); )*
        m
    }};
}

#[allow(dead_code)]
static ZH: std::sync::LazyLock<HashMap<&'static str, &'static str>> =
    std::sync::LazyLock::new(|| {
        table! {
            "app.open" => "打开压缩包",
            "create.new" => "新建压缩包",
            "browser.empty" => "未打开压缩包",
            "browser.columns.name" => "名称",
            "browser.columns.size" => "大小",
            "browser.columns.mtime" => "修改时间",
            "browser.columns.encrypted" => "加密",
            "browser.count" => "{count} 个项目",
            "browser.count_selected" => "已选 {selected} / 共 {count} 个",
            "browser.empty_dir" => "这个文件夹里没有内容",
            "browser.up" => "返回上一级",
            "browser.root" => "根目录",
            "browser.up_hint" => "退格键也可以返回上一级",
            "browser.drop_title" => "把压缩包拖到这里",
            "browser.drop_hint" => "或者点下面的按钮打开一个压缩包",
            "extract.title" => "解压到",
            "extract.dest" => "目标文件夹",
            "extract.browse" => "浏览…",
            "extract.overwrite" => "覆盖已存在文件",
            "extract.start" => "开始解压",
            "extract.all_hint" => "未选择条目：将解压整个压缩包",
            "extract.cancel" => "取消",
            "extract.success" => "解压完成",
            "error.title" => "错误",
            "error.engine" => "操作失败",
            "error.io" => "读写失败（文件被占用、路径无效或磁盘空间不足）",
            "error.not_an_archive" => "不是有效的压缩包",
            "error.password_required" => "需要密码",
            "error.password_incorrect" => "密码错误",
            "error.password_unsupported" => "该压缩包不支持密码",
            "error.cancelled" => "已取消",
            "error.security_blocked" => "条目名不安全，已跳过",
            "error.quota_exceeded" => "解压内容超出大小限制",
            "error.dll_missing" => "缺少解压引擎（7z.dll）",
            "error.settings.invalid" => "设置无效",
            "error.shell.dev_path" => "开发版路径不能注册为默认打开程序，请使用安装后的 ZipNest。",
            "error.shell.associate" => "文件关联注册失败",
            "error.shell.file_menu" => "文件右键菜单注册失败",
            "error.shell.directory_menu" => "文件夹右键菜单注册失败",
            "error.shell.background_menu" => "文件夹背景右键菜单注册失败",
            "settings.title" => "设置",
            "settings.back" => "← 返回",
            "settings.donate" => "支持我们",
            "settings.donate.title" => "支持 ZipNest",
            "settings.donate.body" => "ZipNest 完全免费。它为你省了时间，你的支持就是最好的回报。",
            "settings.donate.wechat" => "微信支付",
            "settings.donate.alipay" => "支付宝",
            "create.title" => "新建压缩包",
            "create.sources" => "要添加的文件",
            "create.add_files" => "添加文件…",
            "create.add_folder" => "添加文件夹…",
            "create.dest" => "输出压缩包",
            "create.dest_hint" => "点下面的「浏览…」选择保存位置",
            "create.format" => "格式",
            "create.level" => "压缩等级",
            "create.level.store" => "仅存储",
            "create.level.fastest" => "最快",
            "create.level.normal" => "标准",
            "create.level.maximum" => "最大",
            "create.level.ultra" => "极致",
            "create.method" => "压缩方法",
            "create.method.auto" => "自动",
            "create.method.copy" => "复制（不压缩）",
            "create.method.deflate" => "Deflate",
            "create.method.lzma2" => "LZMA2",
            "create.method.bzip2" => "BZip2",
            "create.volume" => "分卷大小",
            "create.volume.off" => "关闭",
            "create.volume.10m" => "10 MB",
            "create.volume.100m" => "100 MB",
            "create.volume.1g" => "1 GB",
            "create.volume.custom" => "自定义",
            "create.password" => "密码",
            "create.encrypt_names" => "加密文件名（仅 7z）",
            "create.sfx" => "自解压可执行文件（仅 7z）",
            "create.start" => "开始创建",
            "pick.all" => "所有文件",
            "pick.archives" => "压缩包",
            "format.zip" => "ZIP 压缩包 (*.zip)",
            "format.7z" => "7z 压缩包 (*.7z)",
            "format.tar" => "TAR 归档 (*.tar)",
            "format.tar.gz" => "TAR.GZ 归档 (*.tar.gz)",
            "format.tar.bz2" => "TAR.BZ2 归档 (*.tar.bz2)",
            "format.tar.xz" => "TAR.XZ 归档 (*.tar.xz)",
            "help.title" => "帮助",
            "help.format" => "压缩包格式。7z 压缩率最高；zip 兼容性最好（任何系统都能直接打开）；TAR 系列常用于 Unix/Linux，TAR 本身不压缩，配合 GZ/BZ2/XZ 才压缩。",
            "help.level" => "压缩等级：等级越高，压缩后文件越小，但打包越慢、越费 CPU。\n· 仅存储：不压缩，原样打包，最快\n· 最快：轻度压缩，速度快\n· 标准：速度和体积均衡（推荐）\n· 最大：压缩更小，速度较慢\n· 极致：体积最小，速度最慢",
            "help.method" => "压缩算法，影响压缩率和速度：\n· 自动：让程序按格式选最合适的\n· 复制：不压缩直接存（用于已压缩的文件）\n· Deflate：ZIP 常用，兼容性好\n· LZMA2：7z 常用，压缩率更高\n· BZip2：压缩率较高，速度较慢",
            "help.password" => "设置密码后，打开和解压这个压缩包都需要密码。留空表示不加密。7z 格式支持 AES-256 强加密。",
            "help.encrypt_names" => "勾选后，压缩包内的文件名也会被加密，看不到内容列表。只有 7z 格式支持。",
            "help.volume" => "把大文件拆成多个指定大小的小分卷，方便复制或上传。\n· 10 MB / 100 MB / 1 GB：固定分卷大小\n· 自定义：按你输入的体积分卷，例如 100m 或 1g\n· 关闭：不拆分，打成一个文件",
            "job.canceled" => "取消",
            "job.ok" => "完成",
            "job.ok_skipped" => "完成（{count} 个条目已跳过）",
            "job.close" => "关闭",
            "preview.title" => "预览",
            "preview.no_preview" => "无法预览此文件",
            "preview.truncated" => "预览已截断（文件过大）",
            "settings.language" => "语言",
            "settings.theme" => "外观",
            "settings.theme.system" => "跟随系统",
            "settings.theme.light" => "浅色",
            "settings.theme.dark" => "深色",
            "settings.section.general" => "常规",
            "settings.section.extract" => "解压",
            "settings.section.integration" => "系统集成",
            "settings.section.about" => "关于",
            "settings.section.feedback" => "帮助与反馈",
            "settings.feedback.hint" => "遇到问题？先点「复制诊断信息」，再把它粘贴进邮件发给我。这段文字只包含版本号、Windows 版本号、程序所在路径、几个开关状态和最近的错误代号 —— 不含任何文件名或压缩包内容。",
            "settings.feedback.copy" => "复制诊断信息",
            "settings.feedback.copied" => "已复制到剪贴板",
            "settings.feedback.email" => "反馈邮箱",
            "settings.feedback.copy_email" => "复制邮箱",
            "settings.feedback.mail" => "用邮件发送反馈",
            "settings.feedback.mail_subject" => "问题反馈",
            "settings.default_dir.hint" => "留空 = 解压到压缩包旁边的同名文件夹",
            "settings.language.system" => "跟随系统",
            "settings.language.zh" => "简体中文",
            "settings.language.en" => "English",
            "settings.default_dir" => "默认解压目录",
            "settings.overwrite" => "覆盖策略",
            "settings.ui_zoom" => "界面缩放",
            "settings.ui_zoom.percent" => "{percent}%",
            "settings.max_extract" => "解压大小上限 (MB)",
            "settings.max_extract.hint" => "单次解压写入的数据量超过该值就停止，用来防止超大解压（解压炸弹）。默认 65536 MB。",
            "settings.auto_update" => "启动时自动检查更新",
            "settings.auto_close" => "完成后自动关闭窗口",
            "settings.auto_close.hint" => "压缩或解压完成后自动关掉窗口。关掉后你就看不到「完成」提示和输出位置了，所以默认不自动关闭。",
            "settings.version" => "当前版本",
            "settings.overwrite.hint" => "当目标文件已存在时",
            "settings.overwrite.ask" => "询问我",
            "settings.overwrite.overwrite" => "覆盖",
            "settings.overwrite.skip" => "跳过",
            "settings.overwrite.rename" => "自动重命名",
            "settings.shell.associate" => "默认用 ZipNest 打开压缩包",
            "settings.shell.assoc_hint" => "安装后会接管 ZIP / 7Z / RAR / TAR / GZ / BZ2 / XZ / ISO 的默认打开方式；取消勾选会立刻还回去。",
            "settings.shell.blocked" => "系统给 {exts} 记住了别的默认程序，所以要再改回来一次 —— 点下面的按钮就行，不用去系统设置里翻。",
            "settings.shell.claim_ok" => "已改好：压缩包现在默认用 ZipNest 打开",
            "settings.shell.claim_partial" => "大部分格式已经改回来，个别格式没能改（系统仍在保护它）",
            "settings.shell.set_default" => "设为默认解压软件",
            "settings.shell.set_default.hint" => "点一下立刻生效，不用重启；以后你想换成别的程序，在系统「默认应用」里改一次即可。",
            "settings.shell.status_ours" => "当前状态：ZipNest 是压缩包的默认打开程序",
            "settings.shell.status_off" => "当前状态：未启用默认关联（上面那个勾选框是关的）",
            "settings.shell.context_menu" => "右键菜单",
            "settings.save" => "保存",
            "update.check" => "检查更新",
            "update.title" => "检查更新",
            "update.checking" => "正在检查更新…",
            "update.found" => "发现新版本",
            "update.download" => "前往下载",
            "update.hint" => "点「前往下载」会在浏览器中打开 GitHub 下载页，下载安装包后手动安装即可。",
"update.ready" => "新版本已下载。点击安装后软件会自动退出并静默安装，不弹黑色命令框；安装完成后重新打开就是新版本。",
            "update.install" => "立即安装",
            "update.failed" => "检查更新失败",
        }
    });

#[allow(dead_code)]
static EN: std::sync::LazyLock<HashMap<&'static str, &'static str>> =
    std::sync::LazyLock::new(|| {
        table! {
            "app.open" => "Open archive",
            "create.new" => "New archive",
            "browser.empty" => "No archive open",
            "browser.columns.name" => "Name",
            "browser.columns.size" => "Size",
            "browser.columns.mtime" => "Modified",
            "browser.columns.encrypted" => "Encrypted",
            "browser.count" => "{count} items",
            "browser.count_selected" => "{selected} selected / {count} items",
            "browser.empty_dir" => "This folder is empty",
            "browser.up" => "Up one level",
            "browser.root" => "Root",
            "browser.up_hint" => "Backspace does the same",
            "browser.drop_title" => "Drop an archive here",
            "browser.drop_hint" => "or use the button below to open one",
            "extract.title" => "Extract to",
            "extract.dest" => "Destination folder",
            "extract.browse" => "Browse…",
            "extract.overwrite" => "Overwrite existing files",
            "extract.start" => "Extract",
            "extract.all_hint" => "Nothing selected: extract the whole archive",
            "extract.cancel" => "Cancel",
            "extract.success" => "Extraction complete",
            "error.title" => "Error",
            "error.engine" => "Operation failed",
            "error.io" => "Read/write failed (file in use, invalid path, or out of disk space)",
            "error.not_an_archive" => "Not a valid archive",
            "error.password_required" => "Password required",
            "error.password_incorrect" => "Wrong password",
            "error.password_unsupported" => "This archive does not support passwords",
            "error.cancelled" => "Cancelled",
            "error.security_blocked" => "Entry name is unsafe and was skipped",
            "error.quota_exceeded" => "Extracted content exceeds the size limit",
            "error.dll_missing" => "Missing unpacking engine (7z.dll)",
            "error.settings.invalid" => "Invalid settings",
            "error.shell.dev_path" => "A development build cannot be registered as the default handler. Use the installed ZipNest.",
            "error.shell.associate" => "Failed to register file associations",
            "error.shell.file_menu" => "Failed to register the file context menu",
            "error.shell.directory_menu" => "Failed to register the folder context menu",
            "error.shell.background_menu" => "Failed to register the folder background menu",
            "settings.title" => "Settings",
            "settings.back" => "← Back",
            "settings.donate" => "Support us",
            "settings.donate.title" => "Support ZipNest",
            "settings.donate.body" => "ZipNest is completely free. If it saved you time, your support is the best reward.",
            "settings.donate.wechat" => "WeChat Pay",
            "settings.donate.alipay" => "Alipay",
            "create.title" => "New archive",
            "create.sources" => "Files to add",
            "create.add_files" => "Add files…",
            "create.add_folder" => "Add folder…",
            "create.dest" => "Output archive",
            "create.dest_hint" => "Click Browse… below to choose where to save it",
            "create.format" => "Format",
            "create.level" => "Level",
            "create.level.store" => "Store",
            "create.level.fastest" => "Fastest",
            "create.level.normal" => "Normal",
            "create.level.maximum" => "Maximum",
            "create.level.ultra" => "Ultra",
            "create.method" => "Method",
            "create.method.auto" => "Auto",
            "create.method.copy" => "Copy (no compression)",
            "create.method.deflate" => "Deflate",
            "create.method.lzma2" => "LZMA2",
            "create.method.bzip2" => "BZip2",
            "create.volume" => "Split size",
            "create.volume.off" => "Off",
            "create.volume.10m" => "10 MB",
            "create.volume.100m" => "100 MB",
            "create.volume.1g" => "1 GB",
            "create.volume.custom" => "Custom",
            "create.password" => "Password",
            "create.encrypt_names" => "Encrypt file names (7z only)",
"create.sfx" => "Self-extracting (7z only)",
            "create.start" => "Create",
            "pick.all" => "All files",
            "pick.archives" => "Archives",
            "format.zip" => "ZIP archive (*.zip)",
            "format.7z" => "7z archive (*.7z)",
            "format.tar" => "TAR archive (*.tar)",
            "format.tar.gz" => "TAR.GZ archive (*.tar.gz)",
            "format.tar.bz2" => "TAR.BZ2 archive (*.tar.bz2)",
            "format.tar.xz" => "TAR.XZ archive (*.tar.xz)",
            "help.title" => "Help",
            "help.format" => "Archive format. 7z squeezes the most; zip opens everywhere; TAR is common on Unix/Linux and only packs (GZ/BZ2/XZ add compression).",
            "help.level" => "Compression level: higher = smaller file, slower & more CPU.\n· Store: no compression, fastest\n· Fastest: light compression, fast\n· Normal: balanced (recommended)\n· Maximum: smaller, slower\n· Ultra: smallest, slowest",
            "help.method" => "Algorithm behind compression: Auto picks per format, Copy stores as-is, Deflate is zip-classic, LZMA2 is best for 7z, BZip2 compresses well but slower.",
            "help.password" => "Locks the archive with a password (7z uses AES-256). Leave empty for no encryption.",
            "help.encrypt_names" => "Also hides file names inside the archive (7z only).",
            "help.volume" => "Splits into smaller parts for copying/uploading: 10/100 MB, 1 GB, or custom like 100m/1g. Off = single file.",
            "job.canceled" => "Cancel",
            "job.ok" => "Done",
            "job.ok_skipped" => "Done ({count} entries skipped)",
            "job.close" => "Close",
            "preview.title" => "Preview",
            "preview.no_preview" => "No preview available",
            "preview.truncated" => "Preview truncated (file is large)",
            "settings.language" => "Language",
            "settings.theme" => "Appearance",
            "settings.theme.system" => "System",
            "settings.theme.light" => "Light",
            "settings.theme.dark" => "Dark",
            "settings.section.general" => "General",
            "settings.section.extract" => "Extraction",
            "settings.section.integration" => "Windows integration",
            "settings.section.about" => "About",
            "settings.section.feedback" => "Help & feedback",
            "settings.feedback.hint" => "Something not working? Copy the diagnostics first, then paste them into an email. The text only holds the version, your Windows build, where the program lives, a few toggle states and the last error code — no file names and no archive contents.",
            "settings.feedback.copy" => "Copy diagnostics",
            "settings.feedback.copied" => "Copied to clipboard",
            "settings.feedback.email" => "Feedback email",
            "settings.feedback.copy_email" => "Copy address",
            "settings.feedback.mail" => "Send feedback by email",
            "settings.feedback.mail_subject" => "problem report",
            "settings.default_dir.hint" => "Empty = a folder named after the archive, next to it",
            "settings.language.system" => "System",
            "settings.language.zh" => "Simplified Chinese",
            "settings.language.en" => "English",
            "settings.default_dir" => "Default extract folder",
            "settings.overwrite" => "Overwrite policy",
            "settings.ui_zoom" => "UI zoom",
            "settings.ui_zoom.percent" => "{percent}%",
            "settings.max_extract" => "Extract size limit (MB)",
            "settings.max_extract.hint" => "Extraction stops once a single run writes more than this — the zip-bomb guard. Default: 65536 MB.",
            "settings.auto_update" => "Check for updates on startup",
            "settings.auto_close" => "Close the window when a job finishes",
            "settings.auto_close.hint" => "Close the window by itself after a create or extract job. Its completion message and output path go with it, so this is off by default.",
            "settings.version" => "Current version",
            "settings.overwrite.hint" => "When a file already exists",
            "settings.overwrite.ask" => "Ask me",
            "settings.overwrite.overwrite" => "Overwrite",
            "settings.overwrite.skip" => "Skip",
            "settings.overwrite.rename" => "Rename automatically",
            "settings.shell.associate" => "Open archives with ZipNest by default",
            "settings.shell.assoc_hint" => "Takes over the default handler for ZIP / 7Z / RAR / TAR / GZ / BZ2 / XZ / ISO on install; unticking hands them straight back.",
            "settings.shell.blocked" => "Windows has another default app recorded for {exts}, so it needs changing back once — the button below does that, with no trip to Windows Settings.",
            "settings.shell.claim_ok" => "Done — archives open with ZipNest by default now",
            "settings.shell.claim_partial" => "Most formats were taken over; Windows still protects the rest",
            "settings.shell.set_default" => "Make ZipNest the default",
            "settings.shell.set_default.hint" => "Takes effect immediately, no restart. You can still hand any format to another program in Windows' own Default apps page later.",
            "settings.shell.status_ours" => "Current: ZipNest is the default for archives",
            "settings.shell.status_off" => "Current: default associations are switched off (the box above is unticked)",
            "settings.shell.context_menu" => "Context menu",
            "settings.save" => "Save",
            "update.check" => "Check for updates",
            "update.title" => "Update",
            "update.checking" => "Checking for updates…",
            "update.found" => "A new version is available",
            "update.download" => "Download",
            "update.hint" => "Clicking Download opens the GitHub releases page in your browser; download the installer and run it to update.",
"update.ready" => "New version downloaded. Install will exit and install silently (no console window); reopening after install gives you the new version.",
            "update.install" => "Install now",
            "update.failed" => "Update check failed",
        }
    });

#[cfg(test)]
mod tests {
    use super::{EN, ZH};
    use std::ops::Deref;

    /// Every error key the Rust side can hand to the UI (engine constants from
    /// `archive-core/src/error.rs`, the IPC layer's own keys, and the shell
    /// integration warnings). `tr()` falls back to printing the key itself, so
    /// a missing one shows the user `error.io` — this list is the guard.
    const ERROR_KEYS: [&str; 17] = [
        "error.cancelled",
        "error.dll_missing",
        "error.engine",
        "error.io",
        "error.not_an_archive",
        "error.password_incorrect",
        "error.password_required",
        "error.password_unsupported",
        "error.quota_exceeded",
        "error.security_blocked",
        "error.settings.invalid",
        "error.shell.associate",
        "error.shell.background_menu",
        "error.shell.dev_path",
        "error.shell.directory_menu",
        "error.shell.file_menu",
        "error.title",
    ];

    #[test]
    fn both_tables_carry_exactly_the_same_keys() {
        // A key added to one language only used to be invisible: the missing
        // side silently rendered the raw key.
        let zh = ZH.deref();
        let en = EN.deref();
        let mut only_zh: Vec<_> = zh.keys().filter(|k| !en.contains_key(*k)).collect();
        let mut only_en: Vec<_> = en.keys().filter(|k| !zh.contains_key(*k)).collect();
        only_zh.sort();
        only_en.sort();
        assert!(only_zh.is_empty(), "only in zh-CN: {only_zh:?}");
        assert!(only_en.is_empty(), "only in en-US: {only_en:?}");
        assert!(zh.len() > 100, "the table looks truncated: {} keys", zh.len());
    }

    #[test]
    fn every_error_key_is_translated_in_both_languages() {
        for key in ERROR_KEYS {
            assert!(ZH.deref().contains_key(key), "zh-CN is missing {key}");
            assert!(EN.deref().contains_key(key), "en-US is missing {key}");
        }
    }

    #[test]
    fn the_dynamically_built_keys_all_exist() {
        // These are assembled with `format!` at the call site, so a typo or a
        // removed option shows up as a raw key rather than a compile error.
        for level in ["store", "fastest", "normal", "maximum", "ultra"] {
            let key = format!("create.level.{level}");
            assert!(ZH.deref().contains_key(key.as_str()), "zh-CN lacks {key}");
            assert!(EN.deref().contains_key(key.as_str()), "en-US lacks {key}");
        }
        for method in ["auto", "copy", "deflate", "lzma2", "bzip2"] {
            let key = format!("create.method.{method}");
            assert!(ZH.deref().contains_key(key.as_str()), "zh-CN lacks {key}");
            assert!(EN.deref().contains_key(key.as_str()), "en-US lacks {key}");
        }
        for volume in ["off", "10m", "100m", "1g", "custom"] {
            let key = format!("create.volume.{volume}");
            assert!(ZH.deref().contains_key(key.as_str()), "zh-CN lacks {key}");
            assert!(EN.deref().contains_key(key.as_str()), "en-US lacks {key}");
        }
        for policy in ["ask", "overwrite", "skip", "rename"] {
            let key = format!("settings.overwrite.{policy}");
            assert!(ZH.deref().contains_key(key.as_str()), "zh-CN lacks {key}");
            assert!(EN.deref().contains_key(key.as_str()), "en-US lacks {key}");
        }
        for theme in ["system", "light", "dark"] {
            let key = format!("settings.theme.{theme}");
            assert!(ZH.deref().contains_key(key.as_str()), "zh-CN lacks {key}");
            assert!(EN.deref().contains_key(key.as_str()), "en-US lacks {key}");
        }
        // The file-dialog filters (new in v0.4.11: they used to be English-only
        // literals in main.rs).
        for format in ["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"] {
            let key = format!("format.{format}");
            assert!(ZH.deref().contains_key(key.as_str()), "zh-CN lacks {key}");
            assert!(EN.deref().contains_key(key.as_str()), "en-US lacks {key}");
        }
    }

    #[test]
    fn a_missing_key_still_comes_back_as_itself() {
        // Documented fallback: better a visible key than an empty label.
        assert_eq!(super::tr("zh-CN", "nope.not.here"), "nope.not.here");
        assert_eq!(super::tr("en-US", "nope.not.here"), "nope.not.here");
    }
}


