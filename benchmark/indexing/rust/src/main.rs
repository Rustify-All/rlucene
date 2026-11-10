use rlucene::core::{
    document::{
        document::Document,
        field::{Field, FieldBase, Store},
        fields::Fields,
        field_type::FieldType,
        inet_address_point::InetAddressPoint,
        int_point::IntPoint,
        long_point::LongPoint,
        numeric_doc_values_field::NumericDocValuesField,
        stored_field::StoredField,
        keyword_field::KeywordField,
    },
    index::{
        directory_reader,
        doc_values_type::DocValuesType,
        BytesRef,
        index_options::IndexOptions,
        index_reader::IndexReader,
        index_writer::IndexWriter,
        index_writer_config::{IndexWriterConfig, OpenMode},
        live_index_writer_config::LiveIndexWriterConfig,
        no_merge_policy::NoMergePolicy,
        serial_merge_scheduler::SerialMergeScheduler,
        stored_fields::StoredFields,
        term::Term,
        two_phase_commit::TwoPhaseCommit,
    },
    search::{
        boolean_clause::Occur,
        boolean_query::Builder,
        field_doc::{FieldDoc, FieldsValue},
        index_searcher::from_reader,
        match_all_docs_query::MatchAllDocsQuery,
        query::Query,
        sort::Sort,
        sort_field::{SortField, SortFieldType},
        sort_field_enum::SortFieldEnum,
        sorted_numeric_sort_field::SortedNumericSortField,
        term_query::TermQuery,
        top_field_collector_manager::TopFieldCollectorManager,
        top_score_doc_collector_manager::TopScoreDocCollectorManager,
    },
    store::mmap_directory::MMapDirectory,
    util::close::CloseableRef,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, Write},
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const TS: &str = "@timestamp";
const ROW: &str = "_row";
const RAW: &str = "request.raw";
const DOCS: usize = 1_000_000;
const TOP: usize = 10;
const PAGES: usize = 4;
const RAM_MB: f64 = 256.0;
const BUFFERED_DOCS: i32 = 65_536;
const THRESHOLD: usize = 10_000;
const CASES: [&str; 17] = [
    "all",
    "term",
    "range200",
    "range400",
    "asc",
    "desc",
    "official_asc",
    "official_desc",
    "sample_asc",
    "sample_desc",
    "stable_asc",
    "stable_desc",
    "count_all",
    "count_term",
    "count_range200",
    "count_range400",
    "count_range",
];

fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

fn emit(value: Value) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N]> {
    let (head, rest) = bytes.split_at_checked(N).ok_or("truncated record")?;
    *bytes = rest;
    Ok(head.try_into()?)
}

#[derive(Clone, Copy)]
enum WriteMode { Fresh, Reuse }

struct Row<'a> {
    row: i64, millis: i64, status: i32, size: i32,
    ip: IpAddr, request: &'a str, source: &'a [u8],
}
impl Row<'_> {
    fn raw(&self) -> bool { self.request.encode_utf16().count() <= 256 }
}

fn read_row<'a>(bytes: &mut &'a [u8]) -> Result<Row<'a>> {
    let row = i64::from(u32::from_be_bytes(take(bytes)?));
    let millis = i64::from_be_bytes(take(bytes)?);
    let status = i32::from_be_bytes(take(bytes)?);
    let size = i32::from_be_bytes(take(bytes)?);
    let ip = IpAddr::V4(Ipv4Addr::from(take::<4>(bytes)?));
    let request_len = u16::from_be_bytes(take(bytes)?) as usize;
    let source_len = u16::from_be_bytes(take(bytes)?) as usize;
    let (request, rest) = bytes.split_at_checked(request_len).ok_or("truncated request")?;
    let (source, rest) = rest.split_at_checked(source_len).ok_or("truncated source")?;
    *bytes = rest;
    Ok(Row { row, millis, status, size, ip, request: std::str::from_utf8(request)?, source })
}

struct FieldTypes<'a> {
    request: &'a FieldType,
    sorted_numeric: FieldType,
    sorted_set: FieldType,
}
fn mutable_document(row: &Row<'_>, types: &FieldTypes<'_>, raw: bool) -> Result<Document> {
    let mut doc = Document::new();
    doc.add(StoredField::from_binary("_source", row.source)?);
    doc.add(NumericDocValuesField::new(ROW, row.row));
    doc.add(LongPoint::new(TS, [row.millis])?);
    doc.add(Field::new(TS, row.millis, types.sorted_numeric.clone()));
    doc.add(InetAddressPoint::new("clientip", row.ip)?);
    doc.add(Field::new("clientip", BytesRef::from_bytes(InetAddressPoint::encode(row.ip).to_vec()), types.sorted_set.clone()));
    doc.add(Field::from_string("request", row.request, types.request.clone())?);
    if raw {
        doc.add(KeywordField::from_bytes_ref(RAW, row.request.as_bytes(), Store::No)?);
    }
    for (field, value) in [("status", row.status), ("size", row.size)] {
        doc.add(IntPoint::new(field, [value])?);
        doc.add(Field::new(field, i64::from(value), types.sorted_numeric.clone()));
    }
    Ok(doc)
}

fn update_document(doc: &mut Document, row: &Row<'_>, raw: bool) -> Result<()> {
    let mut fields = doc.into_iter();
    macro_rules! next {
        ($kind:ident) => {{
            let Some(Fields::$kind(field)) = fields.next() else {
                return Err("unexpected reusable field layout".into());
            };
            field
        }};
    }
    next!(Stored).set_bytes_value(row.source.to_vec())?;
    next!(NumericDocValues).set_long_value(row.row)?;
    next!(LongPoint).set_long_values([row.millis])?;
    next!(Field).set_long_value(row.millis)?;
    next!(InetAddressPoint).set_inet_address_value(row.ip)?;
    next!(Field).set_bytes_value(InetAddressPoint::encode(row.ip).to_vec())?;
    next!(Field).set_string_value(row.request)?;
    if raw {
        next!(Keyword).set_bytes_value(row.request.as_bytes())?;
    }
    next!(IntPoint).set_int_values([row.status])?;
    next!(Field).set_long_value(i64::from(row.status))?;
    next!(IntPoint).set_int_values([row.size])?;
    next!(Field).set_long_value(i64::from(row.size))?;
    assert!(fields.next().is_none());
    Ok(())
}

fn write_index(input: &[u8], path: &Path, request_type: &FieldType, mode: WriteMode) -> Result<Value> {
    let dir = Arc::new(MMapDirectory::new(path)?);
    let mut cfg = IndexWriterConfig::new()?;
    cfg.set_open_mode(OpenMode::Create)
        .set_commit_on_close(false)
        .set_merge_scheduler(SerialMergeScheduler::new())
        .set_ram_buffer_size_mb(RAM_MB)
        .set_max_buffered_docs(BUFFERED_DOCS)
        .set_use_compound_file(false);
    cfg.set_merge_policy(NoMergePolicy::default());
    let writer = IndexWriter::new(dir.clone(), cfg)?;

    let mut sorted_numeric = FieldType::new();
    sorted_numeric.set_doc_values_type(DocValuesType::SortedNumeric)?;
    sorted_numeric.freeze();
    let mut sorted_set = FieldType::new();
    sorted_set.set_doc_values_type(DocValuesType::SortedSet)?;
    sorted_set.freeze();
    let types = FieldTypes { request: request_type, sorted_numeric, sorted_set };
    let mut input = input;
    let start;
    match mode {
        WriteMode::Fresh => {
            start = Instant::now();
            for _ in 0..DOCS {
                let row = read_row(&mut input)?;
                let mut doc = mutable_document(&row, &types, row.raw())?;
                writer.add_document(&mut doc)?;
            }
        }
        WriteMode::Reuse => {
            let empty = Row { row: 0, millis: 0, status: 0, size: 0, ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED), request: "", source: &[] };
            let mut full = mutable_document(&empty, &types, true)?;
            let mut no_raw = mutable_document(&empty, &types, false)?;
            start = Instant::now();
            for _ in 0..DOCS {
                let row = read_row(&mut input)?;
                let raw = row.raw();
                let doc = if raw { &mut full } else { &mut no_raw };
                update_document(doc, &row, raw)?;
                writer.add_document(doc)?;
            }
        }
    }
    let write_ns = elapsed(start);
    assert!(input.is_empty());
    let start = Instant::now();
    writer.commit()?;
    let commit_ns = elapsed(start);
    let start = Instant::now();
    writer.close()?;
    let close_ns = elapsed(start);
    dir.close()?;
    Ok(
        json!({"docs": DOCS, "write_ns": write_ns, "commit_ns": commit_ns,
              "close_ns": close_ns, "total_ns": write_ns + commit_ns + close_ns}),
    )
}

fn write_server(input_path: &str, mode: WriteMode) -> Result<()> {
    let input = fs::read(input_path)?;
    let mut request_type = FieldType::new();
    request_type.set_tokenized(true)?;
    request_type.set_index_options(IndexOptions::Docs)?;
    request_type.set_omit_norms(true)?;
    request_type.freeze();
    emit(json!({"ready": true, "pid": std::process::id(), "input_bytes": input.len()}))?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        emit(write_index(&input, Path::new(&line), &request_type, mode)?)?;
    }
    Ok(())
}

fn time_status(lo: i64, hi: i64, status: i32) -> Result<Query> {
    let mut query = Builder::new();
    query.add(LongPoint::new_range_query(TS, lo, hi)?, Occur::Must)?;
    query.add(IntPoint::new_exact_query("status", status)?, Occur::Must)?;
    Ok(query.build().into())
}

fn timed<T, E>(
    call: impl FnOnce() -> std::result::Result<T, E>,
) -> std::result::Result<(u64, T), E> {
    let start = Instant::now();
    let result = call()?;
    Ok((elapsed(start), result))
}

struct Outcome {
    search_ns: u64,
    fetch_ns: u64,
    count: Option<i32>,
    ids: Vec<i32>,
    order: Vec<[i64; 2]>,
    source_bytes: u64,
}

impl Outcome {
    fn new(search_ns: u64) -> Self {
        Self {
            search_ns,
            fetch_ns: 0,
            count: None,
            ids: vec![],
            order: vec![],
            source_bytes: 0,
        }
    }
}

fn query_server(path: &str, config: &str) -> Result<()> {
    let config: Value = serde_json::from_str(&fs::read_to_string(config)?)?;
    let lo = config["range_start_ms_inclusive"]
        .as_i64()
        .ok_or("missing start")?;
    let hi = config["range_end_ms_exclusive"]
        .as_i64()
        .ok_or("missing end")?
        - 1;
    let cursors = [
        config["official_cursor_ms"]
            .as_i64()
            .ok_or("missing official cursor")?,
        config["sample_cursor_ms"]
            .as_i64()
            .ok_or("missing sample cursor")?,
    ];
    let dir = Arc::new(MMapDirectory::new(Path::new(path))?);
    let reader = Arc::new(directory_reader::open(dir.clone())?);
    let max_doc = reader.max_doc()?;
    assert_eq!(max_doc as usize, DOCS);
    assert_eq!(reader.num_docs()?, max_doc);
    let mut searcher = from_reader(reader.clone())?;
    searcher.set_query_cache(None);
    let queries: [Query; 5] = [
        MatchAllDocsQuery::new().into(),
        TermQuery::new(Term::from_text(RAW, "GET / HTTP/1.0")).into(),
        time_status(lo, hi, 200)?,
        time_status(lo, hi, 400)?,
        LongPoint::new_range_query(TS, lo, hi)?.into(),
    ];
    let mut sorts = Vec::with_capacity(2);
    let mut stable_sorts = Vec::with_capacity(2);
    for desc in [false, true] {
        let time = SortedNumericSortField::with_reverse(TS, SortFieldType::Long, desc)?;
        sorts.push(Arc::new(Sort::with_fields([time.clone()])?));
        stable_sorts.push(Arc::new(Sort::with_fields([
            SortFieldEnum::from(time),
            SortField::new(Some(ROW), SortFieldType::Long)?.into(),
        ])?));
    }
    let mut stored = reader.stored_fields()?;
    let mut run = |case: usize| -> Result<Outcome> {
        let mut out;
        if case >= 12 {
            let (ns, count) = timed(|| searcher.count(queries[case - 12].clone()))?;
            out = Outcome::new(ns);
            out.count = Some(count);
            return Ok(out);
        }
        if case < 4 {
            let (ns, hits) = timed(|| {
                let manager = TopScoreDocCollectorManager::new(TOP, THRESHOLD)?;
                searcher.search_with_collector_manager(queries[case].clone(), &manager)
            })?;
            out = Outcome::new(ns);
            out.ids
                .extend(hits.score_docs.into_iter().map(|hit| hit.doc));
        } else {
            let direction = case % 2;
            let sort = if case < 10 {
                &sorts[direction]
            } else {
                &stable_sorts[direction]
            };
            let pages = if case < 10 { 1 } else { PAGES };
            let threshold = if case < 6 { THRESHOLD } else { 0 };
            let mut cursor = if (6..10).contains(&case) {
                Some(FieldDoc::with_fields(
                    max_doc - 1,
                    f32::NAN,
                    [FieldsValue::Long(cursors[(case - 6) / 2])],
                ))
            } else {
                None
            };
            out = Outcome::new(0);
            for _ in 0..pages {
                let (ns, hits) = timed(|| {
                    if let Some(after) = cursor.take() {
                        let manager = TopFieldCollectorManager::with_after(
                            sort.clone(),
                            TOP,
                            after,
                            threshold,
                        )?;
                        searcher.search_with_collector_manager(queries[0].clone(), &manager)
                    } else {
                        let manager = TopFieldCollectorManager::new(sort.clone(), TOP, threshold)?;
                        searcher.search_with_collector_manager(queries[0].clone(), &manager)
                    }
                })?;
                out.search_ns += ns;
                let hits = hits.base.score_docs;
                if pages > 1 {
                    cursor = hits.last().and_then(|hit| hit.as_field().cloned());
                }
                for hit in hits {
                    let ts = match hit.fields()?[0] {
                        FieldsValue::Long(ts) => ts,
                        _ => return Err("unexpected sort value".into()),
                    };
                    out.ids.push(hit.doc());
                    out.order.push([ts, i64::from(hit.doc())]);
                }
            }
        }
        if !out.ids.is_empty() {
            let (ns, size) = timed(|| -> Result<u64> {
                let mut size = 0;
                for id in &out.ids {
                    let doc = stored.document(*id)?;
                    let source = doc.get_binary_value("_source")?.ok_or("missing source")?;
                    size += source.as_byte_slice().len() as u64;
                }
                Ok(size)
            })?;
            out.fetch_ns = ns;
            out.source_bytes = size;
        }
        Ok(out)
    };
    emit(json!({"ready": true, "pid": std::process::id(), "docs": max_doc}))?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let args: Vec<&str> = line.split_whitespace().collect();
        match args.as_slice() {
            ["verify"] => {
                let mut out = serde_json::Map::new();
                for (i, name) in CASES.iter().enumerate() {
                    let r = run(i)?;
                    let value = if let Some(count) = r.count {
                        json!(count)
                    } else if i < 4 {
                        json!(r.ids)
                    } else {
                        json!(r.order)
                    };
                    out.insert((*name).into(), value);
                }
                emit(Value::Object(out))?;
            }
            ["warmup", seconds] => {
                let min = Duration::from_secs(seconds.parse()?);
                let start = Instant::now();
                let mut cycles = 0;
                let mut checksum = 0u64;
                while start.elapsed() < min {
                    for i in 0..CASES.len() {
                        let r = run((cycles + i) % CASES.len())?;
                        checksum +=
                            r.source_bytes + r.count.unwrap_or(0) as u64 + r.ids.len() as u64;
                    }
                    cycles += 1;
                }
                emit(json!({"warmup_ns": elapsed(start), "cycles": cycles, "checksum": checksum}))?;
            }
            ["measure", repeats, rotation] => {
                let repeats: usize = repeats.parse()?;
                let rotation: usize = rotation.parse()?;
                let mut search = vec![Vec::<u64>::new(); CASES.len()];
                let mut fetch = vec![Vec::<u64>::new(); CASES.len()];
                let mut checksum = [0u64; CASES.len()];
                for rep in 0..repeats {
                    for offset in 0..CASES.len() {
                        let i = (rotation + rep + offset) % CASES.len();
                        let r = run(i)?;
                        search[i].push(r.search_ns);
                        fetch[i].push(r.fetch_ns);
                        checksum[i] +=
                            r.source_bytes + r.count.unwrap_or(0) as u64 + r.ids.len() as u64;
                    }
                }
                let result: serde_json::Map<String, Value> = CASES.iter().enumerate().map(|(i, name)|
                    ((*name).into(), json!({"search_ns": search[i], "fetch_ns": fetch[i], "checksum": checksum[i]}))).collect();
                emit(Value::Object(result))?;
            }
            _ => return Err("invalid query command".into()),
        }
    }
    drop(stored);
    drop(searcher);
    reader.close()?;
    dir.close()?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [_, "write" | "fresh", path] => write_server(path, WriteMode::Fresh),
        [_, "reuse", path] => write_server(path, WriteMode::Reuse),
        [_, "query", path, config] => query_server(path, config),
        _ => Err("usage: benchmark write input | benchmark query index config".into()),
    }
}
