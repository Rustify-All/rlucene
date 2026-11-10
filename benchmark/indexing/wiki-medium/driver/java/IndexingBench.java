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
import com.eclipsesource.json.*;
import java.io.*;
import java.lang.management.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.time.*;
import java.time.format.*;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicReference;
import org.apache.lucene.analysis.*;
import org.apache.lucene.analysis.standard.StandardAnalyzer;
import org.apache.lucene.analysis.tokenattributes.*;
import org.apache.lucene.document.*;
import org.apache.lucene.index.*;
import org.apache.lucene.store.*;
import org.apache.lucene.util.*;

/** Public-API Lucene 10.1 adaptation of the pinned luceneutil text workload. */
public class IndexingBench {
  static final DateTimeFormatter DATE = new DateTimeFormatterBuilder().parseCaseInsensitive()
      .appendPattern("dd-MMM-yyyy HH:mm:ss").optionalStart().appendPattern(".SSS")
      .optionalEnd().toFormatter(Locale.US);
  static BufferedReader input(JsonObject c) throws IOException {
    return new BufferedReader(Files.newBufferedReader(Path.of(c.getString("corpus", "")), StandardCharsets.UTF_8), 65536);
  }
  static long[] runtimeStats() {
    long gc = 0, count = 0;
    for (GarbageCollectorMXBean b : ManagementFactory.getGarbageCollectorMXBeans()) {
      gc += b.getCollectionTime(); count += b.getCollectionCount();
    }
    CompilationMXBean jit = ManagementFactory.getCompilationMXBean();
    return new long[]{gc, count, jit != null && jit.isCompilationTimeMonitoringSupported() ? jit.getTotalCompilationTime() : -1};
  }
  static class BenchDoc {
    final Document doc = new Document();
    final Field title = new StringField("title", "", Field.Store.NO);
    final Field titleTokenized = new TextField("titleTokenized", "", Field.Store.YES);
    final Field body = new TextField("body", "", Field.Store.NO);
    final Field label = new StringField("randomLabel", "", Field.Store.NO);
    final Field id = new StringField("id", "", Field.Store.YES);
    final IntPoint idPoint = new IntPoint("id", 0);
    final Field date = new StringField("date", "", Field.Store.YES);
    final IntPoint timeSec = new IntPoint("timesecnum", 0);
    BenchDoc() {
      doc.add(title); doc.add(titleTokenized); doc.add(body); doc.add(label);
      doc.add(id); doc.add(idPoint); doc.add(date); doc.add(timeSec);
    }
    long fill(String line, int number) {
      String[] p = line.split("\t", 5);
      if (p.length < 4) throw new IllegalArgumentException("invalid input at " + number);
      String randomLabel = p[3].strip();
      LocalDateTime dt = LocalDateTime.parse(p[1], DATE);
      date.setStringValue(p[1]);
      body.setStringValue(p[2]); title.setStringValue(p[0]); label.setStringValue(randomLabel);
      titleTokenized.setStringValue(p[0]);
      char[] idChars = new char[6];
      int idNumber = number;
      for(int i=5;i>=0;i--) { idChars[i]="0123456789abcdefghijklmnopqrstuvwxyz".charAt(idNumber%36); idNumber/=36; }
      id.setStringValue(new String(idChars));
      idPoint.setIntValue(number); timeSec.setIntValue(dt.toLocalTime().toSecondOfDay());
      return p[0].length() + p[2].length() + randomLabel.length();
    }
  }
  record Row(int id, String line) {}

  static void index(JsonObject c) throws Exception {
    int n = c.getInt("documents", 0), threads = c.getInt("threads", 1);
    String mode = c.getString("mode", "full");
    AtomicReference<Throwable> error = new AtomicReference<>();
    try (Directory dir = new MMapDirectory(Path.of(c.getString("index", "")))) {
      IndexWriterConfig iwc = new IndexWriterConfig(new StandardAnalyzer(CharArraySet.EMPTY_SET));
      iwc.setOpenMode(IndexWriterConfig.OpenMode.CREATE).setMaxBufferedDocs(c.getInt("max_buffered_docs", -1))
          .setRAMBufferSizeMB(c.getDouble("ram_mb", 2048)).setUseCompoundFile(false);
      TieredMergePolicy policy = new TieredMergePolicy(); policy.setNoCFSRatio(0.0); iwc.setMergePolicy(policy);
      if (c.getString("scheduler", "concurrent").equals("serial")) {
        iwc.setMergeScheduler(new SerialMergeScheduler());
      } else {
        ConcurrentMergeScheduler cms = new ConcurrentMergeScheduler() {
          @Override protected void handleMergeException(Throwable t) { error.compareAndSet(null, t); }
        };
        int m = c.getInt("merge_threads", 1);
        cms.setMaxMergesAndThreads(m + 4, m); cms.enableAutoIOThrottle(); iwc.setMergeScheduler(cms);
      }
      IndexWriter writer = new IndexWriter(dir, iwc);
      BlockingQueue<Row> queue = new ArrayBlockingQueue<>(1024);
      Row end = new Row(-1, "");
      Thread reader = new Thread(() -> {
        try (BufferedReader in = input(c)) {
          for (int i = 0; i < n; i++) {
            String line = in.readLine();
            if (line == null) throw new EOFException("corpus ended at " + i);
            while (!queue.offer(new Row(i, line), 100, TimeUnit.MILLISECONDS)) {
              if (error.get() != null) return;
            }
          }
        } catch (Throwable t) { error.compareAndSet(null, t); }
        finally {
          for (int i = 0; i < threads; i++) {
            try {
              while (!queue.offer(end, 100, TimeUnit.MILLISECONDS)) {
                if (error.get() != null) return;
              }
            } catch (InterruptedException t) { Thread.currentThread().interrupt(); return; }
          }
        }
      }, "line-reader");
      CountDownLatch ready = new CountDownLatch(threads), start = new CountDownLatch(1);
      long[][] totals = new long[threads][3];
      List<Thread> workers = new ArrayList<>();
      for (int i = 0; i < threads; i++) {
        final int slot = i;
        BenchDoc doc = new BenchDoc();
        Thread worker = new Thread(() -> {
          ready.countDown();
          try {
            start.await();
            while (error.get() == null) {
              Row row = queue.poll(100, TimeUnit.MILLISECONDS);
              if (row == null) continue;
              if (row == end) break;
              long units = doc.fill(row.line(), row.id());
              writer.addDocument(doc.doc);
              totals[slot][0]++; totals[slot][2] += units;
            }
          } catch (Throwable t) { error.compareAndSet(null, t); }
        }, "index-" + i);
        workers.add(worker); worker.start();
      }
      reader.start(); ready.await();
      long[] before = runtimeStats();
      long t0 = System.nanoTime(); start.countDown();
      for (Thread worker : workers) worker.join();
      reader.join();
      long t1 = System.nanoTime();
      long[] afterWrite = runtimeStats();
      if (error.get() != null) { writer.rollback(); throw new RuntimeException(error.get()); }
      long commit = 0, close = 0, rollback = 0;
      long tail = System.nanoTime();
      if (mode.equals("full")) {
        long t = System.nanoTime(); writer.commit(); commit = System.nanoTime() - t;
        t = System.nanoTime(); writer.close(); close = System.nanoTime() - t;
      } else {
        long t = System.nanoTime(); writer.rollback(); rollback = System.nanoTime() - t;
      }
      long tf = System.nanoTime();
      long[] after = runtimeStats();
      if (error.get() != null) throw new RuntimeException(error.get());
      long count = 0, bytes = 0, chars = 0;
      for (long[] s : totals) { count += s[0]; bytes += s[1]; chars += s[2]; }
      if (count != n) throw new IllegalStateException("document count mismatch");
      System.out.println(new JsonObject().add("documents", count)
          .add("luceneutil_utf16_units", chars).add("write_ns", t1-t0).add("finish_ns", tf-tail)
          .add("commit_ns", commit).add("close_wait_merges_ns", close).add("rollback_ns", rollback)
          .add("total_ns", tf-t0).add("persisted", mode.equals("full"))
          .add("jit_ms", afterWrite[2]-before[2]).add("gc_ms", afterWrite[0]-before[0])
          .add("gc_count", afterWrite[1]-before[1]).add("total_jit_ms", after[2]-before[2])
          .add("total_gc_ms", after[0]-before[0]).add("total_gc_count", after[1]-before[1]));
    }
  }

  static void analyze(JsonObject c) throws Exception {
    int warmup = c.getInt("warmup", 10000), count = c.getInt("run_count", 100000);
    long hash = 0, tokens = 0, elapsed = 0;
    try (BufferedReader in = input(c); Analyzer analyzer = new StandardAnalyzer(CharArraySet.EMPTY_SET)) {
      for (int i = 0; i < warmup + count; i++) {
        String line = in.readLine();
        if (line == null) throw new EOFException("not enough analyzer input lines");
        long start = System.nanoTime();
        TokenStream ts = analyzer.tokenStream("field", new StringReader(line)); ts.reset();
        CharTermAttribute term = ts.getAttribute(CharTermAttribute.class);
        PositionIncrementAttribute pos = ts.getAttribute(PositionIncrementAttribute.class);
        OffsetAttribute offset = ts.getAttribute(OffsetAttribute.class);
        while (ts.incrementToken()) {
          hash += 31 * ArrayUtil.hashCode(term.buffer(), 0, term.length());
          hash += 31 * pos.getPositionIncrement();
          hash += 31 * offset.startOffset(); hash += 31 * offset.endOffset();
          if (i >= warmup) tokens++;
        }
        ts.end(); ts.close();
        if (i >= warmup) elapsed += System.nanoTime() - start;
      }
    }
    System.out.println(new JsonObject().add("hash", hash).add("tokens", tokens)
        .add("analyze_ns", elapsed).add("warmup", warmup).add("run_count", count));
  }

  public static void main(String[] args) throws Exception {
    JsonObject c = Json.parse(Files.readString(Path.of(args[0]))).asObject();
    switch (c.getString("action", "")) {
      case "index" -> index(c);
      case "analyze" -> analyze(c);
      case "verify" -> VerifyIndex.run(c);
      default -> throw new IllegalArgumentException("unsupported action");
    }
  }
}
