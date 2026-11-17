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
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::util::error::lucene_error::{LuceneError, Result};

/**
 * Base trait for rate-limiting I/O. Implementations are typically shared across multiple
 * IndexInputs or IndexOutputs (for example those involved all merging). Those IndexInputs and
 * IndexOutputs would call [`RateLimiter::pause`] whenever the have read or written more than
 * [`RateLimiter::get_min_pause_check_bytes`] bytes.
 */
pub trait RateLimiter: Send + Sync {
  /**
   * Sets an updated MB-per-second rate limit. An implementation may dynamically update
   * the rate limit during use.
   */
  fn set_mb_per_sec(&self, mb_per_sec: f64) -> Result<()>;

  /** The current MB per second rate limit. */
  fn get_mb_per_sec(&self) -> f64;

  /**
   * Pauses, if necessary, to keep the instantaneous IO rate at or below the target.
   *
   * Note: the implementation is thread-safe.
   *
   * Returns the pause time in nanoseconds.
   */
  fn pause(&self, bytes: i64) -> Result<i64>;

  /**
   * How many bytes caller should add up itself before invoking [`RateLimiter::pause`]. NOTE: The value
   * returned by this method may change over time and is not guaranteed to be constant throughout
   * the lifetime of the RateLimiter. Users are advised to refresh their local values with calls to
   * this method to ensure consistency.
   */
  fn get_min_pause_check_bytes(&self) -> i64;
}

impl<R> RateLimiter for Arc<R>
where
  R: RateLimiter + ?Sized,
{
  fn set_mb_per_sec(&self, mb_per_sec: f64) -> Result<()> {
    (**self).set_mb_per_sec(mb_per_sec)
  }

  fn get_mb_per_sec(&self) -> f64 {
    (**self).get_mb_per_sec()
  }

  fn pause(&self, bytes: i64) -> Result<i64> {
    (**self).pause(bytes)
  }

  fn get_min_pause_check_bytes(&self) -> i64 {
    (**self).get_min_pause_check_bytes()
  }
}

const MIN_PAUSE_CHECK_MSEC: i64 = 5;

/** Simple rate limiter for I/O. */
pub struct SimpleRateLimiter {
  mb_per_sec: AtomicU64,
  min_pause_check_bytes: AtomicI64,
  time_origin: Instant,
  last_ns: Mutex<i64>,
}

impl SimpleRateLimiter {
  /** `mb_per_sec` is the maximum I/O rate in MB/s. */
  pub fn new(mb_per_sec: f64) -> Self {
    let limiter = Self {
      mb_per_sec: AtomicU64::new(0),
      min_pause_check_bytes: AtomicI64::new(0),
      time_origin: Instant::now(),
      last_ns: Mutex::new(1),
    };
    // Safe: initialized with a valid MB-per-second rate; ignore the error.
    let _ = limiter.set_mb_per_sec(mb_per_sec);
    limiter
  }
}

impl RateLimiter for SimpleRateLimiter {
  /** Sets an updated mb per second rate limit. */
  fn set_mb_per_sec(&self, mb_per_sec: f64) -> Result<()> {
    self
      .mb_per_sec
      .store(mb_per_sec.to_bits(), Ordering::SeqCst);
    let min_pause_check =
      ((MIN_PAUSE_CHECK_MSEC as f64 / 1000.0) * mb_per_sec * 1024.0 * 1024.0) as i64;
    self
      .min_pause_check_bytes
      .store(min_pause_check, Ordering::SeqCst);
    Ok(())
  }

  fn get_min_pause_check_bytes(&self) -> i64 {
    self.min_pause_check_bytes.load(Ordering::SeqCst)
  }

  /** The current mb per second rate limit. */
  fn get_mb_per_sec(&self) -> f64 {
    f64::from_bits(self.mb_per_sec.load(Ordering::SeqCst))
  }

  /**
   * Pauses, if necessary, to keep the instantaneous IO rate at or below the target. Be sure to
   * only call this method when bytes &gt; [`RateLimiter::get_min_pause_check_bytes`], otherwise it will pause
   * way too long!
   *
   * Returns the pause time in nanoseconds.
   */
  fn pause(&self, bytes: i64) -> Result<i64> {
    // Use a signed nanosecond clock to retain saturating conversion and wrapping arithmetic
    // for NaN, negative rates and infinite pause lengths. The clock origin is arbitrary;
    // start at one nanosecond, including on coarse-resolution clocks.
    let start_ns = (self.time_origin.elapsed().as_nanos() as i64).wrapping_add(1);

    let seconds_to_pause = (bytes as f64 / 1024.0 / 1024.0) / self.get_mb_per_sec();

    let target_ns;

    // Sync'd to read + write last_ns:
    {
      let mut last = self
        .last_ns
        .lock()
        .map_err(|_| LuceneError::illegal_state("rate limiter state lock is poisoned"))?;

      // Time we should sleep until; this is purely instantaneous
      // rate (just adds seconds onto the last time we had paused to);
      // maybe we should also offer decayed recent history one?
      target_ns = last.wrapping_add((1_000_000_000.0 * seconds_to_pause) as i64);

      if start_ns >= target_ns {
        // OK, current time is already beyond the target sleep time,
        // no pausing to do.

        // Set to start, not target, to enforce the instant rate, not
        // the "averaaged over all history" rate:
        *last = start_ns;
        return Ok(0);
      }

      *last = target_ns;
    }

    let mut cur_ns = start_ns;

    // Sleeping must not consume an unpark token belonging to the caller.
    // Keep checking the remaining duration to preserve the rate-limiting loop.
    loop {
      let pause_ns = target_ns.wrapping_sub(cur_ns);
      if pause_ns > 0 {
        // The minimum practical sleep duration on a general-purpose runtime
        // is 1 msec; if you pass just 1 nsec the default impl rounds
        // this up to 1 msec:
        thread::sleep(Duration::from_nanos(pause_ns as u64));
        cur_ns = (self.time_origin.elapsed().as_nanos() as i64).wrapping_add(1);
        continue;
      }
      break;
    }

    Ok(cur_ns.wrapping_sub(start_ns))
  }
}
