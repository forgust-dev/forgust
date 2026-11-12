//! Forgust server: one binary with the api, worker and storage roles (ADR-0012).

// large `json!` literals (the GitLab-shaped objects in `v4`)
#![recursion_limit = "256"]

pub mod api;
pub mod app;
pub mod auth;
pub mod backup;
pub mod blob;
pub mod ci;
pub mod config;
pub mod crypto;
pub mod error;
pub mod events;
pub mod git_http;
pub mod hooks;
pub mod import;
pub mod jobs;
pub mod languages;
pub mod lfs;
pub mod mcp;
pub mod oauth;
pub mod pages;
pub mod registry;
pub mod render;
pub mod services;
pub mod ssh;
pub mod state;
pub mod telemetry;
pub mod v4;
pub mod web;
