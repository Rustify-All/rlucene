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
use crate::tasks::license::license_check;
use crate::{LogColor, colorize, log, run_cargo};

pub(crate) fn run() {
    license_check::run();
    log(&colorize("Running Cargo clippy ", LogColor::Green, true));
    run_cargo(&[
        "clippy",
        "--fix",
        "--all-targets",
        "--all-features",
        "--allow-dirty",
        "--allow-staged",
    ]);
    log(&colorize("Finished Cargo clippy ", LogColor::Green, true));
    log(&colorize("Running Cargo fix", LogColor::Green, true));
    run_cargo(&[
        "fix",
        "--all-targets",
        "--all-features",
        "--allow-dirty",
        "--allow-staged",
    ]);
    log(&colorize("Finished Cargo fix", LogColor::Green, true));
    log(&colorize("Running Cargo fmt ", LogColor::Green, true));
    run_cargo(&["fmt"]);
    log(&colorize("Finished Cargo fmt ", LogColor::Green, true));
    log(&colorize(
        "✅ ✅ ✅ Finished Cargo tidy",
        LogColor::Green,
        true,
    ));
}
