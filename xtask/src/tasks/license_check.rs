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
use crate::{find_file, log};
use std::{env, process};

pub(crate) fn run() {
    let project_dir = env::current_dir().unwrap();
    let xtask_dir = project_dir.join("xtask");

    let license_path = find_file(&xtask_dir, "LICENSE_HEADER");
    let license_header_path: String = license_path.as_ref().unwrap().to_str().unwrap().to_string();
    if license_path.is_none() {
        log("LICENSE_HEADER file not found: LICENSE_HEADER");
        process::exit(1);
    }

    let license_text =
        super::license::license_checker::load_license_text(license_path.unwrap().as_path());

    let src_dir = project_dir.join("src");
    let libs_dir = project_dir.join("libs");
    let examples_dir = project_dir.join("examples");

    let src_valid = super::license::license_checker::check_licenses_in_dir(&src_dir, &license_text);
    let libs_valid =
        super::license::license_checker::check_licenses_in_dir(&libs_dir, &license_text);
    let xtask_valid =
        super::license::license_checker::check_licenses_in_dir(&xtask_dir, &license_text);
    let examples_valid =
        super::license::license_checker::check_licenses_in_dir(&examples_dir, &license_text);

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
