import com.eclipsesource.json.Json;
import com.eclipsesource.json.JsonArray;
import com.eclipsesource.json.JsonObject;
import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.net.InetAddress;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.List;
import org.apache.lucene.analysis.standard.StandardAnalyzer;
import org.apache.lucene.document.Document;
import org.apache.lucene.document.Field;
import org.apache.lucene.document.FieldType;
import org.apache.lucene.document.InetAddressPoint;
import org.apache.lucene.document.IntPoint;
import org.apache.lucene.document.LongPoint;
import org.apache.lucene.document.NumericDocValuesField;
import org.apache.lucene.document.SortedNumericDocValuesField;
import org.apache.lucene.document.SortedSetDocValuesField;
import org.apache.lucene.document.StoredField;
import org.apache.lucene.document.KeywordField;
import org.apache.lucene.index.DirectoryReader;
import org.apache.lucene.index.DocValues;
import org.apache.lucene.index.DocValuesType;
import org.apache.lucene.index.IndexOptions;
import org.apache.lucene.index.IndexWriter;
import org.apache.lucene.index.IndexWriterConfig;
import org.apache.lucene.index.NoMergePolicy;
import org.apache.lucene.index.SerialMergeScheduler;
import org.apache.lucene.index.Term;
import org.apache.lucene.search.BooleanClause;
import org.apache.lucene.search.BooleanQuery;
import org.apache.lucene.search.FieldDoc;
import org.apache.lucene.search.IndexSearcher;
import org.apache.lucene.search.MatchAllDocsQuery;
import org.apache.lucene.search.Query;
import org.apache.lucene.search.ScoreDoc;
import org.apache.lucene.search.Sort;
import org.apache.lucene.search.SortField;
import org.apache.lucene.search.SortedNumericSortField;
import org.apache.lucene.search.TermQuery;
import org.apache.lucene.search.TopFieldCollectorManager;
import org.apache.lucene.search.TopScoreDocCollectorManager;
import org.apache.lucene.store.MMapDirectory;
import org.apache.lucene.util.BytesRef;

public final class RallyMillion {
    private static final String TS = "@timestamp";
    private static final String ROW = "_row";
    private static final String RAW = "request.raw";
    private static final int DOCS = 1_000_000;
    private static final int TOP = 10;
    private static final int PAGES = 4;
    private static final int THRESHOLD = 10_000;
    private static final int BUFFERED_DOCS = 65_536;
    private static final String[] CASES = {
        "all", "term", "range200", "range400",
        "asc", "desc", "official_asc", "official_desc",
        "sample_asc", "sample_desc", "stable_asc", "stable_desc",
        "count_all", "count_term", "count_range200", "count_range400", "count_range",
    };

    private record Row(long row, long millis, int status, int size, InetAddress ip,
                       byte[] request, byte[] source) {}

    private static Row readRow(ByteBuffer input) throws IOException {
        long row = Integer.toUnsignedLong(input.getInt());
        long millis = input.getLong();
        int status = input.getInt(), size = input.getInt();
        byte[] ip = new byte[4];
        input.get(ip);
        int requestLen = Short.toUnsignedInt(input.getShort());
        int sourceLen = Short.toUnsignedInt(input.getShort());
        byte[] request = new byte[requestLen];
        byte[] source = new byte[sourceLen];
        input.get(request);
        input.get(source);
        return new Row(row, millis, status, size, InetAddress.getByAddress(ip), request, source);
    }

    private static Document document(Row row, FieldType requestType, String text, boolean raw) {
        var doc = new Document();
        doc.add(new StoredField("_source", row.source()));
        doc.add(new NumericDocValuesField(ROW, row.row()));
        doc.add(new LongPoint(TS, row.millis()));
        doc.add(new SortedNumericDocValuesField(TS, row.millis()));
        doc.add(new InetAddressPoint("clientip", row.ip()));
        doc.add(new SortedSetDocValuesField("clientip", new BytesRef(InetAddressPoint.encode(row.ip()))));
        doc.add(new Field("request", text, requestType));
        if (raw) {
            var request = new BytesRef(row.request());
            doc.add(new KeywordField(RAW, request, Field.Store.NO));
        }
        doc.add(new IntPoint("status", row.status()));
        doc.add(new SortedNumericDocValuesField("status", row.status()));
        doc.add(new IntPoint("size", row.size()));
        doc.add(new SortedNumericDocValuesField("size", row.size()));
        return doc;
    }

    private record ReusableDoc(Document doc, Field[] fields) {
        ReusableDoc(Document doc) {
            this(doc, doc.getFields().stream().map(f -> (Field) f).toArray(Field[]::new));
        }

        void update(Row row, String text, boolean raw) {
            int i = 0;
            fields[i++].setBytesValue(row.source());
            fields[i++].setLongValue(row.row());
            fields[i++].setLongValue(row.millis());
            fields[i++].setLongValue(row.millis());
            ((InetAddressPoint) fields[i++]).setInetAddressValue(row.ip());
            fields[i++].setBytesValue(new BytesRef(InetAddressPoint.encode(row.ip())));
            fields[i++].setStringValue(text);
            if (raw) {
                var bytes = new BytesRef(row.request());
                fields[i++].setBytesValue(bytes);
            }
            fields[i++].setIntValue(row.status());
            fields[i++].setLongValue(row.status());
            fields[i++].setIntValue(row.size());
            fields[i++].setLongValue(row.size());
            if (i != fields.length) throw new AssertionError("unexpected reusable field layout");
        }
    }

    private static JsonObject write(byte[] data, Path path, FieldType requestType, boolean reuse) throws IOException {
        try (var dir = new MMapDirectory(path); var analyzer = new StandardAnalyzer()) {
            var config = new IndexWriterConfig(analyzer)
                .setOpenMode(IndexWriterConfig.OpenMode.CREATE)
                .setCommitOnClose(false)
                .setMergeScheduler(new SerialMergeScheduler())
                .setMergePolicy(NoMergePolicy.INSTANCE)
                .setRAMBufferSizeMB(256)
                .setMaxBufferedDocs(BUFFERED_DOCS)
                .setUseCompoundFile(false);
            try (var writer = new IndexWriter(dir, config)) {
                var input = ByteBuffer.wrap(data);
                long start;
                if (reuse) {
                    var empty = new Row(0, 0, 0, 0, InetAddress.getByAddress(new byte[4]), new byte[0], new byte[0]);
                    var full = new ReusableDoc(document(empty, requestType, "", true));
                    var noRaw = new ReusableDoc(document(empty, requestType, "", false));
                    start = System.nanoTime();
                    for (int i = 0; i < DOCS; i++) {
                        var row = readRow(input);
                        String text = new String(row.request(), StandardCharsets.UTF_8);
                        boolean raw = text.length() <= 256;
                        var doc = raw ? full : noRaw;
                        doc.update(row, text, raw);
                        writer.addDocument(doc.doc());
                    }
                } else {
                    start = System.nanoTime();
                    for (int i = 0; i < DOCS; i++) {
                        var row = readRow(input);
                        String text = new String(row.request(), StandardCharsets.UTF_8);
                        writer.addDocument(document(row, requestType, text, text.length() <= 256));
                    }
                }
                long write = System.nanoTime() - start;
                require(!input.hasRemaining(), "unexpected trailing input");
                start = System.nanoTime();
                writer.commit();
                long commit = System.nanoTime() - start;
                start = System.nanoTime();
                writer.close();
                long close = System.nanoTime() - start;
                return new JsonObject().add("docs", DOCS).add("write_ns", write)
                    .add("commit_ns", commit).add("close_ns", close).add("total_ns", write + commit + close);
            }
        }
    }

    private static void require(boolean ok, String message) {
        if (!ok) throw new IllegalStateException(message);
    }

    private static void emit(JsonObject result) {
        System.out.println(result);
        System.out.flush();
    }

    private static JsonObject ready() {
        return new JsonObject().add("ready", true).add("pid", ProcessHandle.current().pid());
    }

    private static BufferedReader commands() {
        return new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
    }

    private static void writeServer(Path dataPath, boolean reuse) throws IOException {
        byte[] input = Files.readAllBytes(dataPath);
        FieldType type = new FieldType();
        type.setTokenized(true);
        type.setIndexOptions(IndexOptions.DOCS);
        type.setOmitNorms(true);
        type.freeze();
        emit(ready().add("input_bytes", input.length));
        try (var lines = commands()) {
            for (String line; (line = lines.readLine()) != null;) emit(write(input, Path.of(line), type, reuse));
        }
    }

    private static Query timeStatus(long lo, long hi, int status) {
        return new BooleanQuery.Builder()
            .add(LongPoint.newRangeQuery(TS, lo, hi), BooleanClause.Occur.MUST)
            .add(IntPoint.newExactQuery("status", status), BooleanClause.Occur.MUST)
            .build();
    }

    private static final class Outcome {
        long search, fetch, sourceBytes;
        Integer count;
        final List<Integer> ids = new ArrayList<>();
        final List<long[]> order = new ArrayList<>();

        long checksum() {
            return sourceBytes + (count == null ? 0 : count) + ids.size();
        }
    }

    private static final class Search {
        final IndexSearcher searcher;
        final DirectoryReader reader;
        final org.apache.lucene.index.StoredFields stored;
        final Query[] queries;
        final long[] cursors;
        final Sort[] sorts = new Sort[2], stableSorts = new Sort[2];

        Search(DirectoryReader reader, JsonObject config) throws IOException {
            this.reader = reader;
            this.searcher = new IndexSearcher(reader);
            searcher.setQueryCache(null);
            this.stored = reader.storedFields();
            long lo = config.getLong("range_start_ms_inclusive", 0);
            long hi = config.getLong("range_end_ms_exclusive", 0) - 1;
            queries = new Query[] {
                new MatchAllDocsQuery(),
                new TermQuery(new Term(RAW, "GET / HTTP/1.0")),
                timeStatus(lo, hi, 200), timeStatus(lo, hi, 400), LongPoint.newRangeQuery(TS, lo, hi),
            };
            cursors = new long[] {config.getLong("official_cursor_ms", 0), config.getLong("sample_cursor_ms", 0)};
            for (int direction = 0; direction < 2; direction++) {
                var time = new SortedNumericSortField(TS, SortField.Type.LONG, direction == 1);
                sorts[direction] = new Sort(time);
                stableSorts[direction] = new Sort(time, new SortField(ROW, SortField.Type.LONG));
            }
        }

        Outcome run(int scenario) throws IOException {
            var out = new Outcome();
            if (scenario >= 12) {
                long start = System.nanoTime();
                int count = searcher.count(queries[scenario - 12]);
                out.search = System.nanoTime() - start;
                out.count = count;
                return out;
            }
            if (scenario < 4) {
                long start = System.nanoTime();
                var manager = new TopScoreDocCollectorManager(TOP, THRESHOLD);
                ScoreDoc[] hits = searcher.search(queries[scenario], manager).scoreDocs;
                out.search = System.nanoTime() - start;
                for (var hit : hits) out.ids.add(hit.doc);
            } else {
                int direction = scenario % 2;
                var sort = scenario < 10 ? sorts[direction] : stableSorts[direction];
                int pages = scenario < 10 ? 1 : PAGES;
                int threshold = scenario < 6 ? THRESHOLD : 0;
                FieldDoc cursor = scenario >= 6 && scenario < 10
                    ? new FieldDoc(reader.maxDoc() - 1, Float.NaN, new Object[] {cursors[(scenario - 6) / 2]})
                    : null;
                for (int p = 0; p < pages; p++) {
                    long start = System.nanoTime();
                    var manager = new TopFieldCollectorManager(sort, TOP, cursor, threshold);
                    ScoreDoc[] hits = searcher.search(queries[0], manager).scoreDocs;
                    out.search += System.nanoTime() - start;
                    if (pages > 1 && hits.length > 0) cursor = (FieldDoc) hits[hits.length - 1];
                    for (var hit : hits) {
                        out.ids.add(hit.doc);
                        out.order.add(new long[] {(Long) ((FieldDoc) hit).fields[0], hit.doc});
                    }
                }
            }
            if (!out.ids.isEmpty()) {
                long start = System.nanoTime();
                long bytes = 0;
                for (int doc : out.ids) bytes += stored.document(doc).getBinaryValue("_source").length;
                out.fetch = System.nanoTime() - start;
                out.sourceBytes = bytes;
            }
            return out;
        }
    }

    private static void queryServer(Path path, Path configPath) throws IOException {
        var config = Json.parse(Files.readString(configPath)).asObject();
        try (var dir = new MMapDirectory(path); var reader = DirectoryReader.open(dir); var lines = commands()) {
            require(reader.maxDoc() == DOCS && reader.numDocs() == DOCS, "wrong index document count");
            var search = new Search(reader, config);
            emit(ready().add("docs", reader.maxDoc()).add("leaves", reader.leaves().size()));
            for (String line; (line = lines.readLine()) != null;) {
                String[] cmd = line.split(" ");
                if (cmd[0].equals("verify")) {
                    var result = new JsonObject();
                    for (int i = 0; i < CASES.length; i++) {
                        Outcome r = search.run(i);
                        if (r.count != null) {
                            result.add(CASES[i], r.count);
                        } else {
                            var hits = new JsonArray();
                            if (i < 4) {
                                for (int doc : r.ids) hits.add(doc);
                            } else {
                                for (var pair : r.order) hits.add(new JsonArray().add(pair[0]).add(pair[1]));
                            }
                            result.add(CASES[i], hits);
                        }
                    }
                    emit(result);
                } else if (cmd[0].equals("warmup")) {
                    long start = System.nanoTime(), min = Long.parseLong(cmd[1]) * 1_000_000_000L;
                    int cycles = 0;
                    long checksum = 0;
                    while (System.nanoTime() - start < min) {
                        for (int i = 0; i < CASES.length; i++) checksum += search.run((cycles + i) % CASES.length).checksum();
                        cycles++;
                    }
                    emit(new JsonObject().add("warmup_ns", System.nanoTime() - start)
                         .add("cycles", cycles).add("checksum", checksum));
                } else if (cmd[0].equals("measure")) {
                    int repeats = Integer.parseInt(cmd[1]), rotation = Integer.parseInt(cmd[2]);
                    long[][] searchNs = new long[CASES.length][repeats], fetchNs = new long[CASES.length][repeats];
                    long[] checksums = new long[CASES.length];
                    for (int rep = 0; rep < repeats; rep++) {
                        for (int offset = 0; offset < CASES.length; offset++) {
                            int i = (rotation + rep + offset) % CASES.length;
                            var out = search.run(i);
                            searchNs[i][rep] = out.search;
                            fetchNs[i][rep] = out.fetch;
                            checksums[i] += out.checksum();
                        }
                    }
                    var result = new JsonObject();
                    for (int i = 0; i < CASES.length; i++) {
                        var s = new JsonArray();
                        var f = new JsonArray();
                        for (int j = 0; j < repeats; j++) {
                            s.add(searchNs[i][j]);
                            f.add(fetchNs[i][j]);
                        }
                        result.add(CASES[i], new JsonObject().add("search_ns", s).add("fetch_ns", f)
                                   .add("checksum", checksums[i]));
                    }
                    emit(result);
                } else {
                    throw new IllegalArgumentException("unknown query command");
                }
            }
        }
    }

    private static void audit(Path index, Path data) throws Exception {
        var input = ByteBuffer.wrap(Files.readAllBytes(data));
        var hash = MessageDigest.getInstance("SHA-256");
        var segments = new JsonArray();
        try (var dir = new MMapDirectory(index); var reader = DirectoryReader.open(dir)) {
            require(reader.numDocs() == DOCS && reader.maxDoc() == DOCS, "doc count");
            for (var context : reader.leaves()) {
                var leaf = context.reader();
                var info = leaf.getFieldInfos().fieldInfo("request");
                require(info.getIndexOptions() == IndexOptions.DOCS && info.omitsNorms()
                        && info.getDocValuesType() == DocValuesType.NONE, "request layout");
                var stored = leaf.storedFields();
                var seq = DocValues.getNumeric(leaf, ROW);
                var ts = DocValues.getSortedNumeric(leaf, TS);
                var status = DocValues.getSortedNumeric(leaf, "status");
                var size = DocValues.getSortedNumeric(leaf, "size");
                var ip = DocValues.getSortedSet(leaf, "clientip");
                var raw = DocValues.getSortedSet(leaf, RAW);
                for (int doc = 0; doc < leaf.maxDoc(); doc++) {
                    Row row = readRow(input);
                    BytesRef source = stored.document(doc).getBinaryValue("_source");
                    require(source.equals(new BytesRef(row.source())), "source mismatch");
                    hash.update(source.bytes, source.offset, source.length);
                    hash.update((byte) '\n');
                    require(seq.advanceExact(doc) && seq.longValue() == row.row(), "row DV");
                    require(ts.advanceExact(doc) && ts.docValueCount() == 1 && ts.nextValue() == row.millis(), "timestamp DV");
                    require(status.advanceExact(doc) && status.docValueCount() == 1 && status.nextValue() == row.status(), "status DV");
                    require(size.advanceExact(doc) && size.docValueCount() == 1 && size.nextValue() == row.size(), "size DV");
                    require(ip.advanceExact(doc) && ip.docValueCount() == 1
                            && ip.lookupOrd(ip.nextOrd()).equals(new BytesRef(InetAddressPoint.encode(row.ip()))), "IP DV");
                    boolean hasRaw = new String(row.request(), StandardCharsets.UTF_8).length() <= 256;
                    require(raw.advanceExact(doc) == hasRaw, "raw DV presence");
                    if (hasRaw) require(raw.docValueCount() == 1
                        && raw.lookupOrd(raw.nextOrd()).equals(new BytesRef(row.request())), "raw DV value");
                }
                segments.add(leaf.maxDoc());
            }
            require(!input.hasRemaining(), "audit did not consume input");
            emit(new JsonObject().add("docs", reader.maxDoc()).add("segments", segments)
                 .add("source_sha256", HexFormat.of().formatHex(hash.digest())));
        }
    }

    public static void main(String[] args) throws Exception {
        switch (args[0]) {
            case "write", "fresh" -> writeServer(Path.of(args[1]), false);
            case "reuse" -> writeServer(Path.of(args[1]), true);
            case "query" -> queryServer(Path.of(args[1]), Path.of(args[2]));
            case "audit" -> audit(Path.of(args[1]), Path.of(args[2]));
            default -> throw new IllegalArgumentException("unknown mode");
        }
    }
}
