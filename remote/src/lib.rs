mod access;
mod cli;
mod fleet;
pub mod host;
mod hub;
mod limits;
mod lock;
mod ops;
mod server;
mod service;
mod settings;

use local_studio_core::link::{devices, noise, pairing};

pub use cli::cli;

const VERSION: &str = env!("LOCAL_STUDIO_BUILD_VERSION");
const CHANNEL: &str = env!("LOCAL_STUDIO_BUILD_CHANNEL");
