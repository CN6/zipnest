//! IPC service layer between the Tauri glue and the archive engine.
//!
//! Framework-free on purpose: the Tauri app wires an emit callback, tests
//! wire a recording vector. No file-system access beyond what the engine
//! performs through [`IpcService`].

pub mod error;
pub mod registry;
pub mod service;
pub mod settings;
pub mod shell;

pub use error::IpcError;
pub use service::{CreateRequest, EntryDto, IpcService, OpenArchiveResult};
pub use settings::{Settings, SettingsPatch, SettingsStore};
pub use shell::{ShellApplier, ShellOptions};
