//! Run recovery regression tests without launching Hebnix or changing the PC:
//! cargo test -p hebnix-app --example proxy_recovery_tests
#![allow(dead_code)]

#[path = "../src/hosts_file.rs"]
mod hosts_file;
#[path = "../src/recovery_tasks.rs"]
mod recovery_tasks;

fn main() {}
