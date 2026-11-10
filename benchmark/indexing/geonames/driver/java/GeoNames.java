import com.eclipsesource.json.Json;
import com.eclipsesource.json.JsonArray;
import com.eclipsesource.json.JsonObject;
import com.eclipsesource.json.JsonValue;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.TreeSet;
import org.apache.lucene.analysis.standard.StandardAnalyzer;
import org.apache.lucene.codecs.lucene101.Lucene101Codec;
import org.apache.lucene.document.Field;
import org.apache.lucene.document.LatLonPoint;
import org.apache.lucene.document.LongPoint;
import org.apache.lucene.document.NumericDocValuesField;
import org.apache.lucene.document.SortedSetDocValuesField;
import org.apache.lucene.document.StringField;
import org.apache.lucene.index.DirectoryReader;
import org.apache.lucene.index.DocValues;
import org.apache.lucene.index.IndexWriter;
import org.apache.lucene.index.IndexWriterConfig;
import org.apache.lucene.index.IndexableField;
import org.apache.lucene.index.LeafReader;
import org.apache.lucene.index.NoMergePolicy;
import org.apache.lucene.index.PointValues;
import org.apache.lucene.index.SerialMergeScheduler;
import org.apache.lucene.index.Term;
import org.apache.lucene.search.IndexSearcher;
import org.apache.lucene.search.MatchAllDocsQuery;
import org.apache.lucene.search.Query;
import org.apache.lucene.search.Sort;
import org.apache.lucene.search.SortField;
import org.apache.lucene.search.TermQuery;
import org.apache.lucene.store.MMapDirectory;
import org.apache.lucene.util.BytesRef;
import org.apache.lucene.util.NumericUtils;

public final class GeoNames {
    private static final String ID = "geonameid", LOCATION = "location", POP = "population", ALIAS = "alias";

    private record Row(String id, double lat, double lon, int latEncoded, int lonEncoded,
                       long population, BytesRef[] aliases) {}

    private static BytesRef take(ByteBuffer input) {
        int len = Short.toUnsignedInt(input.getShort());
        var value = new BytesRef(input.array(), input.position(), len);
        input.position(input.position() + len);
        return value;
    }

    private static Row[] readRows(byte[] data) {
        var in = ByteBuffer.wrap(data);
        var rows = new Row[in.getInt()];
        for (int i = 0; i < rows.length; i++) {
            double lat = in.getDouble(), lon = in.getDouble();
            int latEncoded = in.getInt(), lonEncoded = in.getInt();
            long population = in.getLong();
            String id = take(in).utf8ToString();
            var aliases = new BytesRef[Short.toUnsignedInt(in.getShort())];
            for (int a = 0; a < aliases.length; a++) aliases[a] = take(in);
            rows[i] = new Row(id, lat, lon, latEncoded, lonEncoded, population, aliases);
        }
        require(!in.hasRemaining(), "trailing input");
        return rows;
    }

    private static JsonObject write(Row[] rows, int maxAliases, Path path) throws Exception {
        try (var directory = new MMapDirectory(path); var analyzer = new StandardAnalyzer()) {
            var config = new IndexWriterConfig(analyzer)
                .setOpenMode(IndexWriterConfig.OpenMode.CREATE)
                .setCommitOnClose(false)
                .setMergeScheduler(new SerialMergeScheduler())
                .setRAMBufferSizeMB(256)
                .setMaxBufferedDocs(65_536)
                .setUseCompoundFile(false)
                .setCodec(new Lucene101Codec(Lucene101Codec.Mode.BEST_SPEED));
            config.setMergePolicy(NoMergePolicy.INSTANCE);
            try (var writer = new IndexWriter(directory, config)) {
                var popPoint = new LongPoint(POP, 0);
                var popDv = new NumericDocValuesField(POP, 0);
                var id = new StringField(ID, "", Field.Store.YES);
                var location = new LatLonPoint(LOCATION, 0, 0);
                var slots = new ArrayList<IndexableField>(4 + maxAliases);
                slots.addAll(List.of(popPoint, popDv, id, location));
                var aliases = new Field[maxAliases];
                for (int i = 0; i < maxAliases; i++) {
                    aliases[i] = new Field(ALIAS, new BytesRef(), SortedSetDocValuesField.TYPE);
                    slots.add(aliases[i]);
                }

                long start = System.nanoTime();
                for (var row : rows) {
                    int begin = 2;
                    if (row.population >= 0) {
                        popPoint.setLongValues(row.population);
                        popDv.setLongValue(row.population);
                        begin = 0;
                    }
                    id.setStringValue(row.id);
                    location.setLocationValue(row.lat, row.lon);
                    for (int i = 0; i < row.aliases.length; i++) aliases[i].setBytesValue(row.aliases[i]);
                    writer.addDocument(slots.subList(begin, 4 + row.aliases.length));
                }
                long write = System.nanoTime() - start;
                start = System.nanoTime();
                writer.commit();
                long commit = System.nanoTime() - start;
                start = System.nanoTime();
                writer.close();
                long close = System.nanoTime() - start;
                return new JsonObject().add("docs", rows.length).add("write_ns", write)
                    .add("commit_ns", commit).add("close_ns", close).add("total_ns", write + commit + close);
            }
        }
    }

    private static void require(boolean ok, String message) {
        if (!ok) throw new IllegalStateException(message);
    }

    private static void emit(JsonValue value) {
        System.out.println(value);
        System.out.flush();
    }

    private static void writeServer(Path path) throws Exception {
        var input = Files.readAllBytes(path);
        var rows = readRows(input);
        int maxAliases = 0;
        for (var row : rows) maxAliases = Math.max(maxAliases, row.aliases.length);
        emit(new JsonObject().add("ready", true).add("pid", ProcessHandle.current().pid())
            .add("input_bytes", input.length));
        try (var lines = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8))) {
            for (String line; (line = lines.readLine()) != null;) emit(write(rows, maxAliases, Path.of(line)));
        }
    }

    private static void auditPoints(LeafReader leaf, Row[] rows, int offset, boolean geo) throws Exception {
        var points = leaf.getPointValues(geo ? LOCATION : POP);
        var seen = new boolean[leaf.maxDoc()];
        if (points != null) {
            require(points.getNumDimensions() == (geo ? 2 : 1) &&
                    points.getNumIndexDimensions() == (geo ? 2 : 1) &&
                    points.getBytesPerDimension() == (geo ? 4 : 8), "point geometry");
            points.intersect(new PointValues.IntersectVisitor() {
                public void visit(int doc) {
                    throw new IllegalStateException("expected packed point");
                }

                public void visit(int doc, byte[] packed) {
                    var row = rows[offset + doc];
                    require(!seen[doc], "duplicate point");
                    seen[doc] = true;
                    if (geo) {
                        require(NumericUtils.sortableBytesToInt(packed, 0) == row.latEncoded &&
                                NumericUtils.sortableBytesToInt(packed, 4) == row.lonEncoded, "coordinate " + row.id);
                    } else {
                        require(LongPoint.decodeDimension(packed, 0) == row.population, "population " + row.id);
                    }
                }

                public PointValues.Relation compare(byte[] min, byte[] max) {
                    return PointValues.Relation.CELL_CROSSES_QUERY;
                }
            });
        }
        for (int doc = 0; doc < seen.length; doc++) {
            require(seen[doc] == (geo || rows[offset + doc].population >= 0), "point presence");
        }
    }

    private static void audit(Path path, Path input) throws Exception {
        var rows = readRows(Files.readAllBytes(input));
        try (var directory = new MMapDirectory(path); var reader = DirectoryReader.open(directory)) {
            require(reader.maxDoc() == rows.length && reader.numDocs() == rows.length, "document count");
            var segments = new JsonArray();
            for (var ctx : reader.leaves()) {
                var leaf = ctx.reader();
                var stored = leaf.storedFields();
                var population = DocValues.getNumeric(leaf, POP);
                var alias = DocValues.getSortedSet(leaf, ALIAS);
                var expected = new TreeSet<BytesRef>();
                for (int doc = 0; doc < leaf.maxDoc(); doc++) {
                    var row = rows[ctx.docBase + doc];
                    var storedDoc = stored.document(doc);
                    require(storedDoc.getFields().size() == 1 && row.id.equals(storedDoc.get(ID)), "stored ID");
                    require(population.advanceExact(doc) == (row.population >= 0), "population presence");
                    if (row.population >= 0) require(population.longValue() == row.population, "population DV");
                    expected.clear();
                    for (var value : row.aliases) expected.add(value);
                    require(alias.advanceExact(doc) == !expected.isEmpty(), "alias presence");
                    if (!expected.isEmpty()) {
                        require(alias.docValueCount() == expected.size(), "alias count");
                        for (var value : expected) require(alias.lookupOrd(alias.nextOrd()).equals(value), "alias value");
                    }
                }
                auditPoints(leaf, rows, ctx.docBase, true);
                auditPoints(leaf, rows, ctx.docBase, false);
                segments.add(leaf.maxDoc());
            }
            emit(new JsonObject().add("docs", rows.length).add("segments", segments));
        }
    }

    private static Query query(String kind, JsonValue value) {
        return switch (kind) {
            case "id" -> new TermQuery(new Term(ID, value.asString()));
            case "box" -> {
                var a = value.asArray();
                yield LatLonPoint.newBoxQuery(LOCATION, a.get(0).asDouble(), a.get(1).asDouble(),
                    a.get(2).asDouble(), a.get(3).asDouble());
            }
            case "distance" -> {
                var a = value.asArray();
                yield LatLonPoint.newDistanceQuery(LOCATION, a.get(0).asDouble(), a.get(1).asDouble(), a.get(2).asDouble());
            }
            case "pop" -> LongPoint.newRangeQuery(POP, value.asArray().get(0).asLong(), value.asArray().get(1).asLong());
            case "alias" -> SortedSetDocValuesField.newSlowExactQuery(ALIAS, new BytesRef(value.asString()));
            case "all" -> new MatchAllDocsQuery();
            default -> throw new IllegalArgumentException(kind);
        };
    }

    private static void verify(Path path, Path cases) throws Exception {
        try (var directory = new MMapDirectory(path); var reader = DirectoryReader.open(directory)) {
            var searcher = new IndexSearcher(reader);
            searcher.setQueryCache(null);
            var stored = reader.storedFields();
            var population = new SortField(POP, SortField.Type.LONG, true);
            population.setMissingValue(-1L);
            var popSort = new Sort(population);
            var results = new JsonArray();
            for (var value : Json.parse(Files.readString(cases)).asArray()) {
                var row = value.asArray();
                var query = query(row.get(1).asString(), row.get(2));
                int count = searcher.count(query);
                var hits = searcher.search(query, 10, row.get(3).asBoolean() ? popSort : Sort.INDEXORDER);
                var ids = new JsonArray();
                for (var hit : hits.scoreDocs) {
                    ids.add(new JsonArray().add(hit.doc).add(stored.document(hit.doc).get(ID)));
                }
                results.add(new JsonArray().add(row.get(0)).add(count).add(ids));
            }
            var segments = new JsonArray();
            for (var ctx : reader.leaves()) segments.add(ctx.reader().maxDoc());
            emit(new JsonObject().add("docs", reader.maxDoc()).add("live", reader.numDocs())
                .add("segments", segments).add("results", results));
        }
    }

    public static void main(String[] args) throws Exception {
        switch (args[0]) {
            case "reuse" -> writeServer(Path.of(args[1]));
            case "audit" -> audit(Path.of(args[1]), Path.of(args[2]));
            case "query" -> verify(Path.of(args[1]), Path.of(args[2]));
            default -> throw new IllegalArgumentException(args[0]);
        }
    }
}
