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
use crate::{LogColor, colorize, log};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

pub(crate) fn run() {
  let project_dir = env::current_dir().unwrap();
  let xtask_dir = project_dir.join("xtask");

  let license_path = find_file(&xtask_dir, "LICENSE_HEADER");
  let license_header_path: String = license_path.as_ref().unwrap().to_str().unwrap().to_string();
  if license_path.is_none() {
    log("LICENSE_HEADER file not found: LICENSE_HEADER");
    process::exit(1);
  }

  let license_text = load_license_text(license_path.unwrap().as_path());

  let src_dir = project_dir.join("src");

  let src_valid = check_licenses_in_dir(&src_dir, &license_text);
  let xtask_valid = check_licenses_in_dir(&xtask_dir, &license_text);

  if src_valid && xtask_valid {
    log(&colorize(
      "✅ ✅ ✅ All files have the correct license header",
      LogColor::Green,
      true,
    ));
  } else {
    log(&format!(
      "❌ ❌ ❌ License check failed: you should copy the correct license header from {}",
      colorize(&license_header_path, LogColor::Red, false)
    ));
    process::exit(1);
  }
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
  pub(crate) fn load_license_text(file_path: &Path) -> String {
    fs::read_to_string(file_path).expect("Failed to read license file")
  }

  fn check_license_in_file(file_path: &Path, license_text: &str) -> bool {
    let content = fs::read_to_string(file_path).expect("Unable to read file");
    content.starts_with(license_text)
  }

  pub fn check_licenses_in_dir(dir: &Path, license_text: &str) -> bool {
    let mut all_valid = true;

    for entry in fs::read_dir(dir).expect("Unable to read directory") {
      let entry = entry.expect("Invalid entry");
      let path = entry.path();

      if path.is_dir() {
        all_valid &= check_licenses_in_dir(&path, license_text);
      } else if path.extension().map(|ext| ext == "rs").unwrap_or(false)
        && !check_license_in_file(&path, license_text)
      {
        println!(
          "Missing or incorrect license in file: {}",
          colorize(&path.display().to_string(), LogColor::Red, false)
        );
        all_valid = false;
      }
    }
    all_valid
  }
}
