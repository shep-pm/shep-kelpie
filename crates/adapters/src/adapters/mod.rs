//! The real ports: `gh`, `curl`, the GitHub App's calls,
//! the `claude` command line, its `/usage`, the local review
//! round, the machine's sandbox programs, and the dog's
//! leases over the shepherd channel

pub(crate) mod bridge;
mod classifier;
mod claude;
mod cli;
mod codex;
mod codex_usage;
mod curl;
mod dev_server;
mod disk;
mod e2e_runner;
mod files;
pub mod forwarder;
mod gateway_ports;
pub mod gh;
mod git;
mod github;
pub(crate) mod goose;
mod gpu;
pub(crate) mod harness;
mod harness_mcp;
pub mod host;
mod leases;
mod local;
pub(crate) mod mcp;
mod ntfy;
pub(crate) mod openai_gateway;
pub(crate) mod paddock;
mod pages;
mod pi;
mod process;
mod skills;
mod srt;
mod stop;
mod test_runner;
mod unreachable;
mod usage;

pub use crate::lease::door_pace::DoorPace;
pub use crate::lease::door_pick::DoorBoard;
pub use crate::metrics::ShepherdSink;
pub use bridge::PlaywrightBridges;
pub use classifier::GatewayClassifier;
pub use claude::settings::settings as claude_settings;
#[cfg(any(test, feature = "test-support"))]
pub use claude::write_settings as write_claude_settings;
pub use claude::{ClaudeCli, LOGIN_FIX, LOGIN_VAR, LoginToken};
pub use cli::{Cli, CliAdapter, FollowUp, Launch, Served, Turn};
pub use codex::CodexCli;
pub use codex_usage::CodexMeter;
pub use curl::Curl;
pub use dev_server::SandboxedServers;
pub use disk::StatDisk;
pub use e2e_runner::DogE2e;
pub use gateway_ports::{port, port_named};
pub use gh::{Etags, Gh, GhApps};
pub use git::SystemGit;
pub use github::{CurlGithub, Openssl};
pub use goose::{GOOSE_MINIMUM, GooseCli};
pub use gpu::GpuCurl;
pub use harness::fence_policy;
pub use harness::{Base, LambLabels};
pub use harness_mcp::McpRules;
pub use host::SystemHost;
pub use leases::{Notes, ShepLeases};
pub use local::LocalReviewer;
pub use pages::PlaywrightPages;
pub use pi::{PI_MINIMUM, PiCli};
pub use process::{Sealed, end_groups, feeding};
pub use srt::SandboxRuntime;
#[cfg(any(test, feature = "test-support"))]
pub use srt::srt_settings;
pub use stop::stop_calls;
pub use test_runner::SandboxedTests;
pub use usage::UsageMeter;
