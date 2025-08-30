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
use crate::log;
use std::process::{self, Command};

pub(crate) fn run() {
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
