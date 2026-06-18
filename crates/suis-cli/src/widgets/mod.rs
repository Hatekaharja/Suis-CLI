//! Reusable rendering widgets for the chat UI.
//!
//! Each widget is a stateless `render` function taking a [`Frame`](ratatui::Frame),
//! a target [`Rect`](ratatui::layout::Rect), and the data it draws. Display
//! models that carry their own logic ([`message_list::ChatMessage`],
//! [`permission_prompt::PermissionPrompt`], [`diff_viewer::DiffLineKind`]) live
//! alongside their renderers.

pub mod command_palette;
pub mod confirm_box;
pub mod context_gauge;
pub mod diff_viewer;
pub mod footer;
pub mod input_box;
pub mod list_frame;
pub mod md;
pub mod message_list;
pub mod notice_popup;
pub mod permission_prompt;
pub mod plan_review;
pub mod task_panel;
pub mod usage_popup;
pub mod welcome;
