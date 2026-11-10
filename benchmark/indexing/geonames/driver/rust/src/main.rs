use rlucene::core::{
    analysis::standard::standard_analyzer::StandardAnalyzer,
    codecs::lucene101_codec::{Lucene101Codec, Mode},
    document::{
        field::{Field, FieldBase, Store},
        field_type::FieldType,
        fields::Fields,
        lat_lon_point::LatLonPoint,
        long_point::LongPoint,
        numeric_doc_values_field::NumericDocValuesField,
        sorted_set_doc_values_field::SortedSetDocValuesField,
        string_field::StringField,
    },
    index::{
        BytesRef, BytesRefValue, directory_reader,
        doc_values::DocValues,
        doc_values_iterator::DocValuesIterator,
        doc_values_type::DocValuesType,
        index_reader::IndexReader,
        index_reader_context::IndexReaderContext,
        index_writer::{IndexWriter, IndexingDocument},
        index_writer_config::{IndexWriterConfig, OpenMode},
        leaf_reader::LeafReader,
        live_index_writer_config::LiveIndexWriterConfig,
        no_merge_policy::NoMergePolicy,
        numeric_doc_values::NumericDocValues,
        point_values::{IntersectVisitor, PointValues, Relation},
        serial_merge_scheduler::SerialMergeScheduler,
        sorted_set_doc_values::SortedSetDocValues,
        stored_fields::StoredFields,
        term::Term,
        two_phase_commit::TwoPhaseCommit,
    },
    search::{
        index_searcher::from_reader,
        match_all_docs_query::MatchAllDocsQuery,
        query::Query,
        sort::Sort,
        sort_field::{SortField, SortFieldType, SortFiledBase},
        term_query::TermQuery,
    },
    store::mmap_directory::MMapDirectory,
    util::{
        close::CloseableRef, error::lucene_error::Result as LuceneResult,
        numeric_utils::NumericUtils,
    },
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
const ID: &str = "geonameid";
const LOCATION: &str = "location";
const POP: &str = "population";
const ALIAS: &str = "alias";

struct Row<'a> {
    id: &'a str,
    latitude: f64,
    longitude: f64,
    encoded: [i32; 2],
    population: i64,
    aliases: Vec<&'a [u8]>,
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8]> {
    let (bytes, rest) = input.split_at_checked(len).ok_or("truncated record")?;
    *input = rest;
    Ok(bytes)
}

fn short(input: &mut &[u8]) -> Result<usize> {
    Ok(u16::from_be_bytes(take(input, 2)?.try_into()?) as usize)
}

fn bytes<'a>(input: &mut &'a [u8]) -> Result<&'a [u8]> {
    let len = short(input)?;
    take(input, len)
}

fn read_rows(mut input: &[u8]) -> Result<Vec<Row<'_>>> {
    let count = u32::from_be_bytes(take(&mut input, 4)?.try_into()?) as usize;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let latitude = f64::from_be_bytes(take(&mut input, 8)?.try_into()?);
        let longitude = f64::from_be_bytes(take(&mut input, 8)?.try_into()?);
        let encoded = [
            i32::from_be_bytes(take(&mut input, 4)?.try_into()?),
            i32::from_be_bytes(take(&mut input, 4)?.try_into()?),
        ];
        let population = i64::from_be_bytes(take(&mut input, 8)?.try_into()?);
        let id = std::str::from_utf8(bytes(&mut input)?)?;
        let aliases = (0..short(&mut input)?)
            .map(|_| bytes(&mut input))
            .collect::<Result<Vec<_>>>()?;
        rows.push(Row {
            id,
            latitude,
            longitude,
            encoded,
            population,
            aliases,
        });
    }
    assert!(input.is_empty(), "trailing input");
    Ok(rows)
}

struct ActiveFields<'a>(&'a mut [Fields]);

impl<'a> IntoIterator for ActiveFields<'a> {
    type Item = &'a mut Fields;
    type IntoIter = std::slice::IterMut<'a, Fields>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl IndexingDocument for ActiveFields<'_> {}

fn write_index(rows: &[Row<'_>], max_aliases: usize, path: &Path) -> Result<Value> {
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

    let mut alias_type = FieldType::new();
    alias_type.set_doc_values_type(DocValuesType::SortedSet)?;
    alias_type.freeze();
    let mut fields: Vec<Fields> = vec![
        LongPoint::new(POP, [0_i64])?.into(),
        NumericDocValuesField::new(POP, 0).into(),
        StringField::from_string(ID, "", Store::Yes)?.into(),
        LatLonPoint::new(LOCATION, 0., 0.)?.into(),
    ];
    fields.extend(
        (0..max_aliases)
            .map(|_| Field::from_bytes_ref(ALIAS, b"", alias_type.clone()).map(Fields::from))
            .collect::<LuceneResult<Vec<_>>>()?,
    );

    let start = Instant::now();
    for row in rows {
        let [
            Fields::LongPoint(pop_point),
            Fields::NumericDocValues(pop_dv),
            Fields::String(id),
            Fields::LatLonPoint(location),
            aliases @ ..,
        ] = fields.as_mut_slice()
        else {
            return Err("unexpected field layout".into());
        };
        let mut begin = 2;
        if row.population >= 0 {
            pop_point.set_long_values([row.population])?;
            pop_dv.set_long_value(row.population)?;
            begin = 0;
        }
        id.set_string_value(row.id)?;
        location.set_location_value(row.latitude, row.longitude)?;
        for (slot, value) in aliases.iter_mut().zip(&row.aliases) {
            let Fields::Field(field) = slot else {
                return Err("unexpected alias slot".into());
            };
            field.set_bytes_value(*value)?;
        }
        writer.add_document(ActiveFields(&mut fields[begin..4 + row.aliases.len()]))?;
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
        json!({"docs": rows.len(), "write_ns": write_ns, "commit_ns": commit_ns,
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
    let rows = read_rows(&input)?;
    let max_aliases = rows.iter().map(|row| row.aliases.len()).max().unwrap_or(0);
    emit(json!({"ready": true, "pid": std::process::id(), "input_bytes": input.len()}))?;
    for line in io::stdin().lock().lines() {
        emit(write_index(&rows, max_aliases, Path::new(&line?))?)?;
    }
    Ok(())
}

struct PointAudit<'a> {
    rows: &'a [Row<'a>],
    geo: bool,
    seen: Vec<bool>,
}

impl IntersectVisitor for PointAudit<'_> {
    fn visit(&mut self, _: i32) -> LuceneResult<()> {
        panic!("expected packed point");
    }

    fn visit_with_packed_value(&mut self, doc: i32, value: &[u8]) -> LuceneResult<()> {
        let doc = doc as usize;
        let row = &self.rows[doc];
        assert!(
            !std::mem::replace(&mut self.seen[doc], true),
            "duplicate point"
        );
        if self.geo {
            assert_eq!(value.len(), 8);
            assert_eq!(
                [
                    NumericUtils::sortable_bytes_to_int(value, 0),
                    NumericUtils::sortable_bytes_to_int(value, 4)
                ],
                row.encoded,
                "{}",
                row.id
            );
        } else {
            assert_eq!(
                LongPoint::decode_dimension(value, 0),
                row.population,
                "{}",
                row.id
            );
        }
        Ok(())
    }

    fn compare(&self, _: &[u8], _: &[u8]) -> LuceneResult<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}

fn audit_leaf(reader: &impl LeafReader, rows: &[Row<'_>]) -> Result<()> {
    let mut stored = reader.stored_fields()?;
    let mut pop = DocValues::get_numeric(reader, POP)?;
    let mut alias = DocValues::get_sorted_set(reader, ALIAS)?;
    for (doc, row) in rows.iter().enumerate() {
        let doc = doc as i32;
        let stored = stored.document(doc)?;
        assert_eq!(stored.get_fields().len(), 1);
        assert_eq!(stored.get(ID)?.as_deref().map(String::as_str), Some(row.id));
        assert_eq!(pop.advance_exact(doc)?, row.population >= 0);
        if row.population >= 0 {
            assert_eq!(pop.long_value()?, row.population);
        }
        let mut expected = row.aliases.clone();
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(alias.advance_exact(doc)?, !expected.is_empty());
        if !expected.is_empty() {
            assert_eq!(alias.doc_value_count()? as usize, expected.len());
            for value in expected {
                let ord = alias.next_ord()?;
                assert_eq!(alias.lookup_ord(ord)?.as_bytes(), value);
            }
        }
    }
    for (field, geo) in [(LOCATION, true), (POP, false)] {
        let mut visit = PointAudit {
            rows,
            geo,
            seen: vec![false; rows.len()],
        };
        if let Some(points) = reader.get_point_values(field)? {
            let dims = if geo { 2 } else { 1 };
            assert_eq!(points.get_num_dimensions()?, dims);
            assert_eq!(points.get_num_index_dimensions()?, dims);
            assert_eq!(points.get_bytes_per_dimension()?, if geo { 4 } else { 8 });
            points.intersect(&mut visit)?;
        }
        assert!(
            visit
                .seen
                .into_iter()
                .zip(rows)
                .all(|(seen, row)| seen == (geo || row.population >= 0))
        );
    }
    Ok(())
}

fn audit(path: &str, input: &str) -> Result<()> {
    let data = fs::read(input)?;
    let rows = read_rows(&data)?;
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

fn query(kind: &str, value: &Value) -> Result<Query> {
    let num = |i: usize| value[i].as_f64().ok_or("expected coordinate or distance");
    let long = |i: usize| value[i].as_i64().ok_or("expected population");
    let text = || value.as_str().ok_or("expected text");
    Ok(match kind {
        "id" => TermQuery::new(Term::from_text(ID, text()?)).into(),
        "box" => LatLonPoint::new_box_query(LOCATION, num(0)?, num(1)?, num(2)?, num(3)?)?,
        "distance" => LatLonPoint::new_distance_query(LOCATION, num(0)?, num(1)?, num(2)?)?.into(),
        "pop" => LongPoint::new_range_query(POP, long(0)?, long(1)?)?.into(),
        "alias" => {
            SortedSetDocValuesField::new_slow_exact_query(ALIAS, Some(BytesRef::from(text()?)))
                .into()
        }
        "all" => MatchAllDocsQuery::new().into(),
        _ => return Err("unknown query".into()),
    })
}

fn verify(path: &str, cases: &str) -> Result<()> {
    let cases: Vec<(String, String, Value, bool)> = serde_json::from_slice(&fs::read(cases)?)?;
    let dir = Arc::new(MMapDirectory::new(Path::new(path))?);
    let reader = Arc::new(directory_reader::open(dir)?);
    let mut searcher = from_reader(reader.clone())?;
    searcher.set_query_cache(None);
    let mut stored = reader.stored_fields()?;
    let mut pop_sort = SortField::with_reverse(Some(POP), SortFieldType::Long, true)?;
    pop_sort.set_missing_value(-1_i64)?;
    let pop_sort = Arc::new(Sort::with_fields([pop_sort])?);
    let index_order = Arc::new(Sort::get_index_order()?);
    let mut results = Vec::new();
    for (name, kind, value, by_population) in cases {
        let query = query(&kind, &value)?;
        let count = searcher.count(query.clone())?;
        let sort = if by_population {
            pop_sort.clone()
        } else {
            index_order.clone()
        };
        let hits = searcher.search_with_sort(query, 10, sort)?;
        let mut rows = Vec::new();
        for hit in hits.base.score_docs {
            let doc = stored.document(hit.doc())?;
            let id = doc.get(ID)?.ok_or("missing geonameid")?.into_owned();
            rows.push(json!([hit.doc(), id]));
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
