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
use std::fmt::{Display, Formatter, Write};

use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};

use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Provides support for converting dates to strings and vice-versa.
///
/// The strings are structured so that lexicographic sorting orders them by
/// date, which makes them suitable for use as field values and search terms.
///
/// This type also helps limit the resolution of dates. Do not save dates with
/// a finer resolution than you really need, since range and prefix queries over
/// date strings require more memory and become slower at finer resolutions.
///
/// Another approach is to index unix timestamps as numeric values and query
/// them with point range queries.
pub struct DateTools;

impl DateTools {
  /// Converts a date to a string suitable for indexing.
  ///
  /// The returned string is in the `yyyyMMddHHmmssSSS` format or shorter,
  /// depending on the requested `resolution`, using UTC as the timezone.
  pub fn date_to_string(date: DateTime<Utc>, resolution: Resolution) -> Result<String> {
    Self::time_to_string(date.timestamp_millis(), resolution)
  }

  /// Converts a millisecond time to a string suitable for indexing.
  ///
  /// `time` is expressed as milliseconds since January 1, 1970, 00:00:00 UTC.
  /// The returned string is in the `yyyyMMddHHmmssSSS` format or shorter,
  /// depending on the requested `resolution`, using UTC as the timezone.
  pub fn time_to_string(time: i64, resolution: Resolution) -> Result<String> {
    let date = date_time_from_millis(Self::round(time, resolution)?)?;
    format_date(date, resolution)
  }

  /// Converts a string produced by [`time_to_string`](Self::time_to_string) or
  /// [`date_to_string`](Self::date_to_string) back to milliseconds since
  /// January 1, 1970, 00:00:00 UTC.
  ///
  /// Returns an error if `date_string` is not in the expected format.
  pub fn string_to_time(date_string: &str) -> Result<i64> {
    Ok(Self::string_to_date(date_string)?.timestamp_millis())
  }

  /// Converts a string produced by [`time_to_string`](Self::time_to_string) or
  /// [`date_to_string`](Self::date_to_string) back to a date.
  ///
  /// Returns an error if `date_string` is not in the expected format.
  pub fn string_to_date(date_string: &str) -> Result<DateTime<Utc>> {
    parse_date_string(date_string)
  }

  /// Limits a date's resolution.
  ///
  /// For example, `2004-09-21 13:50:11` will be changed to
  /// `2004-09-01 00:00:00` when using [`Resolution::Month`].
  ///
  /// Returns the date with all values more precise than `resolution` set to `0`
  /// or `1`.
  pub fn round_date(date: DateTime<Utc>, resolution: Resolution) -> Result<DateTime<Utc>> {
    date_time_from_millis(Self::round(date.timestamp_millis(), resolution)?)
  }

  /// Limits a millisecond time's resolution.
  ///
  /// For example, `1095767411000` (which represents
  /// `2004-09-21 13:50:11`) will be changed to `1093989600000`
  /// (`2004-09-01 00:00:00`) when using [`Resolution::Month`].
  ///
  /// Returns the date with all values more precise than `resolution` set to `0`
  /// or `1`, expressed as milliseconds since January 1, 1970, 00:00:00 UTC.
  pub fn round(time: i64, resolution: Resolution) -> Result<i64> {
    if resolution == Resolution::Millisecond {
      return Ok(time);
    }

    let date = date_time_from_millis(time)?;
    let year = date.year();
    let month = date.month();
    let day = date.day();
    let hour = date.hour();
    let minute = date.minute();
    let second = date.second();

    let rounded = match resolution {
      Resolution::Year => date_time(year, 1, 1, 0, 0, 0, 0)?,
      Resolution::Month => date_time(year, month, 1, 0, 0, 0, 0)?,
      Resolution::Day => date_time(year, month, day, 0, 0, 0, 0)?,
      Resolution::Hour => date_time(year, month, day, hour, 0, 0, 0)?,
      Resolution::Minute => date_time(year, month, day, hour, minute, 0, 0)?,
      Resolution::Second => date_time(year, month, day, hour, minute, second, 0)?,
      Resolution::Millisecond => date,
    };
    Ok(rounded.timestamp_millis())
  }
}

/// Specifies the time granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resolution {
  /// Limit a date's resolution to year granularity.
  Year,
  /// Limit a date's resolution to month granularity.
  Month,
  /// Limit a date's resolution to day granularity.
  Day,
  /// Limit a date's resolution to hour granularity.
  Hour,
  /// Limit a date's resolution to minute granularity.
  Minute,
  /// Limit a date's resolution to second granularity.
  Second,
  /// Limit a date's resolution to millisecond granularity.
  Millisecond,
}

impl Resolution {
  /// Constant for year granularity.
  pub const YEAR: Resolution = Resolution::Year;
  /// Constant for month granularity.
  pub const MONTH: Resolution = Resolution::Month;
  /// Constant for day granularity.
  pub const DAY: Resolution = Resolution::Day;
  /// Constant for hour granularity.
  pub const HOUR: Resolution = Resolution::Hour;
  /// Constant for minute granularity.
  pub const MINUTE: Resolution = Resolution::Minute;
  /// Constant for second granularity.
  pub const SECOND: Resolution = Resolution::Second;
  /// Constant for millisecond granularity.
  pub const MILLISECOND: Resolution = Resolution::Millisecond;

  /// Returns the length of the date string format for this resolution.
  pub const fn format_len(self) -> usize {
    match self {
      Resolution::Year => 4,
      Resolution::Month => 6,
      Resolution::Day => 8,
      Resolution::Hour => 10,
      Resolution::Minute => 12,
      Resolution::Second => 14,
      Resolution::Millisecond => 17,
    }
  }

  /// Returns all resolutions in increasing precision order.
  pub fn values() -> impl Iterator<Item = Self> {
    [
      Resolution::Year,
      Resolution::Month,
      Resolution::Day,
      Resolution::Hour,
      Resolution::Minute,
      Resolution::Second,
      Resolution::Millisecond,
    ]
    .into_iter()
  }
}

impl Display for Resolution {
  /// Returns the name of the resolution in lowercase, for backwards
  /// compatibility.
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Resolution::Year => write!(f, "year"),
      Resolution::Month => write!(f, "month"),
      Resolution::Day => write!(f, "day"),
      Resolution::Hour => write!(f, "hour"),
      Resolution::Minute => write!(f, "minute"),
      Resolution::Second => write!(f, "second"),
      Resolution::Millisecond => write!(f, "millisecond"),
    }
  }
}

fn date_time_from_millis(time: i64) -> Result<DateTime<Utc>> {
  DateTime::<Utc>::from_timestamp_millis(time)
    .ok_or_else(|| LuceneError::illegal_argument(format!("date is out of range: {time}")))
}

fn date_time(
  year: i32,
  month: u32,
  day: u32,
  hour: u32,
  minute: u32,
  second: u32,
  millisecond: u32,
) -> Result<DateTime<Utc>> {
  NaiveDate::from_ymd_opt(year, month, day)
    .and_then(|date| date.and_hms_milli_opt(hour, minute, second, millisecond))
    .map(|date| date.and_utc())
    .ok_or_else(|| {
      LuceneError::illegal_argument(format!(
        "invalid date fields: year={year}, month={month}, day={day}, hour={hour}, minute={minute}, second={second}, millisecond={millisecond}"
      ))
    })
}

fn format_date(date: DateTime<Utc>, resolution: Resolution) -> Result<String> {
  let year_width = date
    .year()
    .unsigned_abs()
    .checked_ilog10()
    .map_or(1, |log| log as usize + 1)
    + usize::from(date.year() < 0);
  let mut text = String::with_capacity(resolution.format_len() + year_width.max(4) - 4);
  match resolution {
    Resolution::Year => write!(text, "{:04}", date.year()),
    Resolution::Month => write!(text, "{:04}{:02}", date.year(), date.month()),
    Resolution::Day => write!(
      text,
      "{:04}{:02}{:02}",
      date.year(),
      date.month(),
      date.day()
    ),
    Resolution::Hour => write!(
      text,
      "{:04}{:02}{:02}{:02}",
      date.year(),
      date.month(),
      date.day(),
      date.hour()
    ),
    Resolution::Minute => write!(
      text,
      "{:04}{:02}{:02}{:02}{:02}",
      date.year(),
      date.month(),
      date.day(),
      date.hour(),
      date.minute()
    ),
    Resolution::Second => write!(
      text,
      "{:04}{:02}{:02}{:02}{:02}{:02}",
      date.year(),
      date.month(),
      date.day(),
      date.hour(),
      date.minute(),
      date.second()
    ),
    Resolution::Millisecond => write!(
      text,
      "{:04}{:02}{:02}{:02}{:02}{:02}{:03}",
      date.year(),
      date.month(),
      date.day(),
      date.hour(),
      date.minute(),
      date.second(),
      date.timestamp_subsec_millis()
    ),
  }?;
  Ok(text)
}

fn parse_date_string(date_string: &str) -> Result<DateTime<Utc>> {
  match date_string.len() {
    4 | 6 | 8 | 10 | 12 | 14 | 17 => {},
    _ => return Err(invalid_date_string(date_string)),
  }

  if !date_string.chars().all(|ch| ch.is_ascii_digit()) {
    return Err(invalid_date_string(date_string));
  }

  let year = parse_i32(date_string, 0, 4)?;
  let month = parse_or_default(date_string, 4, 6, 1)?;
  let day = parse_or_default(date_string, 6, 8, 1)?;
  let hour = parse_or_default(date_string, 8, 10, 0)?;
  let minute = parse_or_default(date_string, 10, 12, 0)?;
  let second = parse_or_default(date_string, 12, 14, 0)?;
  let millisecond = parse_or_default(date_string, 14, 17, 0)?;

  date_time(year, month, day, hour, minute, second, millisecond)
    .map_err(|_| invalid_date_string(date_string))
}

fn parse_or_default(
  date_string: &str,
  start: usize,
  end: usize,
  default_value: u32,
) -> Result<u32> {
  if date_string.len() >= end {
    parse_u32(date_string, start, end)
  } else {
    Ok(default_value)
  }
}

fn parse_i32(date_string: &str, start: usize, end: usize) -> Result<i32> {
  date_string[start..end]
    .parse::<i32>()
    .map_err(|_| invalid_date_string(date_string))
}

fn parse_u32(date_string: &str, start: usize, end: usize) -> Result<u32> {
  date_string[start..end]
    .parse::<u32>()
    .map_err(|_| invalid_date_string(date_string))
}

fn invalid_date_string(date_string: &str) -> LuceneError {
  LuceneError::illegal_argument(format!("Input is not a valid date string: {date_string}"))
}
