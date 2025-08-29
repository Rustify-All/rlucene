/*
 * Licensed to the Apache Software Foundation (ASF) under one or more
 * contributor license agreements.  See the NOTICE file distributed with
 * this work for additional information regarding copyright ownership.
 * The ASF licenses this file to You under the Apache License, Version 2.0
 * (the "License"); you may not use this file except in compliance with
 * the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
mod tasks;

use chrono::Local;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

fn find_file(dir: &Path, file_name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).expect("Failed to read directory") {
        let entry = entry.expect("Invalid directory entry");
        let path = entry.path();

        if path.is_file() && path.file_name().map_or(false, |name| name == file_name) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file(&path, file_name) {
                return Some(found);
            }
        }
    }
    None
}

fn run_cargo(args: &[&str]) {
    let status = Command::new("cargo")
        .args(args)
        .status()
        .expect("failed to run cargo");
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

fn log(msg: &str) {
    let now = Local::now();
    eprintln!("[{}] {}", now.format("%Y-%m-%d %H:%M:%S"), msg);
}
/// Check if there are uncommitted changes.
fn check_uncommitted() {
    log("\x1b[1;32mRunning uncommitted changes\x1b[0m");
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .expect("failed to execute git");
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        println!("✅ ✅ ✅ Working directory clean. All changes committed.");
    } else {
        log(
            "❌ ❌ ❌ Uncommitted changes detected after code check. Please commit your work again.",
        );
        log(&stdout);
        let diff = Command::new("git")
            .args(["diff"])
            .output()
            .expect("failed to execute git diff");
        log(&String::from_utf8_lossy(&diff.stdout));
        process::exit(1);
    }
    log("\x1b[1;32mFinished uncommitted changes\x1b[0m");
}

/// format code
fn tidy() {
    license_check();
    log("\x1b[1;32mRunning Cargo clippy \x1b[0m");
    run_cargo(&[
        "clippy",
        "--fix",
        "--all-targets",
        "--all-features",
        "--allow-dirty",
        "--allow-staged",
    ]);
    log("\x1b[1;32mFinished Cargo clippy \x1b[0m");
    log("\x1b[1;32mRunning Cargo fix\x1b[0m");
    run_cargo(&[
        "fix",
        "--all-targets",
        "--all-features",
        "--allow-dirty",
        "--allow-staged",
    ]);
    log("\x1b[1;32mFinished Cargo fix\x1b[0m");
    log("\x1b[1;32mRunning Cargo fmt \x1b[0m");
    run_cargo(&["fmt"]);
    log("\x1b[1;32mFinished Cargo fmt \x1b[0m");
    log("\x1b[1;32m✅ ✅ ✅ Finished Cargo tidy\x1b[0m");
}

/// Before submitting a PR, run this command to format and test the code.
fn commit() {
    tidy();
    check_uncommitted();
    log("\x1b[1;32mRunning Cargo test \x1b[0m");
    run_cargo(&["test"]);
    log("\x1b[1;32m✅ ✅ ✅ Finished Cargo test \x1b[0m");
    log("\x1b[1;32m✅ ✅ ✅ Finished Cargo commit\x1b[0m");
}
/// CI task for Github actions
fn ci() {
    tidy();
    check_uncommitted();
    log("\x1b[1;32mRunning Cargo test \x1b[0m");
    run_cargo(&[
        "test",
        "--verbose",
        "--features",
        "test_log_verbose,nightly",
    ]);
    log("\x1b[1;32m✅ ✅ ✅ Finished Cargo test \x1b[0m");
}

fn license_check() {
    let project_dir = env::current_dir().unwrap();
    let xtask_dir = project_dir.join("xtask");

    let license_path = find_file(&xtask_dir, "LICENSE_HEADER");
    let license_header_path: String = license_path.as_ref().unwrap().to_str().unwrap().to_string();
    if license_path.is_none() {
        log("LICENSE_HEADER file not found: LICENSE_HEADER");
        process::exit(1);
    }

    let license_text =
        tasks::license::license_checker::load_license_text(license_path.unwrap().as_path());

    let src_dir = project_dir.join("src");
    let libs_dir = project_dir.join("libs");
    let examples_dir = project_dir.join("examples");

    let src_valid = tasks::license::license_checker::check_licenses_in_dir(&src_dir, &license_text);
    let libs_valid =
        tasks::license::license_checker::check_licenses_in_dir(&libs_dir, &license_text);
    let xtask_valid =
        tasks::license::license_checker::check_licenses_in_dir(&xtask_dir, &license_text);
    let examples_valid =
        tasks::license::license_checker::check_licenses_in_dir(&examples_dir, &license_text);

    if src_valid && libs_valid && xtask_valid && examples_valid {
        log("\x1b[1;32m✅ ✅ ✅ All files have the correct license header\x1b[0m");
    } else {
        log(&format!(
            "❌ ❌ ❌ License check failed: you should copy the correct license header from \x1b[31m{}\x1b[0m",
            license_header_path
        ));
        process::exit(1);
    }
}

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("tidy") => tidy(),
        Some("commit") => commit(),
        Some("ci") => ci(),
        Some("check-uncommitted") => check_uncommitted(),
        Some("license-check") => license_check(),
        _ => {
            log(
                "Available commands: tidy, commit, ci, check-uncommitted, check-rust-version, license-check",
            );
            process::exit(1);
        },
    }
}
