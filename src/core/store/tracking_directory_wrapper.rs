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
use crate::core::index::index_reader::Identity;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::filter_directory::FilterDirectory;
use crate::core::util::HasIdentity;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};

/// A delegating Directory that records which files were written to and deleted.
pub struct TrackingDirectoryWrapper<D>
where
    D: Directory,
{
    pub(crate) base: FilterDirectory<D>,
    inner: Mutex<Inner>,
    id: Identity,
}
pub struct Inner {
    pub(crate) created_filenames: HashSet<String>,
}
impl<D> TrackingDirectoryWrapper<D>
where
    D: Directory,
{
    pub fn new(input: D) -> Self {
        let lock = Mutex::new(Inner {
            created_filenames: HashSet::new(),
        });
        TrackingDirectoryWrapper {
            base: FilterDirectory::new(input),
            inner: lock,
            id: Identity::new(),
        }
    }

    pub fn get_created_files(&self) -> &Mutex<Inner> {
        &self.inner
    }
    pub fn take_created_files(&mut self) -> HashSet<String> {
        std::mem::take(&mut self.inner.lock().created_filenames)
    }

    pub fn clear_created_files(&mut self) {
        self.inner.lock().created_filenames.clear();
    }
}

impl<D> Display for TrackingDirectoryWrapper<D>
where
    D: Directory,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "TrackingDirectoryWrapper({})", self.base)
    }
}

impl<D> Closeable for TrackingDirectoryWrapper<D>
where
    D: Directory,
{
    fn close(&mut self) -> Result<()> {
        // TODO
        Ok(())
    }
}

impl<D> HasIdentity for TrackingDirectoryWrapper<D>
where
    D: Directory,
{
    fn identity(&self) -> &Identity {
        &self.id
    }
}

impl<D> Directory for TrackingDirectoryWrapper<D>
where
    D: Directory,
{
    fn list_all(&self) -> Result<Vec<String>> {
        self.base.list_all()
    }

    fn delete_file(&self, name: &str) -> Result<()> {
        self.base.delete_file(name)?;
        self.inner.lock().created_filenames.remove(name);
        Ok(())
    }

    fn file_length(&self, name: &str) -> Result<usize> {
        self.base.file_length(name)
    }

    fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
        let output = self.base.create_output(name, context)?;
        self.inner.lock().created_filenames.insert(name.to_string());
        Ok(output)
    }

    type IndexOutput = <FilterDirectory<D> as Directory>::IndexOutput;

    fn create_temp_output(
        &self,
        prefix: &str,
        suffix: &str,
        context: &IOContext,
    ) -> Result<Self::IndexOutput> {
        self.base.create_temp_output(prefix, suffix, context)
    }

    fn sync(&self, names: &[String]) -> Result<()> {
        self.base.sync(names)
    }

    fn sync_metadata(&self) -> Result<()> {
        self.base.sync_metadata()
    }

    fn rename(&self, source: &str, dest: &str) -> Result<()> {
        self.base.rename(source, dest)?;
        let mut inner = self.inner.lock();
        inner.created_filenames.insert(dest.to_string());
        inner.created_filenames.remove(source);
        drop(inner);
        Ok(())
    }

    type IndexInput = <FilterDirectory<D> as Directory>::IndexInput;

    fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
        self.base.open_input(name, context)
    }

    type Lock = <FilterDirectory<D> as Directory>::Lock;

    fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
        self.base.obtain_lock(name)
    }

    fn copy_from(
        &self,
        from: &impl Directory,
        src: &str,
        dest: &str,
        context: &IOContext,
    ) -> Result<()> {
        self.base.copy_from(from, src, dest, context)?;
        self.inner.lock().created_filenames.insert(dest.to_string());
        Ok(())
    }

    fn delete_files_ignoring_exceptions(&self, files: &[String]) {
        self.base.delete_files_ignoring_exceptions(files);
    }

    fn get_pending_deletions(&self) -> Result<HashSet<String>> {
        self.base.get_pending_deletions()
    }

    fn is_fs_directory(&self) -> bool {
        self.base.is_fs_directory()
    }
}

#[cfg(test)]
mod tests {
    use crate::core::store::directory::Directory;
    use crate::core::store::nio_fs_directory::NIOFSDirectory;
    use crate::core::store::nio_fs_index_input::NIOFSIndexInput;
    use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
    use crate::core::store::{BufferedIndexInput, FSDirectory, NativeFSLockFactory};
    use crate::core::util::error::lucene_error::Result;
    use crate::test::store::base_directory_test_case::BaseDirectoryTestCase;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_directory, new_io_context, random,
    };
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[allow(dead_code)] // for quick search
    struct TestTrackingDirectoryWrapper;

    impl BaseDirectoryTestCase for TestTrackingDirectoryWrapper {
        type Directory = TrackingDirectoryWrapper<FSDirectory<NativeFSLockFactory, NIOFSDirectory>>;
        type Output = BufferedIndexInput<NIOFSIndexInput>;
        fn get_directory(&self, path: PathBuf) -> Result<Self::Directory> {
            // TODO: ByteBuffersDirectory 没有实现
            let sub_directory = NIOFSDirectory::new();
            let dir = FSDirectory::new(path, sub_directory)?;
            Ok(TrackingDirectoryWrapper::new(dir))
        }
    }

    #[test]
    fn test_copy_from() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_copy_from(&mut random)
    }
    #[test]
    fn test_rename() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_rename(&mut random)
    }
    #[test]
    fn test_delete_file() -> Result<()> {
        let test = TestTrackingDirectoryWrapper;
        test.test_delete_file()
    }
    #[test]
    fn test_byte() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_byte(&mut random)
    }
    #[test]
    fn test_short() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_short(&mut random)
    }
    #[test]
    fn test_int() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_int(&mut random)
    }
    #[test]
    fn test_long() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_long(&mut random)
    }
    #[test]
    fn test_aligned_little_endian_longs() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_aligned_little_endian_longs(&mut random)
    }
    #[test]
    fn test_unaligned_little_endian_longs() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_unaligned_little_endian_longs(&mut random)
    }
    #[test]
    fn test_little_endian_longs_underflow() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_little_endian_longs_underflow(&mut random)
    }
    #[test]
    fn test_aligned_ints() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_aligned_ints(&mut random)
    }
    #[test]
    fn test_unaligned_ints() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_unaligned_ints(&mut random)
    }
    #[test]
    fn test_ints_underflow() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_ints_underflow(&mut random)
    }
    #[test]
    fn test_aligned_floats() -> Result<()> {
        let test = TestTrackingDirectoryWrapper;
        test.test_aligned_floats()
    }
    #[test]
    fn test_unaligned_floats() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_unaligned_floats(&mut random)
    }
    #[test]
    fn test_floats_underflow() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_floats_underflow(&mut random)
    }
    #[test]
    fn test_string() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_string(&mut random)
    }
    #[test]
    fn test_vint() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_vint(&mut random)
    }
    #[test]
    fn test_vlong() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_vlong(&mut random)
    }
    #[test]
    fn test_zint() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_zint(&mut random)
    }
    #[test]
    fn test_zlong() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_zlong(&mut random)
    }
    #[test]
    fn test_set_of_strings() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_set_of_strings(&mut random)
    }
    #[test]
    fn test_map_of_strings() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_map_of_strings(&mut random)
    }
    #[test]
    fn test_checksum() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_checksum(&mut random)
    }
    #[test]
    fn test_thread_safety_in_list_all() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_thread_safety_in_list_all(&mut random)
    }
    #[test]
    fn test_file_exists_in_list_after_created() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_file_exists_in_list_after_created(&mut random)
    }
    #[test]
    fn test_seek_to_eof_then_back() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_seek_to_eof_then_back(&mut random)
    }
    #[test]
    fn test_illegal_eof() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_illegal_eof(&mut random)
    }
    #[test]
    fn test_seek_past_eof() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_seek_past_eof(&mut random)
    }
    #[test]
    fn test_slice_out_of_bounds() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_slice_out_of_bounds(&mut random)
    }
    #[test]
    fn test_no_dir() -> Result<()> {
        //TODO
        Ok(())
    }

    #[test]
    fn test_copy_bytes() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_copy_bytes(&mut random)
    }
    #[test]
    fn test_copy_bytes_with_threads() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_copy_bytes_with_threads(&mut random)
    }
    #[test]
    fn test_fsync_doesnt_create_new_files() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_fsync_doesnt_create_new_files(&mut random)
    }
    #[test]
    fn test_random_long() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_random_long(&mut random)
    }
    #[test]
    fn test_random_int() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_random_int(&mut random)
    }
    #[test]
    fn test_random_short() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_random_short(&mut random)
    }
    #[test]
    fn test_random_byte() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_random_byte(&mut random)
    }
    #[test]
    fn test_slice_of_slice() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_slice_of_slice(&mut random)
    }
    #[test]
    fn test_large_writes() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_large_writes(&mut random)
    }
    #[test]
    fn test_index_output_to_string() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_index_output_to_string(&mut random)
    }
    #[test]
    fn test_create_temp_output() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_create_temp_output(&mut random)
    }
    #[test]
    fn test_create_output_for_existing_file() -> Result<()> {
        let test = TestTrackingDirectoryWrapper;
        test.test_create_output_for_existing_file()
    }
    #[test]
    fn test_seek_to_end_of_file() -> Result<()> {
        let test = TestTrackingDirectoryWrapper;
        test.test_seek_to_end_of_file()
    }
    #[test]
    fn test_seek_beyond_end_of_file() -> Result<()> {
        let test = TestTrackingDirectoryWrapper;
        test.test_seek_beyond_end_of_file()
    }
    #[test]
    fn test_pending_deletions() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_pending_deletions(&mut random)
    }
    #[test]
    fn test_list_all_is_sorted() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_list_all_is_sorted(&mut random)
    }
    #[test]
    fn test_data_types() -> Result<()> {
        let test = TestTrackingDirectoryWrapper;
        test.test_data_types()
    }
    #[test]
    fn test_group_vint_overflow() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_group_vint_overflow(&mut random)
    }
    #[test]
    fn test_group_vint() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_group_vint(&mut random)
    }
    #[test]
    fn test_prefetch() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_prefetch(&mut random)
    }
    #[test]
    fn test_prefetch_on_slice() -> Result<()> {
        let mut random = random();
        let test = TestTrackingDirectoryWrapper;
        test.test_prefetch_on_slice(&mut random)
    }
    #[test]
    fn test_is_loaded() -> Result<()> {
        //TODO
        Ok(())
    }
    #[test]
    fn test_is_loaded_on_slice() -> Result<()> {
        //TODO
        Ok(())
    }

    #[test]
    fn test_track_empty() -> Result<()> {
        // TODO: ByteBuffersDirectory 没有实现
        let mut random = random();
        let dir = TrackingDirectoryWrapper::new(new_directory(&mut random)?);
        assert_eq!(
            dir.get_created_files().lock().created_filenames,
            HashSet::new()
        );
        Ok(())
    }
    #[test]
    fn test_track_create() -> Result<()> {
        let mut random = random();
        let dir = TrackingDirectoryWrapper::new(new_directory(&mut random)?);
        dir.create_output("foo", &new_io_context(&mut random)?)?;
        assert_eq!(
            dir.get_created_files().lock().created_filenames,
            HashSet::from(["foo".to_string()])
        );
        Ok(())
    }

    #[test]
    fn test_track_delete() -> Result<()> {
        let mut random = random();
        let dir = TrackingDirectoryWrapper::new(new_directory(&mut random)?);
        dir.create_output("foo", &new_io_context(&mut random)?)?;
        assert_eq!(
            dir.get_created_files().lock().created_filenames,
            HashSet::from(["foo".to_string()])
        );
        dir.delete_file("foo")?;
        assert_eq!(
            dir.get_created_files().lock().created_filenames,
            HashSet::new()
        );
        Ok(())
    }

    #[test]
    fn test_track_rename() -> Result<()> {
        let mut random = random();
        let dir = TrackingDirectoryWrapper::new(new_directory(&mut random)?);
        dir.create_output("foo", &new_io_context(&mut random)?)?;
        assert_eq!(
            dir.get_created_files().lock().created_filenames,
            HashSet::from(["foo".to_string()])
        );
        dir.rename("foo", "bar")?;
        assert_eq!(
            dir.get_created_files().lock().created_filenames,
            HashSet::from(["bar".to_string()])
        );
        Ok(())
    }

    #[test]
    fn test_track_copy_from() -> Result<()> {
        let mut random = random();
        let source = TrackingDirectoryWrapper::new(new_directory(&mut random)?);
        let dest = TrackingDirectoryWrapper::new(new_directory(&mut random)?);

        source.create_output("foo", &new_io_context(&mut random)?)?;
        assert_eq!(
            source.get_created_files().lock().created_filenames,
            HashSet::from(["foo".to_string()])
        );

        dest.copy_from(&source, "foo", "bar", &new_io_context(&mut random)?)?;
        assert_eq!(
            dest.get_created_files().lock().created_filenames,
            HashSet::from(["bar".to_string()])
        );
        assert_eq!(
            source.get_created_files().lock().created_filenames,
            HashSet::from(["foo".to_string()])
        );
        Ok(())
    }
}
