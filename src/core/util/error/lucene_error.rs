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
use std::any::Any;
use std::fmt;
use std::io::Error;
use std::string::FromUtf8Error;

use thiserror::Error;

use crate::core::util::VersionError;
use crate::core::util::error::{
  AlreadyClosedError, ArrayIndexOutOfBoundsError, BufferAllocationError, CollectionTerminatedError,
  ConcurrentModificationError, CorruptIndexError, Eof, FuzzyTermsError, IllegalArgumentError,
  IllegalStateError, IndexFormatTooNewError, IndexFormatTooOldError, IndexNotFound,
  LockAlreadyHeldError, LockHeldByOtherError, LockObtainFailedError, LockReleaseFailedError,
  MaxBytesLengthExceededError, MergeAbortedError, MergeError, NeedImplementedError,
  NoMoreTermsError, NoSuchElementError, NotImplementedError, NotSuchFileError, NumberFormatError,
  NumberOverflow, TimeExceededError, TooComplexToDeterminizeError, TooManyClausesError,
  TooManyNestedClausesError, TragedyError, UncheckedIOError, UnreachableError,
  UnsupportedOperationError,
};

/// A panic payload that preserves the failures Java would attach to an
/// `Error` with `Throwable.addSuppressed`.
pub(crate) struct PanicWithSuppressed {
  primary: Box<dyn Any + Send>,
  suppressed: Vec<SuppressedFailure>,
}

pub(crate) enum SuppressedFailure {
  Panic(Box<dyn Any + Send>),
  Exception(LuceneError),
  ExceptionWithSuppressed(ExceptionWithSuppressed),
}

pub(crate) struct ExceptionWithSuppressed {
  primary: LuceneError,
  suppressed: Vec<LuceneError>,
}

impl PanicWithSuppressed {
  pub(crate) fn new(
    primary: Box<dyn Any + Send>,
    suppressed_panics: Vec<Box<dyn Any + Send>>,
    suppressed_exceptions: Vec<LuceneError>,
  ) -> Self {
    let mut exceptions = suppressed_exceptions.into_iter();
    let mut suppressed = suppressed_panics
      .into_iter()
      .map(SuppressedFailure::Panic)
      .collect::<Vec<_>>();
    if let Some(primary) = exceptions.next() {
      suppressed.push(SuppressedFailure::ExceptionWithSuppressed(
        ExceptionWithSuppressed {
          primary,
          suppressed: exceptions.collect(),
        },
      ));
    }
    Self {
      primary,
      suppressed,
    }
  }

  pub(crate) fn with_suppressed(
    primary: Box<dyn Any + Send>,
    suppressed: Vec<SuppressedFailure>,
  ) -> Self {
    Self {
      primary,
      suppressed,
    }
  }

  pub(crate) fn primary(&self) -> &(dyn Any + Send) {
    self.primary.as_ref()
  }

  fn add_suppressed_to_payload(payload: &mut Box<dyn Any + Send>, suppressed: SuppressedFailure) {
    if let Some(panic) = payload.downcast_mut::<PanicWithSuppressed>() {
      panic.suppressed.push(suppressed);
    } else {
      let primary = std::mem::replace(payload, Box::new(()));
      *payload = Box::new(PanicWithSuppressed::with_suppressed(
        primary,
        vec![suppressed],
      ));
    }
  }
}

impl SuppressedFailure {
  fn as_exception(&self, panic_message: &str) -> LuceneError {
    match self {
      Self::Panic(payload) => LuceneError::tragedy_from_panic(panic_message, payload.as_ref()),
      Self::Exception(error) => error.clone(),
      Self::ExceptionWithSuppressed(error) => {
        let mut primary = error.primary.clone();
        for suppressed in &error.suppressed {
          primary.add_suppressed(suppressed.clone());
        }
        primary
      },
    }
  }

  fn into_exception(self, panic_message: &str) -> LuceneError {
    match self {
      Self::Panic(payload) => match payload.downcast::<PanicWithSuppressed>() {
        Ok(panic) => {
          let PanicWithSuppressed {
            primary,
            suppressed,
          } = *panic;
          let mut primary = LuceneError::tragedy_from_panic(panic_message, primary.as_ref());
          for suppressed in suppressed {
            primary.add_suppressed(suppressed.into_exception(panic_message));
          }
          primary
        },
        Err(payload) => LuceneError::tragedy_from_panic(panic_message, payload.as_ref()),
      },
      Self::Exception(error) => error,
      Self::ExceptionWithSuppressed(error) => {
        let mut primary = error.primary;
        for suppressed in error.suppressed {
          primary.add_suppressed(suppressed);
        }
        primary
      },
    }
  }
}

pub(crate) struct CaughtResultDisplay<'a, R: ?Sized>(&'a R);

pub(crate) trait CaughtResultExt {
  fn caught_failure(&self, panic_message: &str) -> Option<LuceneError>;

  fn clone_caught_failure(&self, panic_message: &str) -> Option<CaughtResult>;

  fn caught_panic(&self, panic_message: &str) -> Option<LuceneError>;

  fn add_suppressed<T>(&mut self, suppressed: CaughtResult<T>, panic_message: &str);

  fn fmt_caught_result(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result;

  fn display(&self) -> CaughtResultDisplay<'_, Self>
  where
    Self: Sized,
  {
    CaughtResultDisplay(self)
  }
}

impl<R> fmt::Display for CaughtResultDisplay<'_, R>
where
  R: CaughtResultExt + ?Sized,
{
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.0.fmt_caught_result(formatter)
  }
}

impl<T> CaughtResultExt for CaughtResult<T> {
  fn caught_failure(&self, panic_message: &str) -> Option<LuceneError> {
    match self {
      Ok(Ok(_)) => None,
      Ok(Err(error)) => Some(error.clone()),
      Err(payload) => Some(LuceneError::tragedy_from_panic(
        panic_message,
        payload.as_ref(),
      )),
    }
  }

  fn clone_caught_failure(&self, panic_message: &str) -> Option<CaughtResult> {
    match self {
      Ok(Ok(_)) => None,
      Ok(Err(error)) => Some(Ok(Err(error.clone()))),
      Err(payload) => Some(Err(Box::new(LuceneError::tragedy_from_panic(
        panic_message,
        payload.as_ref(),
      )))),
    }
  }

  fn caught_panic(&self, panic_message: &str) -> Option<LuceneError> {
    match self {
      Err(payload) => Some(LuceneError::tragedy_from_panic(
        panic_message,
        payload.as_ref(),
      )),
      Ok(_) => None,
    }
  }

  fn add_suppressed<U>(&mut self, suppressed: CaughtResult<U>, panic_message: &str) {
    let suppressed = match suppressed {
      Ok(Ok(_)) => return,
      Ok(Err(error)) => SuppressedFailure::Exception(error),
      Err(payload) => SuppressedFailure::Panic(payload),
    };

    match self {
      Ok(Ok(_)) => unreachable!("cannot add a suppressed failure to a successful result"),
      Ok(Err(error)) => error.add_suppressed(suppressed.into_exception(panic_message)),
      Err(payload) => PanicWithSuppressed::add_suppressed_to_payload(payload, suppressed),
    }
  }

  fn fmt_caught_result(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Ok(Ok(_)) => formatter.write_str("Ok"),
      Ok(Err(error)) => fmt::Display::fmt(error, formatter),
      Err(payload) => formatter.write_str(&LuceneError::panic_payload_message(payload.as_ref())),
    }
  }
}

impl fmt::Debug for PanicWithSuppressed {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter
      .debug_struct("PanicWithSuppressed")
      .field(
        "primary",
        &LuceneError::panic_payload_message(self.primary.as_ref()),
      )
      .field("suppressed", &self.suppressed)
      .finish()
  }
}

impl fmt::Debug for SuppressedFailure {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Panic(payload) => formatter
        .debug_tuple("Panic")
        .field(&LuceneError::panic_payload_message(payload.as_ref()))
        .finish(),
      Self::Exception(error) => formatter.debug_tuple("Exception").field(error).finish(),
      Self::ExceptionWithSuppressed(error) => error.fmt(formatter),
    }
  }
}

impl fmt::Debug for ExceptionWithSuppressed {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter
      .debug_struct("ExceptionWithSuppressed")
      .field("primary", &self.primary)
      .field("suppressed", &self.suppressed)
      .finish()
  }
}

#[derive(Debug, Error)]
pub enum LuceneError {
  #[error("{0}")]
  AlreadyClosed(#[from] AlreadyClosedError),
  #[error("{0}")]
  ArrayIndexOutOfBounds(#[from] ArrayIndexOutOfBoundsError),
  #[error("{0}")]
  BufferAllocation(#[from] BufferAllocationError),
  #[error("{0}")]
  CollectionTerminated(#[from] CollectionTerminatedError),
  #[error("{0}")]
  ConcurrentModification(#[from] ConcurrentModificationError),
  #[error("{0}")]
  CorruptIndex(#[from] CorruptIndexError),
  #[error("{0}")]
  Eof(#[from] Eof),
  #[error("conversion failed: {source}")]
  Fmt {
    source: fmt::Error,
    suppressed: Option<Box<LuceneError>>,
  },
  #[error("UTF-8 conversion error: {source}")]
  FromUtf8Error {
    source: FromUtf8Error,
    suppressed: Option<Box<LuceneError>>,
  },
  #[error("{0}")]
  FuzzyTerms(#[from] FuzzyTermsError),
  #[error("{0}")]
  IllegalArgument(#[from] IllegalArgumentError),
  #[error("{0}")]
  IllegalState(#[from] IllegalStateError),
  #[error("{0}")]
  IndexFormatTooNew(#[from] IndexFormatTooNewError),
  #[error("{0}")]
  IndexFormatTooOld(#[from] IndexFormatTooOldError),
  #[error("{0}")]
  IndexNotFound(#[from] IndexNotFound),
  #[error("IO error: {source}")]
  Io {
    source: Error,
    suppressed: Option<Box<LuceneError>>,
  },
  #[error("IO error on {path}: {source}, {err_kind}")]
  IoWithPath {
    source: Error,
    path: String,
    err_kind: String,
    suppressed: Option<Box<LuceneError>>,
  },
  #[error("{0}")]
  LockAlreadyHeld(#[from] LockAlreadyHeldError),
  #[error("{0}")]
  LockHeldByOther(#[from] LockHeldByOtherError),
  #[error("{0}")]
  LockObtainFailed(#[from] LockObtainFailedError),
  #[error("{0}")]
  LockReleaseFailed(#[from] LockReleaseFailedError),
  #[error("{0}")]
  MaxBytesLengthExceeded(#[from] MaxBytesLengthExceededError),
  #[error("{0}")]
  Merge(#[from] MergeError),
  #[error("{0}")]
  MergeAborted(#[from] MergeAbortedError),
  #[error("{0}")]
  NeedImplemented(#[from] NeedImplementedError),
  #[error("{0}")]
  NoMoreTerms(#[from] NoMoreTermsError),
  #[error("{0}")]
  NoSuchElement(#[from] NoSuchElementError),
  #[error("{0}")]
  NoSuchFile(#[from] NotSuchFileError),
  #[error("{0}")]
  NotImplemented(#[from] NotImplementedError),
  #[error("{0}")]
  NumberFormat(#[from] NumberFormatError),
  #[error("{0}")]
  NumberOverflow(#[from] NumberOverflow),
  #[error("parse int error: {source}")]
  ParseIntError {
    source: std::num::ParseIntError,
    suppressed: Option<Box<LuceneError>>,
  },
  #[error("{0}")]
  TimeExceeded(#[from] TimeExceededError),
  #[error("{0}")]
  TooComplexToDeterminize(#[from] TooComplexToDeterminizeError),
  #[error("{0}")]
  TooManyClauses(#[from] TooManyClausesError),
  #[error("{0}")]
  TooManyNestedClauses(#[from] TooManyNestedClausesError),
  #[error("{0}")]
  Tragedy(#[from] TragedyError),
  #[error("{0}")]
  UncheckedIO(#[from] UncheckedIOError),
  #[error("{0}")]
  Unreachable(#[from] UnreachableError),
  #[error("{0}")]
  UnsupportedOperation(#[from] UnsupportedOperationError),
  #[error("UTF-8 decoding error: {source}")]
  Utf8Error {
    source: std::str::Utf8Error,
    suppressed: Option<Box<LuceneError>>,
  },
  #[error("{source}")]
  VersionError {
    source: VersionError,
    suppressed: Option<Box<LuceneError>>,
  },
}

impl Clone for LuceneError {
  fn clone(&self) -> Self {
    match self {
      LuceneError::AlreadyClosed(err) => LuceneError::AlreadyClosed(err.clone()),
      LuceneError::ArrayIndexOutOfBounds(err) => LuceneError::ArrayIndexOutOfBounds(err.clone()),
      LuceneError::BufferAllocation(err) => LuceneError::BufferAllocation(err.clone()),
      LuceneError::CollectionTerminated(err) => LuceneError::CollectionTerminated(err.clone()),
      LuceneError::ConcurrentModification(err) => LuceneError::ConcurrentModification(err.clone()),
      LuceneError::CorruptIndex(err) => LuceneError::CorruptIndex(err.clone()),
      LuceneError::Eof(err) => LuceneError::Eof(err.clone()),
      LuceneError::Fmt { source, suppressed } => LuceneError::Fmt {
        source: *source,
        suppressed: suppressed.clone(),
      },
      LuceneError::FromUtf8Error { source, suppressed } => LuceneError::FromUtf8Error {
        source: source.clone(),
        suppressed: suppressed.clone(),
      },
      LuceneError::FuzzyTerms(err) => LuceneError::FuzzyTerms(err.clone()),
      LuceneError::IllegalArgument(err) => LuceneError::IllegalArgument(err.clone()),
      LuceneError::IllegalState(err) => LuceneError::IllegalState(err.clone()),
      LuceneError::IndexFormatTooNew(err) => LuceneError::IndexFormatTooNew(err.clone()),
      LuceneError::IndexFormatTooOld(err) => LuceneError::IndexFormatTooOld(err.clone()),
      LuceneError::IndexNotFound(err) => LuceneError::IndexNotFound(err.clone()),
      LuceneError::Io { source, suppressed } => LuceneError::Io {
        source: Error::new(source.kind(), source.to_string()),
        suppressed: suppressed.clone(),
      },
      LuceneError::IoWithPath {
        source,
        path,
        err_kind,
        suppressed,
      } => LuceneError::IoWithPath {
        source: Error::new(source.kind(), source.to_string()),
        path: path.clone(),
        err_kind: err_kind.clone(),
        suppressed: suppressed.clone(),
      },
      LuceneError::LockAlreadyHeld(err) => LuceneError::LockAlreadyHeld(err.clone()),
      LuceneError::LockHeldByOther(err) => LuceneError::LockHeldByOther(err.clone()),
      LuceneError::LockObtainFailed(err) => LuceneError::LockObtainFailed(err.clone()),
      LuceneError::LockReleaseFailed(err) => LuceneError::LockReleaseFailed(err.clone()),
      LuceneError::MaxBytesLengthExceeded(err) => LuceneError::MaxBytesLengthExceeded(err.clone()),
      LuceneError::Merge(err) => LuceneError::Merge(err.clone()),
      LuceneError::MergeAborted(err) => LuceneError::MergeAborted(err.clone()),
      LuceneError::NeedImplemented(err) => LuceneError::NeedImplemented(err.clone()),
      LuceneError::NoMoreTerms(err) => LuceneError::NoMoreTerms(err.clone()),
      LuceneError::NoSuchElement(err) => LuceneError::NoSuchElement(err.clone()),
      LuceneError::NoSuchFile(err) => LuceneError::NoSuchFile(err.clone()),
      LuceneError::NotImplemented(err) => LuceneError::NotImplemented(err.clone()),
      LuceneError::NumberFormat(err) => LuceneError::NumberFormat(err.clone()),
      LuceneError::NumberOverflow(err) => LuceneError::NumberOverflow(err.clone()),
      LuceneError::ParseIntError { source, suppressed } => LuceneError::ParseIntError {
        source: source.clone(),
        suppressed: suppressed.clone(),
      },
      LuceneError::TimeExceeded(err) => LuceneError::TimeExceeded(err.clone()),
      LuceneError::TooComplexToDeterminize(err) => {
        LuceneError::TooComplexToDeterminize(err.clone())
      },
      LuceneError::TooManyClauses(err) => LuceneError::TooManyClauses(err.clone()),
      LuceneError::TooManyNestedClauses(err) => LuceneError::TooManyNestedClauses(err.clone()),
      LuceneError::Tragedy(err) => LuceneError::Tragedy(err.clone()),
      LuceneError::UncheckedIO(err) => LuceneError::UncheckedIO(err.clone()),
      LuceneError::Unreachable(err) => LuceneError::Unreachable(err.clone()),
      LuceneError::UnsupportedOperation(err) => LuceneError::UnsupportedOperation(err.clone()),
      LuceneError::Utf8Error { source, suppressed } => LuceneError::Utf8Error {
        source: *source,
        suppressed: suppressed.clone(),
      },
      LuceneError::VersionError { source, suppressed } => LuceneError::VersionError {
        source: source.clone(),
        suppressed: suppressed.clone(),
      },
    }
  }
}

impl From<fmt::Error> for LuceneError {
  fn from(source: fmt::Error) -> Self {
    LuceneError::Fmt {
      source,
      suppressed: None,
    }
  }
}

impl From<FromUtf8Error> for LuceneError {
  fn from(source: FromUtf8Error) -> Self {
    LuceneError::FromUtf8Error {
      source,
      suppressed: None,
    }
  }
}

impl From<Error> for LuceneError {
  fn from(source: Error) -> Self {
    LuceneError::Io {
      source,
      suppressed: None,
    }
  }
}

impl From<std::num::ParseIntError> for LuceneError {
  fn from(source: std::num::ParseIntError) -> Self {
    LuceneError::ParseIntError {
      source,
      suppressed: None,
    }
  }
}

impl From<std::str::Utf8Error> for LuceneError {
  fn from(source: std::str::Utf8Error) -> Self {
    LuceneError::Utf8Error {
      source,
      suppressed: None,
    }
  }
}

impl From<VersionError> for LuceneError {
  fn from(source: VersionError) -> Self {
    LuceneError::VersionError {
      source,
      suppressed: None,
    }
  }
}

macro_rules! error_ctor {
  (@add_suppressed $(($variant:ident)),+ $(,)?) => {
    pub fn add_suppressed(&mut self, source: LuceneError) {
      match self {
        $(
          LuceneError::$variant(err) => {
            err.add_suppressed(source);
          },
        )+
        LuceneError::Fmt { suppressed, .. }
        | LuceneError::FromUtf8Error { suppressed, .. }
        | LuceneError::Io { suppressed, .. }
        | LuceneError::IoWithPath { suppressed, .. }
        | LuceneError::ParseIntError { suppressed, .. }
        | LuceneError::Utf8Error { suppressed, .. }
        | LuceneError::VersionError { suppressed, .. } => {
          match suppressed.as_mut() {
            Some(suppressed) => suppressed.add_suppressed(source),
            None => *suppressed = Some(Box::new(source)),
          }
        },
      }
    }

    pub fn get_suppressed(&self) -> Result<Option<&LuceneError>> {
      match self {
        $(
          LuceneError::$variant(err) => Ok(err.get_suppressed()),
        )+
        LuceneError::Fmt { suppressed, .. }
        | LuceneError::FromUtf8Error { suppressed, .. }
        | LuceneError::Io { suppressed, .. }
        | LuceneError::IoWithPath { suppressed, .. }
        | LuceneError::ParseIntError { suppressed, .. }
        | LuceneError::Utf8Error { suppressed, .. }
        | LuceneError::VersionError { suppressed, .. } => Ok(suppressed.as_deref()),
      }
    }
  };

  ($fn_name:ident, $variant:ident, $error_type:ident) => {
    pub fn $fn_name(err: impl Into<$error_type>) -> Self {
      LuceneError::$variant(err.into())
    }
  };
}
impl LuceneError {
  pub fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
      (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
      message.clone()
    } else if let Some(panic) = payload.downcast_ref::<PanicWithSuppressed>() {
      LuceneError::panic_payload_message(panic.primary())
    } else if let Some(error) = payload.downcast_ref::<LuceneError>() {
      error.to_string()
    } else {
      "unknown panic payload".to_string()
    }
  }

  pub fn tragedy_from_panic(prefix: &str, payload: &(dyn Any + Send)) -> Self {
    if let Some(panic) = payload.downcast_ref::<PanicWithSuppressed>() {
      let mut primary = LuceneError::tragedy_from_panic(prefix, panic.primary());
      for suppressed in &panic.suppressed {
        primary.add_suppressed(suppressed.as_exception(prefix));
      }
      return primary;
    }
    LuceneError::tragedy(format!(
      "{prefix}: {}",
      LuceneError::panic_payload_message(payload)
    ))
  }

  pub fn io_with_path(path: impl Into<String>, err: std::io::Error) -> Self {
    let message = err.kind().to_string();
    LuceneError::IoWithPath {
      source: err,
      path: path.into(),
      err_kind: message,
      suppressed: None,
    }
  }

  pub fn io(err: std::io::Error) -> Self {
    Self::io_with_path("", err)
  }

  /// Returns whether this error corresponds to a Java `IOException` subtype.
  pub fn is_io_error(&self) -> bool {
    matches!(
      self,
      LuceneError::CorruptIndex(_)
        | LuceneError::Eof(_)
        | LuceneError::IndexFormatTooNew(_)
        | LuceneError::IndexFormatTooOld(_)
        | LuceneError::IndexNotFound(_)
        | LuceneError::Io { .. }
        | LuceneError::IoWithPath { .. }
        | LuceneError::LockAlreadyHeld(_)
        | LuceneError::LockHeldByOther(_)
        | LuceneError::LockObtainFailed(_)
        | LuceneError::LockReleaseFailed(_)
        | LuceneError::MergeAborted(_)
        | LuceneError::NoSuchFile(_)
    )
  }

  error_ctor!(already_closed, AlreadyClosed, AlreadyClosedError);
  error_ctor!(
    array_index_out_of_bounds,
    ArrayIndexOutOfBounds,
    ArrayIndexOutOfBoundsError
  );
  error_ctor!(buffer_allocation, BufferAllocation, BufferAllocationError);
  error_ctor!(
    collection_terminated,
    CollectionTerminated,
    CollectionTerminatedError
  );
  error_ctor!(
    concurrent_modification,
    ConcurrentModification,
    ConcurrentModificationError
  );
  error_ctor!(corrupt_index, CorruptIndex, CorruptIndexError);
  error_ctor!(eof, Eof, Eof);
  error_ctor!(fuzzy_terms, FuzzyTerms, FuzzyTermsError);
  error_ctor!(illegal_argument, IllegalArgument, IllegalArgumentError);
  error_ctor!(illegal_state, IllegalState, IllegalStateError);
  pub fn index_format_too_new(
    input: &impl fmt::Display,
    version: i32,
    min_version: i32,
    max_version: i32,
  ) -> Self {
    LuceneError::IndexFormatTooNew(IndexFormatTooNewError::from_input(
      input,
      version,
      min_version,
      max_version,
    ))
  }
  pub fn index_format_too_old(input: &impl fmt::Display, reason: impl Into<String>) -> Self {
    LuceneError::IndexFormatTooOld(IndexFormatTooOldError::from_input(input, reason))
  }

  pub fn index_format_too_old_with_version(
    input: &impl fmt::Display,
    version: i32,
    min_version: i32,
    max_version: i32,
  ) -> Self {
    LuceneError::IndexFormatTooOld(IndexFormatTooOldError::from_input_with_version(
      input,
      version,
      min_version,
      max_version,
    ))
  }
  error_ctor!(index_not_found, IndexNotFound, IndexNotFound);
  error_ctor!(lock_already_held, LockAlreadyHeld, LockAlreadyHeldError);
  error_ctor!(lock_held_by_other, LockHeldByOther, LockHeldByOtherError);
  error_ctor!(lock_obtain_failed, LockObtainFailed, LockObtainFailedError);
  error_ctor!(
    lock_release_failed,
    LockReleaseFailed,
    LockReleaseFailedError
  );
  error_ctor!(
    max_bytes_length_exceeded,
    MaxBytesLengthExceeded,
    MaxBytesLengthExceededError
  );
  error_ctor!(merge, Merge, MergeError);
  error_ctor!(merge_abort, MergeAborted, MergeAbortedError);
  error_ctor!(need_implemented, NeedImplemented, NeedImplementedError);
  error_ctor!(no_more_terms, NoMoreTerms, NoMoreTermsError);
  error_ctor!(no_such_element, NoSuchElement, NoSuchElementError);
  error_ctor!(not_such_file, NoSuchFile, NotSuchFileError);
  error_ctor!(not_implemented, NotImplemented, NotImplementedError);
  error_ctor!(number_format, NumberFormat, NumberFormatError);
  error_ctor!(number_overflow, NumberOverflow, NumberOverflow);
  error_ctor!(time_exceeded, TimeExceeded, TimeExceededError);
  error_ctor!(
    too_complex_to_determinize,
    TooComplexToDeterminize,
    TooComplexToDeterminizeError
  );
  error_ctor!(too_many_clauses, TooManyClauses, TooManyClausesError);
  error_ctor!(
    too_many_nested_clauses,
    TooManyNestedClauses,
    TooManyNestedClausesError
  );
  error_ctor!(tragedy, Tragedy, TragedyError);
  error_ctor!(unchecked_io_error, UncheckedIO, UncheckedIOError);
  error_ctor!(unreachable, Unreachable, UnreachableError);
  error_ctor!(
    unsupported_operation,
    UnsupportedOperation,
    UnsupportedOperationError
  );

  error_ctor!(
    @add_suppressed
    (AlreadyClosed),
    (ArrayIndexOutOfBounds),
    (BufferAllocation),
    (CollectionTerminated),
    (ConcurrentModification),
    (CorruptIndex),
    (Eof),
    (FuzzyTerms),
    (IllegalArgument),
    (IllegalState),
    (IndexFormatTooNew),
    (IndexFormatTooOld),
    (IndexNotFound),
    (LockAlreadyHeld),
    (LockHeldByOther),
    (LockObtainFailed),
    (LockReleaseFailed),
    (MaxBytesLengthExceeded),
    (Merge),
    (MergeAborted),
    (NeedImplemented),
    (NoMoreTerms),
    (NoSuchElement),
    (NoSuchFile),
    (NotImplemented),
    (NumberFormat),
    (NumberOverflow),
    (TimeExceeded),
    (TooComplexToDeterminize),
    (TooManyClauses),
    (TooManyNestedClauses),
    (Tragedy),
    (UncheckedIO),
    (Unreachable),
    (UnsupportedOperation),
  );
}

pub type Result<T> = core::result::Result<T, LuceneError>;

pub type CaughtResult<T = ()> = std::thread::Result<Result<T>>;
