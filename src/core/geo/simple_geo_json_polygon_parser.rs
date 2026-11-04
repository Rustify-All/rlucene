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
use std::borrow::Cow;

use crate::core::geo::polygon::Polygon;
use crate::core::util::error::lucene_error::{LuceneError, Result};

#[derive(Clone, Debug)]
enum JsonValue {
  Array(Vec<JsonValue>),
  Bool(bool),
  Null,
  Number(f64),
  String(String),
}

pub struct SimpleGeoJSONPolygonParser<'a> {
  input: &'a str,
  upto: usize,
  poly_type: Option<&'static str>,
  coordinates: Option<Vec<JsonValue>>,
}

impl<'a> SimpleGeoJSONPolygonParser<'a> {
  pub fn new(input: &'a str) -> Self {
    Self {
      input,
      upto: 0,
      poly_type: None,
      coordinates: None,
    }
  }

  pub fn parse(&mut self) -> Result<Vec<Polygon>> {
    self.parse_object("")?;
    self.read_end()?;

    let coordinates = self
      .coordinates
      .take()
      .ok_or_else(|| self.new_parse_error("did not see any polygon coordinates"))?;
    let poly_type = self
      .poly_type
      .take()
      .ok_or_else(|| self.new_parse_error("did not see type: Polygon or MultiPolygon"))?;

    if poly_type == "Polygon" {
      Ok(vec![self.parse_polygon(&coordinates)?])
    } else {
      let mut polygons = Vec::with_capacity(coordinates.len());
      for coordinate in &coordinates {
        let coordinate = match coordinate {
          JsonValue::Array(items) => items,
          _ => {
            return Err(self.new_parse_error(format_args!(
              "elements of coordinates array should be an array, but got: {}",
              coordinate.type_name()
            )));
          },
        };
        polygons.push(self.parse_polygon(coordinate)?);
      }
      Ok(polygons)
    }
  }

  fn parse_object(&mut self, path: &str) -> Result<()> {
    self.scan_char('{')?;
    let mut first = true;
    loop {
      let ch = self.peek()?;
      if ch == '}' {
        break;
      } else if !first {
        if ch == ',' {
          self.upto += 1;
          if self.peek()? == '}' {
            break;
          }
        } else {
          return Err(self.new_parse_error(format_args!("expected , but got {}", ch)));
        }
      }

      first = false;

      let upto_start = self.upto;
      let key = self.parse_string()?;

      if path == "crs.properties" && key == "href" {
        self.upto = upto_start;
        return Err(self.new_parse_error("cannot handle linked crs"));
      }

      self.scan_char(':')?;

      let upto_start = self.upto;
      let ch = self.peek()?;

      let value = if ch == '[' {
        let new_path = if path.is_empty() {
          Cow::Borrowed(key.as_str())
        } else {
          Cow::Owned(format!("{}.{}", path, key))
        };
        self.parse_array(&new_path)?
      } else if ch == '{' {
        let new_path = if path.is_empty() {
          Cow::Borrowed(key.as_str())
        } else {
          Cow::Owned(format!("{}.{}", path, key))
        };
        self.parse_object(&new_path)?;
        JsonValue::Null
      } else if ch == '"' {
        JsonValue::String(self.parse_string()?)
      } else if ch == 't' {
        self.scan_string("true")?;
        JsonValue::Bool(true)
      } else if ch == 'f' {
        self.scan_string("false")?;
        JsonValue::Bool(false)
      } else if ch == 'n' {
        self.scan_string("null")?;
        JsonValue::Null
      } else if ch == '-' || ch == '.' || ch.is_ascii_digit() {
        JsonValue::Number(self.parse_number()?)
      } else if ch == '}' {
        break;
      } else {
        return Err(self.new_parse_error(format_args!(
          "expected array, object, string or literal value, but got: {}",
          ch
        )));
      };

      if path == "crs.properties" && key == "name" {
        let crs = match &value {
          JsonValue::String(crs) => crs,
          _ => {
            self.upto = upto_start;
            return Err(self.new_parse_error(format_args!(
              "crs.properties.name should be a string, but saw: {}",
              value.type_name()
            )));
          },
        };
        if !crs.starts_with("urn:ogc:def:crs:OGC") || !crs.ends_with(":CRS84") {
          self.upto = upto_start;
          return Err(
            self.new_parse_error(format_args!("crs must be CRS84 from OGC, but saw: {}", crs)),
          );
        }
      }

      if key == "type" && !path.starts_with("crs") {
        let type_name = match &value {
          JsonValue::String(type_name) => type_name,
          _ => {
            self.upto = upto_start;
            return Err(self.new_parse_error(format_args!(
              "type should be a string, but got: {}",
              value.type_name()
            )));
          },
        };

        if type_name == "Polygon" && Self::is_valid_geometry_path(path) {
          self.poly_type = Some("Polygon");
        } else if type_name == "MultiPolygon" && Self::is_valid_geometry_path(path) {
          self.poly_type = Some("MultiPolygon");
        } else if (type_name == "FeatureCollection" || type_name == "Feature")
          && (path == "features.[]" || path.is_empty())
        {
        } else {
          self.upto = upto_start;
          return Err(self.new_parse_error(format_args!(
            "can only handle type FeatureCollection (if it has a single polygon geometry), Feature, Polygon or MultiPolygon, but got {}",
            type_name
          )));
        }
      } else if key == "coordinates" && Self::is_valid_geometry_path(path) {
        let coordinates = match value {
          JsonValue::Array(items) => items,
          other => {
            self.upto = upto_start;
            return Err(self.new_parse_error(format_args!(
              "coordinates should be an array, but got: {}",
              other.type_name()
            )));
          },
        };

        if self.coordinates.is_some() {
          self.upto = upto_start;
          return Err(self.new_parse_error("only one Polygon or MultiPolygon is supported"));
        }
        self.coordinates = Some(coordinates);
      }
    }

    self.scan_char('}')?;
    Ok(())
  }

  fn is_valid_geometry_path(path: &str) -> bool {
    path.is_empty() || path == "geometry" || path == "features.[].geometry"
  }

  fn parse_polygon(&self, coordinates: &[JsonValue]) -> Result<Polygon> {
    let mut holes = Vec::with_capacity(coordinates.len().saturating_sub(1));
    let first = coordinates
      .first()
      .ok_or_else(|| self.new_parse_error("polygon array must not be empty"))?;

    let poly_points = match first {
      JsonValue::Array(items) => self.parse_points(items)?,
      _ => {
        return Err(self.new_parse_error(format_args!(
          "first element of polygon array must be an array [[lat, lon], [lat, lon] ...] but got: {}",
          first.type_name()
        )));
      },
    };

    for coordinate in &coordinates[1..] {
      let hole_points = match coordinate {
        JsonValue::Array(items) => self.parse_points(items)?,
        _ => {
          return Err(self.new_parse_error(format_args!(
            "elements of coordinates array must be an array [[lat, lon], [lat, lon] ...] but got: {}",
            coordinate.type_name()
          )));
        },
      };
      holes.push(Polygon::new(hole_points.0, hole_points.1, vec![])?);
    }

    Polygon::new(poly_points.0, poly_points.1, holes)
  }

  fn parse_points(&self, values: &[JsonValue]) -> Result<(Vec<f64>, Vec<f64>)> {
    let mut lats = Vec::with_capacity(values.len());
    let mut lons = Vec::with_capacity(values.len());

    for point in values {
      let point = match point {
        JsonValue::Array(items) => items,
        _ => {
          return Err(self.new_parse_error(format_args!(
            "elements of coordinates array must [lat, lon] array, but got: {}",
            point.type_name()
          )));
        },
      };

      if point.len() != 2 {
        return Err(self.new_parse_error(format_args!(
          "elements of coordinates array must [lat, lon] array, but got wrong element count: {}",
          point.len()
        )));
      }

      let lon = match &point[0] {
        JsonValue::Number(value) => *value,
        _ => {
          return Err(self.new_parse_error(format_args!(
            "elements of coordinates array must [lat, lon] array, but first element is not a Double: {}",
            point[0].type_name()
          )));
        },
      };

      let lat = match &point[1] {
        JsonValue::Number(value) => *value,
        _ => {
          return Err(self.new_parse_error(format_args!(
            "elements of coordinates array must [lat, lon] array, but second element is not a Double: {}",
            point[1].type_name()
          )));
        },
      };

      lons.push(lon);
      lats.push(lat);
    }

    Ok((lats, lons))
  }

  fn parse_array(&mut self, path: &str) -> Result<JsonValue> {
    let mut result = Vec::new();
    let mut child_path = None;
    self.scan_char('[')?;
    while self.upto < self.input.len() {
      let ch = self.peek()?;
      if ch == ']' {
        self.scan_char(']')?;
        return Ok(JsonValue::Array(result));
      }

      if !result.is_empty() {
        if ch != ',' {
          return Err(self.new_parse_error(format_args!(
            "expected ',' separating list items, but got '{}'",
            ch
          )));
        }
        self.upto += 1;

        if self.upto == self.input.len() {
          return Err(self.new_parse_error("hit EOF while parsing array"));
        }
      }

      let ch = self.peek()?;
      let value = if ch == '[' {
        self.parse_array(child_path.get_or_insert_with(|| format!("{}.[]", path)))?
      } else if ch == '{' {
        self.parse_object(child_path.get_or_insert_with(|| format!("{}.[]", path)))?;
        JsonValue::Null
      } else if ch == '-' || ch == '.' || ch.is_ascii_digit() {
        JsonValue::Number(self.parse_number()?)
      } else if ch == '"' {
        JsonValue::String(self.parse_string()?)
      } else {
        return Err(self.new_parse_error(format_args!(
          "expected another array or number while parsing array, not '{}'",
          ch
        )));
      };

      result.push(value);
    }

    Err(self.new_parse_error("hit EOF while reading array"))
  }

  fn parse_number(&mut self) -> Result<f64> {
    let start = self.upto;
    while self.upto < self.input.len() {
      let ch = self.input.as_bytes()[self.upto] as char;
      if ch == '-' || ch == '.' || ch.is_ascii_digit() || ch == 'e' || ch == 'E' {
        self.upto += 1;
      } else {
        break;
      }
    }

    self.input[start..self.upto].parse::<f64>().map_err(|_| {
      self.upto = start;
      self.new_parse_error("could not parse number as double")
    })
  }

  fn parse_string(&mut self) -> Result<String> {
    self.scan_char('"')?;
    if let Some(length) = self.input.as_bytes()[self.upto..]
      .iter()
      .position(|byte| *byte == b'"' || *byte == b'\\' || !byte.is_ascii())
      && self.input.as_bytes()[self.upto + length] == b'"'
    {
      let value = self.input[self.upto..self.upto + length].to_owned();
      self.upto += length + 1;
      return Ok(value);
    }
    let mut result = String::new();
    while self.upto < self.input.len() {
      let ch = self.input.as_bytes()[self.upto] as char;
      if ch == '"' {
        self.upto += 1;
        return Ok(result);
      }

      if ch == '\\' {
        self.upto += 1;
        if self.upto == self.input.len() {
          return Err(self.new_parse_error("hit EOF inside string literal"));
        }

        let escaped = self.input.as_bytes()[self.upto] as char;
        if escaped == 'u' {
          self.upto += 1;
          if self.upto + 4 > self.input.len() {
            return Err(self.new_parse_error("hit EOF inside string literal"));
          }
          let hex = &self.input[self.upto..self.upto + 4];
          let codepoint = u32::from_str_radix(hex, 16)
            .map_err(|_| self.new_parse_error("invalid unicode escape in string literal"))?;
          let ch = char::from_u32(codepoint)
            .ok_or_else(|| self.new_parse_error("invalid unicode escape in string literal"))?;
          result.push(ch);
          self.upto += 4;
        } else if escaped == '\\' {
          result.push('\\');
          self.upto += 1;
        } else {
          return Err(self.new_parse_error(format_args!(
            "unsupported string escape character \\{}",
            escaped
          )));
        }
      } else {
        result.push(ch);
        self.upto += 1;
      }
    }

    Err(self.new_parse_error("hit EOF inside string literal"))
  }

  fn peek(&mut self) -> Result<char> {
    while self.upto < self.input.len() {
      let ch = self.input.as_bytes()[self.upto] as char;
      if Self::is_json_whitespace(ch) {
        self.upto += 1;
        continue;
      }
      return Ok(ch);
    }

    Err(self.new_parse_error("unexpected EOF"))
  }

  fn scan_char(&mut self, expected: char) -> Result<()> {
    while self.upto < self.input.len() {
      let ch = self.input.as_bytes()[self.upto] as char;
      if Self::is_json_whitespace(ch) {
        self.upto += 1;
        continue;
      }
      if ch != expected {
        return Err(self.new_parse_error(format_args!("expected '{}' but got '{}'", expected, ch)));
      }
      self.upto += 1;
      return Ok(());
    }

    Err(self.new_parse_error(format_args!("expected '{}' but got EOF", expected)))
  }

  fn scan_string(&mut self, expected: &str) -> Result<()> {
    if self.upto + expected.len() > self.input.len() {
      return Err(self.new_parse_error(format_args!("expected \"{}\" but hit EOF", expected)));
    }

    let substring = &self.input[self.upto..self.upto + expected.len()];
    if substring != expected {
      return Err(self.new_parse_error(format_args!(
        "expected \"{}\" but got \"{}\"",
        expected, substring
      )));
    }
    self.upto += expected.len();
    Ok(())
  }

  fn read_end(&mut self) -> Result<()> {
    while self.upto < self.input.len() {
      let ch = self.input.as_bytes()[self.upto] as char;
      if !Self::is_json_whitespace(ch) {
        return Err(self.new_parse_error(format_args!(
          "unexpected character '{}' after end of GeoJSON object",
          ch
        )));
      }
      self.upto += 1;
    }
    Ok(())
  }

  fn is_json_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r')
  }

  fn new_parse_error<T>(&self, details: T) -> LuceneError
  where
    T: std::fmt::Display,
  {
    let end = self.input.len().min(self.upto + 1);
    let (prefix, fragment) = if self.upto < 50 {
      ("", &self.input[..end])
    } else {
      ("...", &self.input[self.upto - 50..end])
    };

    LuceneError::illegal_state(format!(
      "{} at character offset {}; fragment leading to this:\n{}{}",
      details, self.upto, prefix, fragment
    ))
  }
}

impl JsonValue {
  fn type_name(&self) -> &'static str {
    match self {
      JsonValue::Array(_) => "array",
      JsonValue::Bool(value) => {
        let _ = value;
        "boolean"
      },
      JsonValue::Null => "null",
      JsonValue::Number(_) => "number",
      JsonValue::String(_) => "string",
    }
  }
}
