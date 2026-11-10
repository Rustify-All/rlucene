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
import java.security.MessageDigest;
import java.util.HexFormat;
import org.apache.lucene.analysis.standard.StandardAnalyzer;
import org.apache.lucene.analysis.tokenattributes.CharTermAttribute;
import org.apache.lucene.analysis.tokenattributes.OffsetAttribute;
import org.apache.lucene.analysis.tokenattributes.PositionIncrementAttribute;
import org.apache.lucene.codecs.lucene101.Lucene101Codec;
import org.apache.lucene.document.Document;
import org.apache.lucene.document.Field.Store;
import org.apache.lucene.document.StringField;
import org.apache.lucene.document.TextField;
import org.apache.lucene.index.DirectoryReader;
import org.apache.lucene.index.DocValuesType;
import org.apache.lucene.index.IndexOptions;
import org.apache.lucene.index.IndexWriter;
import org.apache.lucene.index.IndexWriterConfig;
import org.apache.lucene.index.NoMergePolicy;
import org.apache.lucene.index.PostingsEnum;
import org.apache.lucene.index.SerialMergeScheduler;
import org.apache.lucene.index.Term;
import org.apache.lucene.search.BooleanClause;
import org.apache.lucene.search.BooleanQuery;
import org.apache.lucene.search.DocIdSetIterator;
import org.apache.lucene.search.IndexSearcher;
import org.apache.lucene.search.MatchNoDocsQuery;
import org.apache.lucene.search.PhraseQuery;
import org.apache.lucene.search.Query;
import org.apache.lucene.search.TermQuery;
import org.apache.lucene.search.TopScoreDocCollectorManager;
import org.apache.lucene.store.MMapDirectory;
import org.apache.lucene.util.BytesRef;

public final class Passage {
    private static final String PID = "pid";
    private static final String TEXT = "passage";

    private static String take(ByteBuffer input, int length) {
        require(length >= 0 && length <= input.remaining(), "truncated input");
        int start = input.position();
        input.position(start + length);
        return new String(input.array(), start, length, StandardCharsets.UTF_8);
    }

    private static JsonObject write(byte[] data, Path path) throws Exception {
        var input = ByteBuffer.wrap(data);
        int count = input.getInt();
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
                var doc = new Document();
                var pid = new StringField(PID, "", Store.YES);
                var text = new TextField(TEXT, "", Store.YES);
                doc.add(pid);
                doc.add(text);

                long start = System.nanoTime();
                for (int i = 0; i < count; ++i) {
                    int pidLength = input.getInt();
                    int textLength = input.getInt();
                    pid.setStringValue(take(input, pidLength));
                    text.setStringValue(take(input, textLength));
                    writer.addDocument(doc);
                }
                long write = System.nanoTime() - start;
                require(!input.hasRemaining(), "trailing input");
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

    private static void emit(JsonValue json) {
        System.out.println(json);
        System.out.flush();
    }

    private static void writeServer(Path path) throws Exception {
        byte[] input = Files.readAllBytes(path);
        emit(new JsonObject().add("ready", true).add("pid", ProcessHandle.current().pid())
            .add("input_bytes", input.length));
        try (var lines = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8))) {
            for (String line; (line = lines.readLine()) != null;) emit(write(input, Path.of(line)));
        }
    }

    private static JsonArray tokens(StandardAnalyzer analyzer, String text) throws Exception {
        var tokens = new JsonArray();
        try (var stream = analyzer.tokenStream(TEXT, text)) {
            var term = stream.addAttribute(CharTermAttribute.class);
            var inc = stream.addAttribute(PositionIncrementAttribute.class);
            var offset = stream.addAttribute(OffsetAttribute.class);
            stream.reset();
            while (stream.incrementToken()) {
                tokens.add(new JsonArray().add(term.toString()).add(inc.getPositionIncrement())
                    .add(offset.startOffset()).add(offset.endOffset()));
            }
            stream.end();
        }
        return tokens;
    }

    private static Query query(StandardAnalyzer analyzer, String kind, JsonValue value) throws Exception {
        return switch (kind) {
            case "pid" -> new TermQuery(new Term(PID, value.asString()));
            case "term" -> new TermQuery(new Term(TEXT, value.asString()));
            case "phrase" -> {
                var builder = new PhraseQuery.Builder();
                for (var term : value.asArray()) builder.add(new Term(TEXT, term.asString()));
                yield builder.build();
            }
            case "dev" -> {
                var words = tokens(analyzer, value.asString());
                var builder = new BooleanQuery.Builder();
                for (var row : words) {
                    builder.add(new TermQuery(new Term(TEXT, row.asArray().get(0).asString())), BooleanClause.Occur.SHOULD);
                }
                yield words.isEmpty() ? new MatchNoDocsQuery("empty analyzed query") : builder.build();
            }
            default -> throw new IllegalArgumentException(kind);
        };
    }

    private static JsonArray segments(DirectoryReader reader) {
        var segments = new JsonArray();
        for (var leaf : reader.leaves()) segments.add(leaf.reader().maxDoc());
        return segments;
    }

    private static void verify(Path path, Path cases) throws Exception {
        try (var directory = new MMapDirectory(path); var reader = DirectoryReader.open(directory);
                var analyzer = new StandardAnalyzer()) {
            var searcher = new IndexSearcher(reader);
            searcher.setQueryCache(null);
            var stored = reader.storedFields();
            var results = new JsonArray();
            for (var input : Json.parse(Files.readString(cases)).asArray()) {
                var row = input.asArray();
                var query = query(analyzer, row.get(1).asString(), row.get(2));
                int count = searcher.count(query);
                var hits = searcher.search(query, new TopScoreDocCollectorManager(10, reader.maxDoc()));
                require(count == hits.totalHits.value(), "inconsistent hit count");
                var found = new JsonArray();
                for (var hit : hits.scoreDocs) {
                    String pid = stored.document(hit.doc).get(PID);
                    require(pid != null, "missing pid");
                    found.add(new JsonArray().add(hit.doc).add(pid).add(Integer.toUnsignedLong(Float.floatToRawIntBits(hit.score))));
                }
                results.add(new JsonArray().add(row.get(0)).add(count).add(found));
            }
            emit(new JsonObject().add("docs", reader.maxDoc()).add("live", reader.numDocs())
                .add("segments", segments(reader)).add("results", results));
        }
    }

    private static final class Digest {
        private final MessageDigest sha;
        private final ByteBuffer buffer = ByteBuffer.allocate(64 * 1024);

        Digest() throws Exception { sha = MessageDigest.getInstance("SHA-256"); }

        void number(long value) {
            if (buffer.remaining() < Long.BYTES) flush();
            buffer.putLong(value);
        }

        void term(BytesRef term) {
            number(term.length);
            flush();
            sha.update(term.bytes, term.offset, term.length);
        }

        private void flush() {
            sha.update(buffer.array(), 0, buffer.position());
            buffer.clear();
        }

        String finish() {
            flush();
            return HexFormat.of().formatHex(sha.digest());
        }
    }

    private static JsonObject postings(org.apache.lucene.index.LeafReader reader, String field) throws Exception {
        var digest = new Digest();
        boolean positions = field.equals(TEXT);
        var terms = reader.terms(field);
        long termCount = 0, docCount = 0, tokenCount = 0;
        if (terms != null) {
            require(terms.hasFreqs() == positions && terms.hasPositions() == positions
                && !terms.hasOffsets() && !terms.hasPayloads(), "unexpected postings options");
            var iterator = terms.iterator();
            PostingsEnum postings = null;
            for (BytesRef term; (term = iterator.next()) != null;) {
                ++termCount;
                digest.term(term);
                digest.number(iterator.docFreq());
                postings = iterator.postings(postings, positions ? PostingsEnum.POSITIONS : PostingsEnum.NONE);
                for (int doc; (doc = postings.nextDoc()) != DocIdSetIterator.NO_MORE_DOCS;) {
                    ++docCount;
                    digest.number(doc);
                    if (positions) {
                        int frequency = postings.freq();
                        tokenCount += frequency;
                        digest.number(frequency);
                        for (int i = 0; i < frequency; ++i) digest.number(postings.nextPosition());
                    }
                }
            }
        }
        return new JsonObject().add("terms", termCount).add("postings", docCount)
            .add("positions", tokenCount).add("sha256", digest.finish());
    }

    private static void audit(Path path, Path inputPath) throws Exception {
        var input = ByteBuffer.wrap(Files.readAllBytes(inputPath));
        int count = input.getInt();
        try (var directory = new MMapDirectory(path); var reader = DirectoryReader.open(directory)) {
            require(reader.maxDoc() == count && reader.numDocs() == count, "index count mismatch");
            var stored = reader.storedFields();
            for (int i = 0; i < count; ++i) {
                int pidLength = input.getInt();
                int textLength = input.getInt();
                String pid = take(input, pidLength);
                String text = take(input, textLength);
                var doc = stored.document(i);
                require(doc.getFields().size() == 2 && pid.equals(doc.get(PID)) && text.equals(doc.get(TEXT)),
                    "stored mismatch at row " + i + " pid " + pid);
            }
            require(!input.hasRemaining(), "trailing input");
            var audited = new JsonArray();
            for (var leaf : reader.leaves()) {
                var segment = leaf.reader();
                var infos = segment.getFieldInfos();
                require(infos.size() == 2, "field count mismatch");
                var pid = infos.fieldInfo(PID);
                var text = infos.fieldInfo(TEXT);
                require(pid.getIndexOptions() == IndexOptions.DOCS && pid.omitsNorms() && !pid.hasNorms(),
                    "pid schema mismatch");
                require(text.getIndexOptions() == IndexOptions.DOCS_AND_FREQS_AND_POSITIONS && text.hasNorms(),
                    "passage schema mismatch");
                require(pid.getDocValuesType() == DocValuesType.NONE && text.getDocValuesType() == DocValuesType.NONE
                    && !pid.hasTermVectors() && !text.hasTermVectors(), "unexpected extra field features");
                var norms = segment.getNormValues(TEXT);
                var digest = new Digest();
                int normDocs = 0;
                for (int i = 0; i < segment.maxDoc(); ++i) {
                    if (norms.advanceExact(i)) {
                        ++normDocs;
                        digest.number(i);
                        digest.number(norms.longValue());
                    }
                }
                audited.add(new JsonObject().add("docs", segment.maxDoc()).add("norm_docs", normDocs)
                    .add("norms_sha256", digest.finish()).add(PID, postings(segment, PID))
                    .add(TEXT, postings(segment, TEXT)));
            }
            emit(new JsonObject().add("stored_documents", count).add("segments", audited));
        }
    }

    public static void main(String[] args) throws Exception {
        switch (args[0]) {
            case "reuse" -> writeServer(Path.of(args[1]));
            case "tokens" -> {
                var out = new JsonArray();
                try (var analyzer = new StandardAnalyzer()) {
                    for (var text : Json.parse(Files.readString(Path.of(args[1]))).asArray()) {
                        out.add(tokens(analyzer, text.asString()));
                    }
                }
                emit(out);
            }
            case "query" -> verify(Path.of(args[1]), Path.of(args[2]));
            case "audit" -> audit(Path.of(args[1]), Path.of(args[2]));
            default -> throw new IllegalArgumentException(args[0]);
        }
    }
}
