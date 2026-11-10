use rlucene::core::{
    document::{inet_address_point::InetAddressPoint, int_point::IntPoint, long_point::LongPoint},
    index::{
        BytesRefValue, directory_reader,
        doc_values::DocValues,
        doc_values_iterator::DocValuesIterator,
        doc_values_type::DocValuesType,
        index_options::IndexOptions,
        index_reader::IndexReader,
        index_reader_context::IndexReaderContext,
        leaf_reader::LeafReader,
        numeric_doc_values::NumericDocValues,
        point_values::{IntersectVisitor, PointValues, Relation},
        sorted_numeric_doc_values::SortedNumericDocValues,
        sorted_set_doc_values::SortedSetDocValues,
        stored_fields::StoredFields,
        terms::Terms,
        terms_enum::TermsEnum,
    },
    search::{
        doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS},
        index_searcher::from_reader,
    },
    store::mmap_directory::MMapDirectory,
    util::{bytes_ref_iterator::BytesRefIterator, error::lucene_error::Result as LuceneResult},
};
use serde_json::{Value, json};
use std::{
    fs,
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const RAW: &str = "request.raw";

struct Row<'a> {
    id: i64,
    timestamp: i64,
    status: i32,
    size: i32,
    address: [u8; 16],
    request: &'a str,
    source: &'a [u8],
}

fn read_rows(mut input: &[u8]) -> Result<Vec<Row<'_>>> {
    let mut rows = Vec::new();
    while !input.is_empty() {
        let (header, rest) = input.split_at_checked(28).ok_or("truncated header")?;
        let request_len = u16::from_be_bytes(header[24..26].try_into()?) as usize;
        let source_len = u16::from_be_bytes(header[26..28].try_into()?) as usize;
        let (request, rest) = rest
            .split_at_checked(request_len)
            .ok_or("truncated request")?;
        let (source, rest) = rest
            .split_at_checked(source_len)
            .ok_or("truncated source")?;
        let ipv4: [u8; 4] = header[20..24].try_into()?;
        rows.push(Row {
            id: i64::from(u32::from_be_bytes(header[0..4].try_into()?)),
            timestamp: i64::from_be_bytes(header[4..12].try_into()?),
            status: i32::from_be_bytes(header[12..16].try_into()?),
            size: i32::from_be_bytes(header[16..20].try_into()?),
            address: InetAddressPoint::encode(IpAddr::V4(Ipv4Addr::from(ipv4))),
            request: std::str::from_utf8(request)?,
            source,
        });
        input = rest;
    }
    Ok(rows)
}

struct PointAudit<'a> {
    field: &'a str,
    rows: &'a [Row<'a>],
    visited: Vec<bool>,
}

impl IntersectVisitor for PointAudit<'_> {
    fn visit(&mut self, _: i32) -> LuceneResult<()> {
        panic!("expected packed point")
    }

    fn visit_with_packed_value(&mut self, doc: i32, bytes: &[u8]) -> LuceneResult<()> {
        let doc = doc as usize;
        let row = &self.rows[doc];
        assert!(
            !std::mem::replace(&mut self.visited[doc], true),
            "duplicate point"
        );
        match self.field {
            "@timestamp" => assert_eq!(LongPoint::decode_dimension(bytes, 0), row.timestamp),
            "status" => assert_eq!(IntPoint::decode_dimension(bytes, 0), row.status),
            "size" => assert_eq!(IntPoint::decode_dimension(bytes, 0), row.size),
            "clientip" => assert_eq!(bytes, row.address),
            _ => unreachable!(),
        }
        Ok(())
    }

    fn compare(&self, _: &[u8], _: &[u8]) -> LuceneResult<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}

fn audit_columns(reader: &impl LeafReader, rows: &[Row<'_>]) -> Result<()> {
    let mut stored = reader.stored_fields()?;
    let mut row_id = reader
        .get_numeric_doc_values("_row")?
        .ok_or("missing row DV")?;
    let mut timestamp = reader
        .get_sorted_numeric_doc_values("@timestamp")?
        .ok_or("missing timestamp DV")?;
    let mut status = reader
        .get_sorted_numeric_doc_values("status")?
        .ok_or("missing status DV")?;
    let mut size = reader
        .get_sorted_numeric_doc_values("size")?
        .ok_or("missing size DV")?;
    let mut address = reader
        .get_sorted_set_doc_values("clientip")?
        .ok_or("missing IP DV")?;
    let mut raw = DocValues::get_sorted_set(reader, RAW)?;
    let infos = reader.get_field_infos()?;
    let info = infos
        .field_info_by_name(RAW)?
        .ok_or("missing raw field info")?;
    assert_eq!(info.get_index_options(), &IndexOptions::Docs);
    assert_eq!(info.get_doc_values_type(), &DocValuesType::SortedSet);
    assert!(info.omits_norms());

    for (doc, row) in rows.iter().enumerate() {
        let doc = doc as i32;
        let document = stored.document(doc)?;
        let source = document
            .get_binary_value("_source")?
            .ok_or("missing stored source")?;
        assert_eq!(source.as_byte_slice(), row.source, "stored row {}", row.id);
        assert!(document.get_binary_value(RAW)?.is_none());
        assert!(row_id.advance_exact(doc)?);
        assert_eq!(row_id.long_value()?, row.id);
        for (column, value) in [
            (&mut timestamp, row.timestamp),
            (&mut status, i64::from(row.status)),
            (&mut size, i64::from(row.size)),
        ] {
            assert!(column.advance_exact(doc)?);
            assert_eq!(column.doc_value_count()?, 1);
            assert_eq!(column.next_value()?, value);
        }
        assert!(address.advance_exact(doc)?);
        assert_eq!(address.doc_value_count()?, 1);
        let ord = address.next_ord()?;
        assert_eq!(address.lookup_ord(ord)?.as_bytes(), row.address);
        let has_raw = row.request.encode_utf16().count() <= 256;
        assert_eq!(raw.advance_exact(doc)?, has_raw);
        if has_raw {
            assert_eq!(raw.doc_value_count()?, 1);
            let ord = raw.next_ord()?;
            assert_eq!(raw.lookup_ord(ord)?.as_bytes(), row.request.as_bytes());
        }
    }

    for field in ["@timestamp", "status", "size", "clientip"] {
        let points = reader.get_point_values(field)?.ok_or("missing points")?;
        let mut visitor = PointAudit {
            field,
            rows,
            visited: vec![false; rows.len()],
        };
        points.intersect(&mut visitor)?;
        assert!(
            visitor.visited.into_iter().all(|seen| seen),
            "missing {field} point"
        );
    }
    Ok(())
}

fn audit_postings<R: LeafReader>(
    baseline: &R,
    reader: &R,
    field: &str,
    rows: &[Row<'_>],
) -> Result<[u64; 2]> {
    let old_terms = baseline.terms(field)?.ok_or("missing baseline terms")?;
    let new_terms = reader.terms(field)?.ok_or("missing candidate terms")?;
    assert!(!new_terms.has_freqs());
    assert!(!new_terms.has_positions());
    let mut old = old_terms.iterator()?;
    let mut new = new_terms.iterator()?;
    let mut count = [0, 0];
    let mut seen = vec![false; rows.len()];
    let mut old_postings = None;
    let mut new_postings = None;
    loop {
        let raw_term = {
            let a = old.next()?;
            let b = new.next()?;
            assert_eq!(
                a.as_ref().map(|term| term.as_bytes()),
                b.as_ref().map(|term| term.as_bytes())
            );
            let Some(term) = b else { break };
            (field == RAW).then(|| term.as_bytes().to_vec())
        };
        assert_eq!(old.doc_freq()?, new.doc_freq()?);
        let mut a = old.postings(old_postings)?;
        let mut b = new.postings(new_postings)?;
        count[0] += 1;
        loop {
            let doc = b.next_doc()?;
            assert_eq!(a.next_doc()?, doc);
            if doc == NO_MORE_DOCS {
                break;
            }
            if let Some(raw_term) = &raw_term {
                assert!(!std::mem::replace(&mut seen[doc as usize], true));
                assert_eq!(rows[doc as usize].request.as_bytes(), raw_term);
            }
            count[1] += 1;
        }
        old_postings = Some(a);
        new_postings = Some(b);
    }
    if field == RAW {
        for (row, was_seen) in rows.iter().zip(seen) {
            assert_eq!(row.request.encode_utf16().count() <= 256, was_seen);
        }
    }
    Ok(count)
}

fn audit(input: &Path, baseline: &Path, indexes: &[String]) -> Result<()> {
    let input = fs::read(input)?;
    let rows = read_rows(&input)?;
    assert_eq!(rows.len(), 1_000_000);
    let baseline = Arc::new(MMapDirectory::new(baseline)?);
    let baseline = from_reader(Arc::new(directory_reader::open(baseline)?))?;
    let old_leaves = baseline.get_top_reader_context().leaves()?;
    for index in indexes {
        let directory = Arc::new(MMapDirectory::new(Path::new(index))?);
        let reader = Arc::new(directory_reader::open(directory)?);
        assert_eq!(reader.max_doc()? as usize, rows.len());
        assert_eq!(reader.num_docs()?, reader.max_doc()?);
        let searcher = from_reader(reader)?;
        let leaves = searcher.get_top_reader_context().leaves()?;
        assert_eq!(leaves.len(), 16);
        assert_eq!(old_leaves.len(), leaves.len());
        let mut segments = Vec::new();
        let mut postings: [[u64; 2]; 2] = [[0; 2]; 2];
        for (old, new) in old_leaves.iter().zip(leaves) {
            let old_reader = old.reader();
            let reader = new.reader();
            let max = reader.max_doc()?;
            assert_eq!(old_reader.max_doc()?, max);
            assert_eq!(old.doc_base, new.doc_base);
            segments.push(max);
            let leaf_rows = &rows[new.doc_base..new.doc_base + max as usize];
            audit_columns(reader, leaf_rows)?;
            for (field, totals) in ["request", RAW].into_iter().zip(&mut postings) {
                for (total, count) in totals
                    .iter_mut()
                    .zip(audit_postings(old_reader, reader, field, leaf_rows)?)
                {
                    *total += count;
                }
            }
        }
        assert_eq!(segments[..15], [65_536; 15]);
        assert_eq!(segments[15], 16_960);
        let receipt: Value = json!({
            "index": index, "docs": rows.len(), "segments": segments,
            "stored_documents": rows.len(), "doc_values_columns": 6, "point_values": rows.len() * 4,
            "postings": {"request": postings[0], "request.raw": postings[1]},
        });
        println!("{receipt}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, baseline, indexes @ ..] = args.as_slice() else {
        return Err("usage: audit <input> <baseline index> <indexes...>".into());
    };
    audit(Path::new(input), Path::new(baseline), indexes)
}
