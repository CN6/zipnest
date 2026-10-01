//! Tiny i18n: zh/en string tables, Chinese font loading, and image decode.

use eframe::egui;
use std::collections::HashMap;
use std::ops::Deref;

pub fn os_language() -> &'static str {
    // Windows returns e.g. "zh-CN" in GetUserDefaultUILanguage via env? Not set;
    // fall back to zh by default for this user, overridden by settings later.
    "zh-CN"
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for name in ["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"] {
        let path = format!("C:\\Windows\\Fonts\\{name}");
        if let Ok(bytes) = std::fs::read(&path) {
            fonts
                .font_data
                .insert("zh".to_owned(), egui::FontData::from_owned(bytes));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("zh".to_owned());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
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
            "create.method" => "压缩方法",
            "create.password" => "密码",
            "create.encrypt_names" => "加密文件名（仅 7z）",
            "create.volume" => "分卷大小",
            "create.sfx" => "自解压可执行文件（仅 7z）",
            "create.start" => "开始创建",
            "job.canceled" => "取消",
            "job.ok" => "完成",
            "preview.title" => "预览",
            "preview.no_preview" => "无法预览此文件",
            "settings.language.system" => "跟随系统",
            "settings.language.zh" => "简体中文",
            "settings.language.en" => "English",
            "settings.default_dir" => "默认解压目录",
            "settings.overwrite.ask" => "询问",
            "settings.overwrite.overwrite" => "覆盖",
            "settings.overwrite.skip" => "跳过",
            "settings.overwrite.rename" => "自动重命名",
            "settings.shell.associate" => "默认用 ZipNest 打开压缩包",
            "settings.shell.context_menu" => "右键菜单",
            "settings.save" => "保存",
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
            "create.method" => "Method",
            "create.password" => "Password",
            "create.encrypt_names" => "Encrypt file names (7z only)",
            "create.volume" => "Split into volumes",
            "create.sfx" => "Self-extracting (7z only)",
            "create.start" => "Create",
            "job.canceled" => "Cancel",
            "job.ok" => "Done",
            "preview.title" => "Preview",
            "preview.no_preview" => "No preview available",
            "settings.language.system" => "System",
            "settings.language.zh" => "Simplified Chinese",
            "settings.language.en" => "English",
            "settings.default_dir" => "Default extract folder",
            "settings.overwrite.ask" => "Ask",
            "settings.overwrite.overwrite" => "Overwrite",
            "settings.overwrite.skip" => "Skip",
            "settings.overwrite.rename" => "Rename",
            "settings.shell.associate" => "Open archives with ZipNest by default",
            "settings.shell.context_menu" => "Context menu",
            "settings.save" => "Save",
        }
    });
