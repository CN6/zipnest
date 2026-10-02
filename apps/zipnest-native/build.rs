//! Windows resource embedding: gives zipnest.exe its icon, version metadata,
//! and a real window title. Without this, Explorer/taskbar/shortcuts would
//! fall back to a generic (blank) icon.

fn main() {
    #[cfg(windows)]
    {
        embed_resource::compile("src/assets/app.rc", embed_resource::NONE);
    }
}