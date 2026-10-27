#![forbid(unsafe_code)]

pub mod accelerator;
pub mod blocker;
pub mod bookmarks;
pub mod codex_store;
pub mod commands;
pub mod downloads;
pub mod extensions;
pub mod geometry;
pub mod icon;
pub mod ids;
pub mod injection;
pub mod item;
pub mod items;
pub mod layout;
pub mod macos;
pub mod navigation;
/// Markdown notes the person owns, described by an index built from the files.
pub mod notes;
pub mod page_choice;
pub mod permissions;
pub mod ports;
pub mod profiles;
pub mod runtime_security;
pub mod search;
pub mod session;
pub mod spaces;
pub mod split;
/// Time on the web and focus sessions.
pub mod time;
pub mod userscripts;
pub mod webkitgtk;
pub mod webview2;
pub mod windows;
pub mod work;

/// Allowlisted application appearance preferences.
pub mod preferences;
/// Colour themes and the window ground native derives from them.
pub mod theme;

/// Durable user-authored resources shared by Browse and Work.
pub mod resources;
