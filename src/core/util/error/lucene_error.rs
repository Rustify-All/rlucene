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
use std::borrow::Cow;
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

/// A panic payload that preserves failures suppressed during cleanup.
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
    let capacity = suppressed_panics.len() + usize::from(!suppressed_exceptions.is_empty());
    let mut errors = suppressed_exceptions.into_iter();
    let mut suppressed = Vec::with_capacity(capacity);
    suppressed.extend(suppressed_panics.into_iter().map(SuppressedFailure::Panic));
    if let Some(primary) = errors.next() {
      suppressed.push(SuppressedFailure::ExceptionWithSuppressed(
        ExceptionWithSuppressed {
          primary,
          suppressed: errors.collect(),
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
}

pub(crate) struct CaughtResultDisplay<'a, R: ?Sized>(&'a R);

pub(crate) trait CaughtResultExt {
  fn caught_failure(&self, panic_message: &str) -> Option<LuceneError>;

  fn clone_caught_failure(&self, panic_message: &str) -> Option<CaughtResult>;

  /// Adds a cleanup failure to an existing primary failure. If the primary operation succeeded,
  /// the cleanup failure becomes the primary failure so it can still be propagated.
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

  #[inline]
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

  fn add_suppressed<U>(&mut self, suppressed: CaughtResult<U>, panic_message: &str) {
    match suppressed {
      Ok(Ok(_)) => {},
      Ok(Err(suppressed)) => match self {
        Ok(Ok(_)) => *self = Ok(Err(suppressed)),
        Ok(Err(primary)) => primary.add_suppressed(suppressed),
        Err(primary) => PanicWithSuppressed::add_suppressed_to_payload(
          primary,
          SuppressedFailure::Exception(suppressed),
        ),
      },
      Err(suppressed) => match self {
        Ok(Ok(_)) => *self = Err(suppressed),
        Ok(Err(primary)) => {
          primary.add_suppressed(LuceneError::tragedy_from_panic(
            panic_message,
            suppressed.as_ref(),
          ));
        },
        Err(primary) => PanicWithSuppressed::add_suppressed_to_payload(
          primary,
          SuppressedFailure::Panic(suppressed),
        ),
      },
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
      Self::Exception(error) => formatter.debug_tuple("Error").field(error).finish(),
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

/// An owned Lucene error with its payload stored behind a single `Box`.
///
/// Use the existing constructors or `From` conversions to create errors, and
/// [`Self::kind`] to inspect them without consuming their payloads.
#[derive(Error)]
#[error(transparent)]
#[repr(transparent)]
pub struct LuceneError(Box<LuceneErrorKind>);

/// The payload and classification of a [`LuceneError`].
///
/// This type retains the concrete error payloads. Use [`LuceneError`] when
/// returning or storing an error so the full payload is not carried inline.
#[derive(Debug, Error)]
pub enum LuceneErrorKind {
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
    suppressed: Option<LuceneError>,
  },
  #[error("UTF-8 conversion error: {source}")]
  FromUtf8Error {
    source: FromUtf8Error,
    suppressed: Option<LuceneError>,
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
    suppressed: Option<LuceneError>,
  },
  #[error("IO error on {path}: {source}, {err_kind}")]
  IoWithPath {
    source: Error,
    path: String,
    err_kind: String,
    suppressed: Option<LuceneError>,
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
    suppressed: Option<LuceneError>,
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
    suppressed: Option<LuceneError>,
  },
  #[error("{source}")]
  VersionError {
    source: VersionError,
    suppressed: Option<LuceneError>,
  },
}

impl Clone for LuceneErrorKind {
  fn clone(&self) -> Self {
    match self {
      LuceneErrorKind::AlreadyClosed(err) => LuceneErrorKind::AlreadyClosed(err.clone()),
      LuceneErrorKind::ArrayIndexOutOfBounds(err) => {
        LuceneErrorKind::ArrayIndexOutOfBounds(err.clone())
      },
      LuceneErrorKind::BufferAllocation(err) => LuceneErrorKind::BufferAllocation(err.clone()),
      LuceneErrorKind::CollectionTerminated(err) => {
        LuceneErrorKind::CollectionTerminated(err.clone())
      },
      LuceneErrorKind::ConcurrentModification(err) => {
        LuceneErrorKind::ConcurrentModification(err.clone())
      },
      LuceneErrorKind::CorruptIndex(err) => LuceneErrorKind::CorruptIndex(err.clone()),
      LuceneErrorKind::Eof(err) => LuceneErrorKind::Eof(err.clone()),
      LuceneErrorKind::Fmt { source, suppressed } => LuceneErrorKind::Fmt {
        source: *source,
        suppressed: suppressed.clone(),
      },
      LuceneErrorKind::FromUtf8Error { source, suppressed } => LuceneErrorKind::FromUtf8Error {
        source: source.clone(),
        suppressed: suppressed.clone(),
      },
      LuceneErrorKind::FuzzyTerms(err) => LuceneErrorKind::FuzzyTerms(err.clone()),
      LuceneErrorKind::IllegalArgument(err) => LuceneErrorKind::IllegalArgument(err.clone()),
      LuceneErrorKind::IllegalState(err) => LuceneErrorKind::IllegalState(err.clone()),
      LuceneErrorKind::IndexFormatTooNew(err) => LuceneErrorKind::IndexFormatTooNew(err.clone()),
      LuceneErrorKind::IndexFormatTooOld(err) => LuceneErrorKind::IndexFormatTooOld(err.clone()),
      LuceneErrorKind::IndexNotFound(err) => LuceneErrorKind::IndexNotFound(err.clone()),
      LuceneErrorKind::Io { source, suppressed } => LuceneErrorKind::Io {
        source: Error::new(source.kind(), source.to_string()),
        suppressed: suppressed.clone(),
      },
      LuceneErrorKind::IoWithPath {
        source,
        path,
        err_kind,
        suppressed,
      } => LuceneErrorKind::IoWithPath {
        source: Error::new(source.kind(), source.to_string()),
        path: path.clone(),
        err_kind: err_kind.clone(),
        suppressed: suppressed.clone(),
      },
      LuceneErrorKind::LockAlreadyHeld(err) => LuceneErrorKind::LockAlreadyHeld(err.clone()),
      LuceneErrorKind::LockHeldByOther(err) => LuceneErrorKind::LockHeldByOther(err.clone()),
      LuceneErrorKind::LockObtainFailed(err) => LuceneErrorKind::LockObtainFailed(err.clone()),
      LuceneErrorKind::LockReleaseFailed(err) => LuceneErrorKind::LockReleaseFailed(err.clone()),
      LuceneErrorKind::MaxBytesLengthExceeded(err) => {
        LuceneErrorKind::MaxBytesLengthExceeded(err.clone())
      },
      LuceneErrorKind::Merge(err) => LuceneErrorKind::Merge(err.clone()),
      LuceneErrorKind::MergeAborted(err) => LuceneErrorKind::MergeAborted(err.clone()),
      LuceneErrorKind::NeedImplemented(err) => LuceneErrorKind::NeedImplemented(err.clone()),
      LuceneErrorKind::NoMoreTerms(err) => LuceneErrorKind::NoMoreTerms(err.clone()),
      LuceneErrorKind::NoSuchElement(err) => LuceneErrorKind::NoSuchElement(err.clone()),
      LuceneErrorKind::NoSuchFile(err) => LuceneErrorKind::NoSuchFile(err.clone()),
      LuceneErrorKind::NotImplemented(err) => LuceneErrorKind::NotImplemented(err.clone()),
      LuceneErrorKind::NumberFormat(err) => LuceneErrorKind::NumberFormat(err.clone()),
      LuceneErrorKind::NumberOverflow(err) => LuceneErrorKind::NumberOverflow(err.clone()),
      LuceneErrorKind::ParseIntError { source, suppressed } => LuceneErrorKind::ParseIntError {
        source: source.clone(),
        suppressed: suppressed.clone(),
      },
      LuceneErrorKind::TimeExceeded(err) => LuceneErrorKind::TimeExceeded(err.clone()),
      LuceneErrorKind::TooComplexToDeterminize(err) => {
        LuceneErrorKind::TooComplexToDeterminize(err.clone())
      },
      LuceneErrorKind::TooManyClauses(err) => LuceneErrorKind::TooManyClauses(err.clone()),
      LuceneErrorKind::TooManyNestedClauses(err) => {
        LuceneErrorKind::TooManyNestedClauses(err.clone())
      },
      LuceneErrorKind::Tragedy(err) => LuceneErrorKind::Tragedy(err.clone()),
      LuceneErrorKind::UncheckedIO(err) => LuceneErrorKind::UncheckedIO(err.clone()),
      LuceneErrorKind::Unreachable(err) => LuceneErrorKind::Unreachable(err.clone()),
      LuceneErrorKind::UnsupportedOperation(err) => {
        LuceneErrorKind::UnsupportedOperation(err.clone())
      },
      LuceneErrorKind::Utf8Error { source, suppressed } => LuceneErrorKind::Utf8Error {
        source: *source,
        suppressed: suppressed.clone(),
      },
      LuceneErrorKind::VersionError { source, suppressed } => LuceneErrorKind::VersionError {
        source: source.clone(),
        suppressed: suppressed.clone(),
      },
    }
  }
}

impl LuceneError {
  /// Borrows the concrete error payload without allocating or cloning it.
  pub fn kind(&self) -> &LuceneErrorKind {
    self.0.as_ref()
  }

  /// Mutably borrows the concrete error payload while retaining its owner.
  pub fn kind_mut(&mut self) -> &mut LuceneErrorKind {
    self.0.as_mut()
  }

  /// Consumes the error and moves its payload out of the box without cloning it.
  pub fn into_kind(self) -> LuceneErrorKind {
    *self.0
  }
}

impl fmt::Debug for LuceneError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Debug::fmt(self.kind(), formatter)
  }
}

impl Clone for LuceneError {
  fn clone(&self) -> Self {
    self.kind().clone().into()
  }
}

impl From<LuceneErrorKind> for LuceneError {
  fn from(kind: LuceneErrorKind) -> Self {
    Self(Box::new(kind))
  }
}

macro_rules! impl_from_error_payload {
  ($($error:ty),+ $(,)?) => {
    $(
      impl From<$error> for LuceneError {
        #[cold]
        fn from(error: $error) -> Self {
          LuceneErrorKind::from(error).into()
        }
      }
    )+
  };
}

impl_from_error_payload!(
  AlreadyClosedError,
  ArrayIndexOutOfBoundsError,
  BufferAllocationError,
  CollectionTerminatedError,
  ConcurrentModificationError,
  CorruptIndexError,
  Eof,
  FuzzyTermsError,
  IllegalArgumentError,
  IllegalStateError,
  IndexFormatTooNewError,
  IndexFormatTooOldError,
  IndexNotFound,
  LockAlreadyHeldError,
  LockHeldByOtherError,
  LockObtainFailedError,
  LockReleaseFailedError,
  MaxBytesLengthExceededError,
  MergeError,
  MergeAbortedError,
  NeedImplementedError,
  NoMoreTermsError,
  NoSuchElementError,
  NotSuchFileError,
  NotImplementedError,
  NumberFormatError,
  NumberOverflow,
  TimeExceededError,
  TooComplexToDeterminizeError,
  TooManyClausesError,
  TooManyNestedClausesError,
  TragedyError,
  UncheckedIOError,
  UnreachableError,
  UnsupportedOperationError,
);

impl From<fmt::Error> for LuceneError {
  fn from(source: fmt::Error) -> Self {
    LuceneErrorKind::Fmt {
      source,
      suppressed: None,
    }
    .into()
  }
}

impl From<FromUtf8Error> for LuceneError {
  fn from(source: FromUtf8Error) -> Self {
    LuceneErrorKind::FromUtf8Error {
      source,
      suppressed: None,
    }
    .into()
  }
}

impl From<Error> for LuceneError {
  fn from(source: Error) -> Self {
    LuceneErrorKind::Io {
      source,
      suppressed: None,
    }
    .into()
  }
}

impl From<std::num::ParseIntError> for LuceneError {
  fn from(source: std::num::ParseIntError) -> Self {
    LuceneErrorKind::ParseIntError {
      source,
      suppressed: None,
    }
    .into()
  }
}

impl From<std::str::Utf8Error> for LuceneError {
  fn from(source: std::str::Utf8Error) -> Self {
    LuceneErrorKind::Utf8Error {
      source,
      suppressed: None,
    }
    .into()
  }
}

impl From<VersionError> for LuceneError {
  fn from(source: VersionError) -> Self {
    LuceneErrorKind::VersionError {
      source,
      suppressed: None,
    }
    .into()
  }
}

macro_rules! error_ctor {
  (@add_suppressed $(($variant:ident)),+ $(,)?) => {
    pub fn add_suppressed(&mut self, source: LuceneError) {
      match self.kind_mut() {
        $(
          LuceneErrorKind::$variant(err) => {
            err.add_suppressed(source);
          },
        )+
        LuceneErrorKind::Fmt { suppressed, .. }
        | LuceneErrorKind::FromUtf8Error { suppressed, .. }
        | LuceneErrorKind::Io { suppressed, .. }
        | LuceneErrorKind::IoWithPath { suppressed, .. }
        | LuceneErrorKind::ParseIntError { suppressed, .. }
        | LuceneErrorKind::Utf8Error { suppressed, .. }
        | LuceneErrorKind::VersionError { suppressed, .. } => {
          match suppressed.as_mut() {
            Some(suppressed) => suppressed.add_suppressed(source),
            None => *suppressed = Some(source),
          }
        },
      }
    }

    pub fn get_suppressed(&self) -> Result<Option<&LuceneError>> {
      match self.kind() {
        $(
          LuceneErrorKind::$variant(err) => Ok(err.get_suppressed()),
        )+
        LuceneErrorKind::Fmt { suppressed, .. }
        | LuceneErrorKind::FromUtf8Error { suppressed, .. }
        | LuceneErrorKind::Io { suppressed, .. }
        | LuceneErrorKind::IoWithPath { suppressed, .. }
        | LuceneErrorKind::ParseIntError { suppressed, .. }
        | LuceneErrorKind::Utf8Error { suppressed, .. }
        | LuceneErrorKind::VersionError { suppressed, .. } => Ok(suppressed.as_ref()),
      }
    }
  };

  ($fn_name:ident, $variant:ident, $error_type:ident) => {
    #[cold]
    pub fn $fn_name<T>(err: T) -> Self
    where
      T: Into<$error_type>,
    {
      LuceneErrorKind::$variant(err.into()).into()
    }
  };
}
impl LuceneError {
  pub fn panic_payload_message(payload: &(dyn Any + Send)) -> Cow<'_, str> {
    if let Some(message) = payload.downcast_ref::<&str>() {
      Cow::Borrowed(message)
    } else if let Some(message) = payload.downcast_ref::<String>() {
      Cow::Borrowed(message)
    } else if let Some(panic) = payload.downcast_ref::<PanicWithSuppressed>() {
      LuceneError::panic_payload_message(panic.primary())
    } else if let Some(error) = payload.downcast_ref::<LuceneError>() {
      Cow::Owned(error.to_string())
    } else {
      Cow::Borrowed("unknown panic payload")
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

  pub fn io_with_path<T>(path: T, err: std::io::Error) -> Self
  where
    T: Into<String>,
  {
    let message = err.kind().to_string();
    LuceneErrorKind::IoWithPath {
      source: err,
      path: path.into(),
      err_kind: message,
      suppressed: None,
    }
    .into()
  }

  pub fn io(err: std::io::Error) -> Self {
    Self::io_with_path("", err)
  }

  /// Returns whether this is an I/O-related error.
  pub fn is_io_error(&self) -> bool {
    matches!(
      self.kind(),
      LuceneErrorKind::CorruptIndex(_)
        | LuceneErrorKind::Eof(_)
        | LuceneErrorKind::IndexFormatTooNew(_)
        | LuceneErrorKind::IndexFormatTooOld(_)
        | LuceneErrorKind::IndexNotFound(_)
        | LuceneErrorKind::Io { .. }
        | LuceneErrorKind::IoWithPath { .. }
        | LuceneErrorKind::LockAlreadyHeld(_)
        | LuceneErrorKind::LockHeldByOther(_)
        | LuceneErrorKind::LockObtainFailed(_)
        | LuceneErrorKind::LockReleaseFailed(_)
        | LuceneErrorKind::MergeAborted(_)
        | LuceneErrorKind::NoSuchFile(_)
    )
  }

  /// Returns whether this is an illegal-state error, including an already-closed error.
  pub fn is_illegal_state_error(&self) -> bool {
    matches!(
      self.kind(),
      LuceneErrorKind::IllegalState(_) | LuceneErrorKind::AlreadyClosed(_)
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
  pub fn index_format_too_new<T>(
    input: &T,
    version: i32,
    min_version: i32,
    max_version: i32,
  ) -> Self
  where
    T: fmt::Display,
  {
    LuceneErrorKind::IndexFormatTooNew(IndexFormatTooNewError::from_input(
      input,
      version,
      min_version,
      max_version,
    ))
    .into()
  }
  pub fn index_format_too_old<T, T2>(input: &T, reason: T2) -> Self
  where
    T: fmt::Display,
    T2: Into<String>,
  {
    LuceneErrorKind::IndexFormatTooOld(IndexFormatTooOldError::from_input(input, reason)).into()
  }

  pub fn index_format_too_old_with_version<T>(
    input: &T,
    version: i32,
    min_version: i32,
    max_version: i32,
  ) -> Self
  where
    T: fmt::Display,
  {
    LuceneErrorKind::IndexFormatTooOld(IndexFormatTooOldError::from_input_with_version(
      input,
      version,
      min_version,
      max_version,
    ))
    .into()
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
