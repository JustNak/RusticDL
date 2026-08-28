mod bridge;
mod handlers;
mod protocol;
mod server;

#[allow(unused_imports)]
pub use crate::branding::PIPE_NAME;
#[allow(unused_imports)]
pub use bridge::{
    BrowserPrompt, BrowserPromptView, IpcBridge, PromptDecision, DOWNLOAD_PROMPT_TIMEOUT,
};
#[allow(unused_imports)]
pub use protocol::PROTOCOL_VERSION;
pub use server::start_ipc_server;
