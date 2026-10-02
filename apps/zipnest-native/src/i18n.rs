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
            "extract.title" => "解压到",
            "extract.dest" => "目标文件夹",
            "extract.browse" => "浏览…",
            "extract.overwrite" => "覆盖已存在文件",
            "extract.start" => "开始解压",
            "extract.cancel" => "取消",
            "extract.success" => "解压完成",
            "error.title" => "错误",
            "error.shell.dev_path" => "开发版路径不能注册为默认打开程序，请使用安装后的 ZipNest。",
            "settings.title" => "设置",
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
            "create.method" => "压缩方法",
            "create.password" => "密码",
            "create.encrypt_names" => "加密文件名（仅 7z）",
            "create.volume" => "分卷大小",
            "create.sfx" => "自解压可执行文件（仅 7z）",
            "create.start" => "开始创建",
            "help.format" => "压缩包格式。7z 压缩率最高；zip 兼容性最好（任何系统都能直接打开）；TAR 系列常用于 Unix/Linux，TAR 本身不压缩，配合 GZ/BZ2/XZ 才压缩。",
            "help.level" => "压缩等级：等级越高，压缩后文件越小，但打包越慢、越费 CPU。\n· 仅存储：不压缩，原样打包，最快\n· 最快：轻度压缩，速度快\n· 标准：速度和体积均衡（推荐）\n· 最大：压缩更小，速度较慢\n· 极致：体积最小，速度最慢",
            "help.method" => "压缩算法，影响压缩率和速度：\n· 自动：让程序按格式选最合适的\n· 复制：不压缩直接存（用于已压缩的文件）\n· Deflate：ZIP 常用，兼容性好\n· LZMA2：7z 常用，压缩率更高\n· BZip2：压缩率较高，速度较慢",
            "help.password" => "设置密码后，打开和解压这个压缩包都需要密码。留空表示不加密。7z 格式支持 AES-256 强加密。",
            "help.encrypt_names" => "勾选后，压缩包内的文件名也会被加密，看不到内容列表。只有 7z 格式支持。",
            "help.volume" => "把大文件拆成多个指定大小的小分卷，方便复制或上传。\n· 10 MB / 100 MB / 1 GB：固定分卷大小\n· 自定义：按你输入的体积分卷，例如 100m 或 1g\n· 关闭：不拆分，打成一个文件",
            "job.canceled" => "取消",
            "job.ok" => "完成",
            "preview.title" => "预览",
            "preview.no_preview" => "无法预览此文件",
            "settings.language" => "语言",
            "settings.language.system" => "跟随系统",
            "settings.language.zh" => "简体中文",
            "settings.language.en" => "English",
            "settings.default_dir" => "默认解压目录",
            "settings.overwrite" => "覆盖策略",
            "settings.ui_zoom" => "界面缩放",
            "settings.ui_zoom.percent" => "{percent}%",
            "settings.overwrite.hint" => "当目标文件已存在时",
            "settings.overwrite.ask" => "询问我",
            "settings.overwrite.overwrite" => "覆盖",
            "settings.overwrite.skip" => "跳过",
            "settings.overwrite.rename" => "自动重命名",
            "settings.shell.associate" => "默认用 ZipNest 打开压缩包",
            "settings.shell.context_menu" => "右键菜单",
            "settings.save" => "保存",
            "update.check" => "检查更新",
            "update.title" => "检查更新",
            "update.checking" => "正在检查更新…",
            "update.found" => "发现新版本",
            "update.download" => "下载并安装",
            "update.ready" => "新版本已下载。点击安装将自动关闭当前版本并覆盖更新。",
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
            "extract.title" => "Extract to",
            "extract.dest" => "Destination folder",
            "extract.browse" => "Browse…",
            "extract.overwrite" => "Overwrite existing files",
            "extract.start" => "Extract",
            "extract.cancel" => "Cancel",
            "extract.success" => "Extraction complete",
            "error.title" => "Error",
            "error.shell.dev_path" => "A development build cannot be registered as the default handler. Use the installed ZipNest.",
            "settings.title" => "Settings",
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
            "create.method" => "Method",
            "create.password" => "Password",
            "create.encrypt_names" => "Encrypt file names (7z only)",
            "create.volume" => "Split into volumes",
"create.sfx" => "Self-extracting (7z only)",
            "create.start" => "Create",
            "help.format" => "Archive format. 7z squeezes the most; zip opens everywhere; TAR is common on Unix/Linux and only packs (GZ/BZ2/XZ add compression).",
            "help.level" => "Compression level: higher = smaller file, slower & more CPU.\n· Store: no compression, fastest\n· Fastest: light compression, fast\n· Normal: balanced (recommended)\n· Maximum: smaller, slower\n· Ultra: smallest, slowest",
            "help.method" => "Algorithm behind compression: Auto picks per format, Copy stores as-is, Deflate is zip-classic, LZMA2 is best for 7z, BZip2 compresses well but slower.",
            "help.password" => "Locks the archive with a password (7z uses AES-256). Leave empty for no encryption.",
            "help.encrypt_names" => "Also hides file names inside the archive (7z only).",
            "help.volume" => "Splits into smaller parts for copying/uploading: 10/100 MB, 1 GB, or custom like 100m/1g. Off = single file.",
            "job.canceled" => "Cancel",
            "job.ok" => "Done",
            "preview.title" => "Preview",
            "preview.no_preview" => "No preview available",
            "settings.language" => "Language",
            "settings.language.system" => "System",
            "settings.language.zh" => "Simplified Chinese",
            "settings.language.en" => "English",
            "settings.default_dir" => "Default extract folder",
            "settings.overwrite" => "Overwrite policy",
            "settings.ui_zoom" => "UI zoom",
            "settings.ui_zoom.percent" => "{percent}%",
            "settings.overwrite.hint" => "When a file already exists",
            "settings.overwrite.ask" => "Ask me",
            "settings.overwrite.overwrite" => "Overwrite",
            "settings.overwrite.skip" => "Skip",
            "settings.overwrite.rename" => "Rename automatically",
            "settings.shell.associate" => "Open archives with ZipNest by default",
            "settings.shell.context_menu" => "Context menu",
            "settings.save" => "Save",
            "update.check" => "Check for updates",
            "update.title" => "Update",
            "update.checking" => "Checking for updates…",
            "update.found" => "A new version is available",
            "update.download" => "Download & install",
            "update.ready" => "New version downloaded. Install will close this version and overwrite it.",
            "update.install" => "Install now",
            "update.failed" => "Update check failed",
        }
    });


