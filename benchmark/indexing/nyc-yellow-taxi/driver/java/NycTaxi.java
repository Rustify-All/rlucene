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
import org.apache.lucene.analysis.standard.StandardAnalyzer;
import org.apache.lucene.codecs.lucene101.Lucene101Codec;
import org.apache.lucene.document.DoubleDocValuesField;
import org.apache.lucene.document.DoublePoint;
import org.apache.lucene.document.IntPoint;
import org.apache.lucene.document.LongPoint;
import org.apache.lucene.document.NumericDocValuesField;
import org.apache.lucene.document.StoredField;
import org.apache.lucene.index.DirectoryReader;
import org.apache.lucene.index.DocValues;
import org.apache.lucene.index.IndexWriter;
import org.apache.lucene.index.IndexWriterConfig;
import org.apache.lucene.index.IndexableField;
import org.apache.lucene.index.LeafReader;
import org.apache.lucene.index.NoMergePolicy;
import org.apache.lucene.index.PointValues;
import org.apache.lucene.index.SerialMergeScheduler;
import org.apache.lucene.search.BooleanClause;
import org.apache.lucene.search.BooleanQuery;
import org.apache.lucene.search.FieldExistsQuery;
import org.apache.lucene.search.IndexSearcher;
import org.apache.lucene.search.MatchAllDocsQuery;
import org.apache.lucene.search.Query;
import org.apache.lucene.search.Sort;
import org.apache.lucene.search.SortField;
import org.apache.lucene.store.MMapDirectory;

public final class NycTaxi {
    private static final String ID = "source_row";
    private static final String[] FIELDS = {"tpep_pickup_datetime", "tpep_dropoff_datetime",
        "PULocationID", "DOLocationID", "trip_distance", "total_amount", ID};
    private static final int RECORD_BYTES = 49;

    private static boolean present(int mask, int field) {
        return (mask & (1 << field)) != 0;
    }

    private static int count(byte[] input) {
        int count = ByteBuffer.wrap(input).getInt();
        require(count >= 0 && input.length == 4L + (long) count * RECORD_BYTES, "input size");
        return count;
    }

    private static JsonObject write(byte[] input, int count, Path path) throws Exception {
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
                var idStored = new StoredField(ID, 0L);
                var id = new LongPoint(ID, 0);
                var pickup = new LongPoint(FIELDS[0], 0);
                var pickupDv = new NumericDocValuesField(FIELDS[0], 0);
                var dropoff = new LongPoint(FIELDS[1], 0);
                var dropoffDv = new NumericDocValuesField(FIELDS[1], 0);
                var pu = new IntPoint(FIELDS[2], 0);
                var puDv = new NumericDocValuesField(FIELDS[2], 0);
                var dropZone = new IntPoint(FIELDS[3], 0);
                var dropZoneDv = new NumericDocValuesField(FIELDS[3], 0);
                var distance = new DoublePoint(FIELDS[4], 0);
                var distanceDv = new DoubleDocValuesField(FIELDS[4], 0);
                var total = new DoublePoint(FIELDS[5], 0);
                var totalDv = new DoubleDocValuesField(FIELDS[5], 0);
                var slots = List.<IndexableField>of(idStored, id, pickup, pickupDv, dropoff, dropoffDv,
                    pu, puDv, dropZone, dropZoneDv, distance, distanceDv, total, totalDv);
                var layouts = new ArrayList<List<IndexableField>>(64);
                for (int mask = 0; mask < 64; mask++) {
                    var layout = new ArrayList<IndexableField>(14);
                    layout.add(idStored);
                    layout.add(id);
                    for (int field = 0; field < 6; field++) {
                        if (present(mask, field)) {
                            layout.add(slots.get(2 + 2 * field));
                            layout.add(slots.get(3 + 2 * field));
                        }
                    }
                    layouts.add(layout);
                }
                var data = ByteBuffer.wrap(input);
                data.position(4);
                long start = System.nanoTime();
                for (int i = 0; i < count; i++) {
                    long rowId = data.getLong();
                    int mask = Byte.toUnsignedInt(data.get());
                    long a = data.getLong(), b = data.getLong();
                    int c = data.getInt(), d = data.getInt();
                    double e = data.getDouble(), f = data.getDouble();
                    idStored.setLongValue(rowId);
                    id.setLongValue(rowId);
                    if (present(mask, 0)) {
                        pickup.setLongValue(a);
                        pickupDv.setLongValue(a);
                    }
                    if (present(mask, 1)) {
                        dropoff.setLongValue(b);
                        dropoffDv.setLongValue(b);
                    }
                    if (present(mask, 2)) {
                        pu.setIntValue(c);
                        puDv.setLongValue(c);
                    }
                    if (present(mask, 3)) {
                        dropZone.setIntValue(d);
                        dropZoneDv.setLongValue(d);
                    }
                    if (present(mask, 4)) {
                        distance.setDoubleValue(e);
                        distanceDv.setDoubleValue(e);
                    }
                    if (present(mask, 5)) {
                        total.setDoubleValue(f);
                        totalDv.setDoubleValue(f);
                    }
                    writer.addDocument(layouts.get(mask));
                }
                long write = System.nanoTime() - start;
                start = System.nanoTime();
                writer.commit();
                long commit = System.nanoTime() - start;
                start = System.nanoTime();
                writer.close();
                long close = System.nanoTime() - start;
                return new JsonObject().add("docs", count).add("write_ns", write)
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
        int count = count(input);
        emit(new JsonObject().add("ready", true).add("pid", ProcessHandle.current().pid())
            .add("input_bytes", input.length));
        try (var lines = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8))) {
            for (String line; (line = lines.readLine()) != null;) emit(write(input, count, Path.of(line)));
        }
    }

    private record Row(long id, int mask, long[] values) {}

    private static Row[] readRows(byte[] input) {
        var rows = new Row[count(input)];
        var data = ByteBuffer.wrap(input);
        data.position(4);
        for (int i = 0; i < rows.length; i++) {
            long id = data.getLong();
            int mask = Byte.toUnsignedInt(data.get());
            rows[i] = new Row(id, mask, new long[]{data.getLong(), data.getLong(), data.getInt(),
                data.getInt(), data.getLong(), data.getLong()});
        }
        return rows;
    }

    private static long storedId(org.apache.lucene.document.Document doc) {
        require(doc.getFields().size() == 1, "stored field count");
        return doc.getField(ID).numericValue().longValue();
    }

    private static void auditPoints(LeafReader leaf, Row[] rows, int offset, int field) throws Exception {
        var points = leaf.getPointValues(FIELDS[field]);
        var seen = new boolean[leaf.maxDoc()];
        if (points != null) {
            require(points.getNumDimensions() == 1 && points.getNumIndexDimensions() == 1 &&
                points.getBytesPerDimension() == (field == 2 || field == 3 ? 4 : 8), "point geometry");
            points.intersect(new PointValues.IntersectVisitor() {
                public void visit(int doc) {
                    throw new IllegalStateException("expected packed point");
                }

                public void visit(int doc, byte[] packed) {
                    var row = rows[offset + doc];
                    require(!seen[doc], "duplicate point");
                    seen[doc] = true;
                    require(field == 6 || present(row.mask, field), "unexpected point");
                    long actual = switch (field) {
                        case 2, 3 -> IntPoint.decodeDimension(packed, 0);
                        case 4, 5 -> Double.doubleToRawLongBits(DoublePoint.decodeDimension(packed, 0));
                        default -> LongPoint.decodeDimension(packed, 0);
                    };
                    long expected = switch (field) {
                        case 4, 5 -> Double.doubleToLongBits(Double.longBitsToDouble(row.values[field]));
                        case 6 -> row.id;
                        default -> row.values[field];
                    };
                    if (actual != expected) throw new IllegalStateException("point " + field + " source " + row.id);
                }

                public PointValues.Relation compare(byte[] min, byte[] max) {
                    return PointValues.Relation.CELL_CROSSES_QUERY;
                }
            });
        }
        for (int doc = 0; doc < seen.length; doc++) {
            require(seen[doc] == (field == 6 || present(rows[offset + doc].mask, field)), "point presence");
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
                for (int doc = 0; doc < leaf.maxDoc(); doc++) {
                    require(storedId(stored.document(doc)) == rows[ctx.docBase + doc].id, "stored ID");
                }
                for (int field = 0; field < FIELDS.length; field++) {
                    if (field != 6) {
                        var dv = DocValues.getNumeric(leaf, FIELDS[field]);
                        for (int doc = 0; doc < leaf.maxDoc(); doc++) {
                            var row = rows[ctx.docBase + doc];
                            boolean present = present(row.mask, field);
                            require(dv.advanceExact(doc) == present, "DV presence");
                            if (present && dv.longValue() != row.values[field]) {
                                throw new IllegalStateException("DV " + field + " source " + row.id);
                            }
                        }
                    }
                    auditPoints(leaf, rows, ctx.docBase, field);
                }
                segments.add(leaf.maxDoc());
            }
            emit(new JsonObject().add("docs", rows.length).add("segments", segments));
        }
    }

    private static Query range(int field, long lo, long hi) {
        return switch (field) {
            case 2, 3 -> IntPoint.newRangeQuery(FIELDS[field], Math.toIntExact(lo), Math.toIntExact(hi));
            case 4, 5 -> DoublePoint.newRangeQuery(FIELDS[field], Double.longBitsToDouble(lo), Double.longBitsToDouble(hi));
            default -> LongPoint.newRangeQuery(FIELDS[field], lo, hi);
        };
    }

    private static Query query(JsonArray clauses) {
        if (clauses.isEmpty()) return new MatchAllDocsQuery();
        var first = clauses.get(0).asArray();
        if (clauses.size() == 1 && !first.get(1).isNull()) {
            return range(first.get(0).asInt(), first.get(1).asLong(), first.get(2).asLong());
        }
        var builder = new BooleanQuery.Builder();
        boolean hasRange = false;
        for (var value : clauses) {
            var clause = value.asArray();
            int field = clause.get(0).asInt();
            if (clause.get(1).isNull()) {
                require(clause.get(2).isNull(), "incomplete range");
                builder.add(new FieldExistsQuery(FIELDS[field]), BooleanClause.Occur.MUST_NOT);
            } else {
                hasRange = true;
                builder.add(range(field, clause.get(1).asLong(), clause.get(2).asLong()), BooleanClause.Occur.FILTER);
            }
        }
        if (!hasRange) builder.add(new MatchAllDocsQuery(), BooleanClause.Occur.FILTER);
        return builder.build();
    }

    private static Sort sort(JsonArray fields) {
        if (fields.isEmpty()) return Sort.INDEXORDER;
        var sorts = new SortField[fields.size()];
        for (int i = 0; i < sorts.length; i++) {
            var spec = fields.get(i).asArray();
            int field = spec.get(0).asInt();
            boolean reverse = spec.get(1).asBoolean();
            var type = switch (field) {
                case 0, 1 -> SortField.Type.LONG;
                case 2, 3 -> SortField.Type.INT;
                default -> SortField.Type.DOUBLE;
            };
            var sort = new SortField(FIELDS[field], type, reverse);
            switch (type) {
                case LONG -> sort.setMissingValue(Long.MAX_VALUE);
                case INT -> sort.setMissingValue(Integer.MAX_VALUE);
                case DOUBLE -> sort.setMissingValue(0.0);
                default -> throw new IllegalArgumentException(type.toString());
            }
            sorts[i] = sort;
        }
        return new Sort(sorts);
    }

    private static void verify(Path path, Path cases) throws Exception {
        try (var directory = new MMapDirectory(path); var reader = DirectoryReader.open(directory)) {
            var searcher = new IndexSearcher(reader);
            searcher.setQueryCache(null);
            var stored = reader.storedFields();
            var results = new JsonArray();
            for (var value : Json.parse(Files.readString(cases)).asArray()) {
                var row = value.asArray();
                var query = query(row.get(1).asArray());
                int count = searcher.count(query);
                var hits = searcher.search(query, 10, sort(row.get(2).asArray()));
                var ids = new JsonArray();
                for (var hit : hits.scoreDocs) {
                    ids.add(new JsonArray().add(hit.doc).add(storedId(stored.document(hit.doc))));
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
