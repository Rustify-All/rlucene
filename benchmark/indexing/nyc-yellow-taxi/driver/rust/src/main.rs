use rlucene::core::{
    analysis::standard::standard_analyzer::StandardAnalyzer,
    codecs::lucene101_codec::{Lucene101Codec, Mode},
    document::{
        double_doc_values_field::DoubleDocValuesField, double_point::DoublePoint, field::FieldBase,
        fields::Fields, int_point::IntPoint, long_point::LongPoint,
        numeric_doc_values_field::NumericDocValuesField, stored_field::StoredField,
    },
    index::{
        directory_reader,
        doc_values::DocValues,
        doc_values_iterator::DocValuesIterator,
        index_reader::IndexReader,
        index_reader_context::IndexReaderContext,
        index_writer::{IndexWriter, IndexingDocument},
        index_writer_config::{IndexWriterConfig, OpenMode},
        indexable_field::IndexableField,
        leaf_reader::LeafReader,
        live_index_writer_config::LiveIndexWriterConfig,
        no_merge_policy::NoMergePolicy,
        numeric_doc_values::NumericDocValues,
        point_values::{IntersectVisitor, PointValues, Relation},
        serial_merge_scheduler::SerialMergeScheduler,
        stored_fields::StoredFields,
        two_phase_commit::TwoPhaseCommit,
    },
    search::{
        boolean_clause::Occur,
        boolean_query::Builder,
        field_exists_query::FieldExistsQuery,
        index_searcher::from_reader,
        match_all_docs_query::MatchAllDocsQuery,
        query::Query,
        sort::Sort,
        sort_field::{SortField, SortFieldType, SortFiledBase},
    },
    store::mmap_directory::MMapDirectory,
    util::{close::CloseableRef, error::lucene_error::Result as LuceneResult},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::Path,
    sync::Arc,
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ID: &str = "source_row";
const FIELDS: [&str; 6] = [
    "tpep_pickup_datetime",
    "tpep_dropoff_datetime",
    "PULocationID",
    "DOLocationID",
    "trip_distance",
    "total_amount",
];
const RECORD_BYTES: usize = 49;
const PRESENT: u8 = 63;

#[derive(Clone, Copy)]
struct Row {
    id: i64,
    mask: u8,
    values: [i64; 6],
}

impl Row {
    fn decode(data: &[u8]) -> Self {
        let read_long = |start| i64::from_be_bytes(data[start..start + 8].try_into().unwrap());
        let read_int =
            |start| i32::from_be_bytes(data[start..start + 4].try_into().unwrap()) as i64;
        Self {
            id: read_long(0),
            mask: data[8],
            values: [
                read_long(9),
                read_long(17),
                read_int(25),
                read_int(29),
                read_long(33),
                read_long(41),
            ],
        }
    }

    fn present(self, field: usize) -> bool {
        self.mask & (1 << field) != 0
    }
}

fn records(input: &[u8]) -> Result<(usize, &[u8])> {
    let count = u32::from_be_bytes(input.get(..4).ok_or("missing row count")?.try_into()?) as usize;
    let data = &input[4..];
    if data.len() != count * RECORD_BYTES {
        return Err("invalid input size".into());
    }
    Ok((count, data))
}

struct DenseFields<'a>(&'a mut [Fields]);

impl<'a> IntoIterator for DenseFields<'a> {
    type Item = &'a mut Fields;
    type IntoIter = std::slice::IterMut<'a, Fields>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl IndexingDocument for DenseFields<'_> {}

struct ActiveFields<'a> {
    fields: std::slice::IterMut<'a, Fields>,
    mask: u16,
}

impl<'a> Iterator for ActiveFields<'a> {
    type Item = &'a mut Fields;

    fn next(&mut self) -> Option<Self::Item> {
        if self.mask == 0 {
            return None;
        }
        let skip = self.mask.trailing_zeros();
        self.mask >>= skip + 1;
        self.fields.nth(skip as usize)
    }
}

impl IndexingDocument for ActiveFields<'_> {}

fn write_index(count: usize, data: &[u8], path: &Path) -> Result<Value> {
    let dir = Arc::new(MMapDirectory::new(path)?);
    let mut config = IndexWriterConfig::with_analyzer(StandardAnalyzer::new())?;
    config
        .set_open_mode(OpenMode::Create)
        .set_commit_on_close(false)
        .set_merge_scheduler(SerialMergeScheduler::new())
        .set_ram_buffer_size_mb(256.0)
        .set_max_buffered_docs(65_536)
        .set_use_compound_file(false)
        .set_codec(Lucene101Codec::with_mode(Mode::BestSpeed)?);
    config.set_merge_policy(NoMergePolicy::default());
    let writer = IndexWriter::new(dir.clone(), config)?;

    let mut fields = [
        StoredField::from_i64(ID, 0)?.into(),
        LongPoint::new(ID, [0])?.into(),
        LongPoint::new(FIELDS[0], [0])?.into(),
        NumericDocValuesField::new(FIELDS[0], 0).into(),
        LongPoint::new(FIELDS[1], [0])?.into(),
        NumericDocValuesField::new(FIELDS[1], 0).into(),
        IntPoint::new(FIELDS[2], [0])?.into(),
        NumericDocValuesField::new(FIELDS[2], 0).into(),
        IntPoint::new(FIELDS[3], [0])?.into(),
        NumericDocValuesField::new(FIELDS[3], 0).into(),
        DoublePoint::new(FIELDS[4], [0.0])?.into(),
        DoubleDocValuesField::new(FIELDS[4], 0.0).into(),
        DoublePoint::new(FIELDS[5], [0.0])?.into(),
        DoubleDocValuesField::new(FIELDS[5], 0.0).into(),
    ];
    let mut masks = [3_u16; 64];
    for mask in 0..=PRESENT {
        for field in 0..FIELDS.len() {
            if mask & (1 << field) != 0 {
                masks[mask as usize] |= 3 << (2 + 2 * field);
            }
        }
    }

    let start = Instant::now();
    for record in data.chunks_exact(RECORD_BYTES) {
        let row = Row::decode(record);
        let [
            Fields::Stored(id_stored),
            Fields::LongPoint(id),
            Fields::LongPoint(pickup),
            Fields::NumericDocValues(pickup_dv),
            Fields::LongPoint(dropoff),
            Fields::NumericDocValues(dropoff_dv),
            Fields::IntPoint(pu),
            Fields::NumericDocValues(pu_dv),
            Fields::IntPoint(do_),
            Fields::NumericDocValues(do_dv),
            Fields::DoublePoint(distance),
            Fields::DoubleDocValues(distance_dv),
            Fields::DoublePoint(total),
            Fields::DoubleDocValues(total_dv),
        ] = &mut fields
        else {
            unreachable!("fixed field layout")
        };
        let [a, b, c, d, e, f] = row.values;
        id_stored.set_long_value(row.id)?;
        id.set_long_values([row.id])?;
        if row.present(0) {
            pickup.set_long_values([a])?;
            pickup_dv.set_long_value(a)?;
        }
        if row.present(1) {
            dropoff.set_long_values([b])?;
            dropoff_dv.set_long_value(b)?;
        }
        if row.present(2) {
            pu.set_int_values([c as i32])?;
            pu_dv.set_long_value(c)?;
        }
        if row.present(3) {
            do_.set_int_values([d as i32])?;
            do_dv.set_long_value(d)?;
        }
        if row.present(4) {
            let distance_value = f64::from_bits(e as u64);
            distance.set_double_values([distance_value])?;
            distance_dv.set_double_value(distance_value)?;
        }
        if row.present(5) {
            let total_value = f64::from_bits(f as u64);
            total.set_double_values([total_value])?;
            total_dv.set_double_value(total_value)?;
        }
        if row.mask == PRESENT {
            writer.add_document(DenseFields(&mut fields))?;
        } else {
            writer.add_document(ActiveFields {
                fields: fields.iter_mut(),
                mask: masks[row.mask as usize],
            })?;
        }
    }
    let write_ns = start.elapsed().as_nanos() as u64;
    let start = Instant::now();
    writer.commit()?;
    let commit_ns = start.elapsed().as_nanos() as u64;
    let start = Instant::now();
    writer.close()?;
    let close_ns = start.elapsed().as_nanos() as u64;
    dir.close()?;
    Ok(
        json!({"docs": count, "write_ns": write_ns, "commit_ns": commit_ns,
              "close_ns": close_ns, "total_ns": write_ns + commit_ns + close_ns}),
    )
}

fn emit(value: Value) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn write_server(path: &str) -> Result<()> {
    let input = fs::read(path)?;
    let (count, data) = records(&input)?;
    emit(json!({"ready": true, "pid": std::process::id(), "input_bytes": input.len()}))?;
    for line in io::stdin().lock().lines() {
        emit(write_index(count, data, Path::new(&line?))?)?;
    }
    Ok(())
}

fn stored_id(stored: &rlucene::core::document::document::Document) -> Result<i64> {
    assert_eq!(stored.get_fields().len(), 1);
    let field = stored.get_field(ID).ok_or("missing stored ID")?;
    field
        .numeric_value()?
        .and_then(|v| v.to_i64())
        .ok_or_else(|| "non-numeric ID".into())
}

fn point_value(row: Row, field: usize) -> Option<i64> {
    if field == FIELDS.len() {
        return Some(row.id);
    }
    if !row.present(field) {
        return None;
    }
    let value = row.values[field];
    // DoublePoint orders all NaNs at the canonical positive quiet NaN.
    if field >= 4 && f64::from_bits(value as u64).is_nan() {
        return Some(0x7ff8_0000_0000_0000);
    }
    Some(value)
}

struct PointAudit<'a> {
    rows: &'a [Row],
    field: usize,
    seen: Vec<bool>,
}

impl IntersectVisitor for PointAudit<'_> {
    fn visit(&mut self, _: i32) -> LuceneResult<()> {
        panic!("expected packed point");
    }

    fn visit_with_packed_value(&mut self, doc: i32, value: &[u8]) -> LuceneResult<()> {
        let doc = doc as usize;
        assert!(
            !std::mem::replace(&mut self.seen[doc], true),
            "duplicate point"
        );
        let actual = match self.field {
            2 | 3 => IntPoint::decode_dimension(value, 0) as i64,
            4 | 5 => DoublePoint::decode_dimension(value, 0).to_bits() as i64,
            _ => LongPoint::decode_dimension(value, 0),
        };
        assert_eq!(
            Some(actual),
            point_value(self.rows[doc], self.field),
            "point {} at source {}",
            self.field,
            self.rows[doc].id
        );
        Ok(())
    }

    fn compare(&self, _: &[u8], _: &[u8]) -> LuceneResult<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}

fn audit_leaf(reader: &impl LeafReader, rows: &[Row]) -> Result<()> {
    let mut stored = reader.stored_fields()?;
    for (doc, row) in rows.iter().enumerate() {
        assert_eq!(stored_id(&stored.document(doc as i32)?)?, row.id);
    }
    for (field, name) in FIELDS.into_iter().chain([ID]).enumerate() {
        if field < FIELDS.len() {
            let mut dv = DocValues::get_numeric(reader, name)?;
            for (doc, row) in rows.iter().enumerate() {
                assert_eq!(
                    dv.advance_exact(doc as i32)?,
                    row.present(field),
                    "DV presence {name}"
                );
                if row.present(field) {
                    assert_eq!(
                        dv.long_value()?,
                        row.values[field],
                        "DV {name} source {}",
                        row.id
                    );
                }
            }
        }
        let mut visit = PointAudit {
            rows,
            field,
            seen: vec![false; rows.len()],
        };
        if let Some(points) = reader.get_point_values(name)? {
            assert_eq!(points.get_num_dimensions()?, 1);
            assert_eq!(points.get_num_index_dimensions()?, 1);
            assert_eq!(
                points.get_bytes_per_dimension()?,
                if matches!(field, 2 | 3) { 4 } else { 8 }
            );
            points.intersect(&mut visit)?;
        }
        for (seen, row) in visit.seen.into_iter().zip(rows) {
            assert_eq!(
                seen,
                point_value(*row, field).is_some(),
                "point presence {name}"
            );
        }
    }
    Ok(())
}

fn audit(path: &str, input: &str) -> Result<()> {
    let data = fs::read(input)?;
    let (_, data) = records(&data)?;
    let rows: Vec<_> = data.chunks_exact(RECORD_BYTES).map(Row::decode).collect();
    let dir = Arc::new(MMapDirectory::new(Path::new(path))?);
    let reader = Arc::new(directory_reader::open(dir)?);
    let ctx = reader.clone().get_context()?;
    let mut offset = 0;
    let mut segments = Vec::new();
    for leaf in ctx.leaves()? {
        let count = leaf.reader().max_doc()? as usize;
        audit_leaf(leaf.reader(), &rows[offset..offset + count])?;
        segments.push(count);
        offset += count;
    }
    assert_eq!(offset, rows.len());
    assert_eq!(reader.num_docs()? as usize, rows.len());
    emit(json!({"docs": offset, "segments": segments}))
}

type Clause = (usize, Option<i64>, Option<i64>);
type Case = (String, Vec<Clause>, Vec<(usize, bool)>);

fn range(field: usize, lo: i64, hi: i64) -> Result<Query> {
    Ok(match field {
        2 | 3 => IntPoint::new_range_query(FIELDS[field], lo.try_into()?, hi.try_into()?)?.into(),
        4 | 5 => DoublePoint::new_range_query(
            FIELDS[field],
            f64::from_bits(lo as u64),
            f64::from_bits(hi as u64),
        )?
        .into(),
        6 => LongPoint::new_range_query(ID, lo, hi)?.into(),
        _ => LongPoint::new_range_query(FIELDS[field], lo, hi)?.into(),
    })
}

fn query(clauses: &[Clause]) -> Result<Query> {
    if clauses.is_empty() {
        return Ok(MatchAllDocsQuery::new().into());
    }
    if let [(field, Some(lo), Some(hi))] = clauses {
        return range(*field, *lo, *hi);
    }
    let mut builder = Builder::new();
    if clauses.iter().all(|(_, lo, _)| lo.is_none()) {
        builder.add(MatchAllDocsQuery::new(), Occur::Filter)?;
    }
    for &(field, lo, hi) in clauses {
        match (lo, hi) {
            (Some(lo), Some(hi)) => builder.add(range(field, lo, hi)?, Occur::Filter)?,
            (None, None) => builder.add(FieldExistsQuery::new(FIELDS[field]), Occur::MustNot)?,
            _ => return Err("incomplete range".into()),
        };
    }
    Ok(builder.build().into())
}

fn sort(fields: &[(usize, bool)]) -> Result<Sort> {
    if fields.is_empty() {
        return Ok(Sort::get_index_order()?);
    }
    let mut sorts = Vec::with_capacity(fields.len());
    for &(field, reverse) in fields {
        let sort_type = match field {
            0 | 1 => SortFieldType::Long,
            2 | 3 => SortFieldType::Int,
            _ => SortFieldType::Double,
        };
        let mut sort = SortField::with_reverse(Some(FIELDS[field]), sort_type, reverse)?;
        match field {
            0 | 1 => sort.set_missing_value(i64::MAX)?,
            2 | 3 => sort.set_missing_value(i32::MAX)?,
            _ => sort.set_missing_value(0.0_f64)?,
        }
        sorts.push(sort);
    }
    Ok(Sort::with_fields(sorts)?)
}

fn verify(path: &str, cases: &str) -> Result<()> {
    let cases: Vec<Case> = serde_json::from_slice(&fs::read(cases)?)?;
    let dir = Arc::new(MMapDirectory::new(Path::new(path))?);
    let reader = Arc::new(directory_reader::open(dir)?);
    let mut searcher = from_reader(reader.clone())?;
    searcher.set_query_cache(None);
    let mut stored = reader.stored_fields()?;
    let mut results = Vec::new();
    for (name, clauses, fields) in cases {
        let query = query(&clauses)?;
        let count = searcher.count(query.clone())?;
        let hits = searcher.search_with_sort(query, 10, Arc::new(sort(&fields)?))?;
        let mut rows = Vec::new();
        for hit in hits.base.score_docs {
            rows.push(json!([hit.doc(), stored_id(&stored.document(hit.doc())?)?]));
        }
        results.push(json!([name, count, rows]));
    }
    let ctx = reader.clone().get_context()?;
    let segments = ctx
        .leaves()?
        .iter()
        .map(|leaf| leaf.reader().max_doc())
        .collect::<LuceneResult<Vec<_>>>()?;
    emit(
        json!({"docs": reader.max_doc()?, "live": reader.num_docs()?,
                "segments": segments, "results": results}),
    )
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.as_slice() {
        [_, mode, path] if mode == "reuse" => write_server(path),
        [_, mode, path, input] if mode == "audit" => audit(path, input),
        [_, mode, path, cases] if mode == "query" => verify(path, cases),
        _ => Err("usage: reuse input | audit index input | query index cases".into()),
    }
}
