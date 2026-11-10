use rlucene::core::{
    analysis::{
        analyzer::Analyzer, standard::standard_analyzer::StandardAnalyzer,
        token_stream::TokenStream,
    },
    codecs::lucene101_codec::{Lucene101Codec, Mode},
    document::{
        document::Document,
        field::{FieldBase, Store},
        fields::Fields,
        string_field::StringField,
        text_field::TextField,
    },
    index::{
        directory_reader,
        index_reader::IndexReader,
        index_reader_context::IndexReaderContext,
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
        boolean_clause::Occur, boolean_query, index_searcher::from_reader,
        match_no_docs_query::MatchNoDocsQuery, phrase_query, query::Query, term_query::TermQuery,
        top_score_doc_collector_manager::TopScoreDocCollectorManager,
    },
    store::mmap_directory::MMapDirectory,
    util::{
        attribute_source::AttributeSource,
        close::{Closeable, CloseableRef},
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
const PID: &str = "pid";
const TEXT: &str = "passage";

fn emit(value: Value) -> Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &value)?;
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8]> {
    let (bytes, rest) = input.split_at_checked(count).ok_or("truncated input")?;
    *input = rest;
    Ok(bytes)
}

fn length(input: &mut &[u8]) -> Result<usize> {
    Ok(u32::from_be_bytes(take(input, 4)?.try_into()?) as usize)
}

fn write_index(mut input: &[u8], path: &Path) -> Result<Value> {
    let count = length(&mut input)?;
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
    let mut doc = Document::new();
    doc.add(StringField::from_string(PID, "", Store::Yes)?);
    doc.add(TextField::from_string(TEXT, "", Store::Yes)?);

    let start = Instant::now();
    for _ in 0..count {
        let pid_len = length(&mut input)?;
        let text_len = length(&mut input)?;
        let pid = std::str::from_utf8(take(&mut input, pid_len)?)?;
        let text = std::str::from_utf8(take(&mut input, text_len)?)?;
        let mut fields = (&mut doc).into_iter();
        let (Some(Fields::String(pid_field)), Some(Fields::Text(text_field))) =
            (fields.next(), fields.next())
        else {
            return Err("unexpected field layout".into());
        };
        pid_field.set_string_value(pid)?;
        text_field.set_string_value(text)?;
        writer.add_document(&mut doc)?;
    }
    let write_ns = start.elapsed().as_nanos() as u64;
    assert!(input.is_empty(), "trailing input");
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

fn write_server(path: &str) -> Result<()> {
    let input = fs::read(path)?;
    emit(json!({"ready": true, "pid": std::process::id(), "input_bytes": input.len()}))?;
    for line in io::stdin().lock().lines() {
        emit(write_index(&input, Path::new(&line?))?)?;
    }
    Ok(())
}

fn tokens(analyzer: &StandardAnalyzer, text: &str) -> Result<Vec<(String, i32, i32, i32)>> {
    let mut stream = analyzer.token_stream_from_text(TEXT, text)?;
    let mut out = Vec::new();
    stream.reset()?;
    while stream.increment_token()? {
        let a = stream.get_attribute_source();
        let term = a.buffer()?[..a.length()?].iter().collect();
        out.push((
            term,
            a.get_position_increment()?,
            a.start_offset()?,
            a.end_offset()?,
        ));
    }
    stream.end()?;
    stream.close()?;
    Ok(out)
}

fn query(analyzer: &StandardAnalyzer, kind: &str, value: &Value) -> Result<Query> {
    let term = |field, value| TermQuery::new(Term::from_text(field, value)).into();
    Ok(match kind {
        "pid" => term(PID, value.as_str().ok_or("expected pid")?),
        "term" => term(TEXT, value.as_str().ok_or("expected term")?),
        "phrase" => {
            let mut builder = phrase_query::Builder::new();
            for word in value.as_array().ok_or("expected phrase")? {
                builder.add_term(Term::from_text(TEXT, word.as_str().ok_or("expected term")?))?;
            }
            builder.build()?.into()
        }
        "dev" => {
            let words = tokens(analyzer, value.as_str().ok_or("expected dev query")?)?;
            let mut builder = boolean_query::Builder::new();
            if words.is_empty() {
                MatchNoDocsQuery::with_reason("empty analyzed query").into()
            } else {
                for (word, _, _, _) in &words {
                    let clause: Query = TermQuery::new(Term::from_text(TEXT, word.as_str())).into();
                    builder.add(clause, Occur::Should)?;
                }
                builder.build().into()
            }
        }
        _ => return Err("unknown query kind".into()),
    })
}

fn verify(path: &str, config: &str) -> Result<()> {
    let analyzer = StandardAnalyzer::new();
    let cases: Vec<(String, String, Value)> = serde_json::from_slice(&fs::read(config)?)?;
    let dir = Arc::new(MMapDirectory::new(Path::new(path))?);
    let reader = Arc::new(directory_reader::open(dir.clone())?);
    let mut searcher = from_reader(reader.clone())?;
    searcher.set_query_cache(None);
    let mut stored = reader.stored_fields()?;
    let mut results = Vec::new();
    let max_doc = reader.max_doc()?;
    for (name, kind, value) in cases {
        let query = query(&analyzer, &kind, &value)?;
        let count = searcher.count(query.clone())?;
        let manager = TopScoreDocCollectorManager::new(10, max_doc as usize)?;
        let hits = searcher.search_with_collector_manager(query, &manager)?;
        assert_eq!(count as usize, hits.total_hits.value() as usize);
        let mut rows = Vec::new();
        for hit in hits.score_docs {
            let doc = stored.document(hit.doc)?;
            let pid = doc.get(PID)?.ok_or("missing pid")?.into_owned();
            rows.push(json!([hit.doc, pid, hit.score.to_bits()]));
        }
        results.push(json!([name, count, rows]));
    }
    let context = reader.clone().get_context()?;
    let segments = context
        .leaves()?
        .iter()
        .map(|leaf| leaf.reader().max_doc())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    emit(
        json!({"docs": max_doc, "live": reader.num_docs()?, "segments": segments, "results": results}),
    )
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.as_slice() {
        [_, mode, path] if mode == "reuse" => write_server(path),
        [_, mode, path] if mode == "tokens" => {
            let analyzer = StandardAnalyzer::new();
            let input: Vec<String> = serde_json::from_slice(&fs::read(path)?)?;
            let results = input
                .iter()
                .map(|text| tokens(&analyzer, text))
                .collect::<Result<Vec<_>>>()?;
            emit(json!(results))
        }
        [_, mode, path, config] if mode == "query" => verify(path, config),
        _ => Err("usage: reuse input | tokens config | query index config".into()),
    }
}
