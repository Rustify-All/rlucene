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

pub(crate) fn find_file(dir: &Path, file_name: &str) -> Option<PathBuf> {
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

pub(crate) fn run_cargo(args: &[&str]) {
    let status = Command::new("cargo")
        .args(args)
        .status()
        .expect("failed to run cargo");
    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
}

pub(crate) fn log(msg: &str) {
    let now = Local::now();
    eprintln!("[{}] {}", now.format("%Y-%m-%d %H:%M:%S"), msg);
}

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("tidy") => tasks::tidy::run(),
        Some("commit") => tasks::commit::run(),
        Some("ci") => tasks::ci::run(),
        Some("check-uncommitted") => tasks::check_uncommitted::run(),
        Some("license-check") => tasks::license_check::run(),
        _ => {
            log(
                "Available commands: tidy, commit, ci, check-uncommitted, check-rust-version, license-check",
            );
            process::exit(1);
        },
    }
}
