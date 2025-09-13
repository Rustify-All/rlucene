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
pub trait DirectoryReader {}

pub mod directory_reader_util {
    use crate::core::index::IndexFileNames;
    use crate::core::store::directory::Directory;
    use crate::core::util::error::lucene_error::Result;

    pub fn index_exists(directory: &impl Directory) -> Result<bool> {
        // LUCENE-2812, LUCENE-2727, LUCENE-4738: this logic will
        // return true in cases that should arguably be false,
        // such as only IW.prepareCommit has been called, or a
        // corrupt first commit, but it's too deadly to make
        // this logic "smarter" and risk accidentally returning
        // false due to various cases like file description
        // exhaustion, access denied, etc., because in that
        // case IndexWriter may delete the entire index.  It's
        // safer to err towards "index exists" than try to be
        // smart about detecting not-yet-fully-committed or
        // corrupt indices.  This means that IndexWriter will
        // throw an exception on such indices and the app must
        // resolve the situation manually:
        let files = directory.list_all()?; // returns Vec<String>

        let prefix = format!("{}_", IndexFileNames::SEGMENTS);
        for file in files {
            if file.starts_with(&prefix) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
