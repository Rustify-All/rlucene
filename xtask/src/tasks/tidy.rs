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
use crate::{log, run_cargo};

pub(crate) fn run() {
    super::license_check::run();
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
