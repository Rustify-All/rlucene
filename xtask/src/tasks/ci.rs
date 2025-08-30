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
use crate::{LogColor, colorize, log, run_cargo};

pub(crate) fn run() {
    super::tidy::run();
    super::check_uncommitted::run();
    log(&colorize("Running Cargo test ", LogColor::Green, true));
    run_cargo(&["test", "--verbose", "--features", "test_log_verbose"]);
    log(&colorize(
        "✅ ✅ ✅ Finished Cargo test ",
        LogColor::Green,
        true,
    ));
}
