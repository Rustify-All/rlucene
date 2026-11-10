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
import java.nio.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.security.*;
import java.util.*;
import org.apache.lucene.document.Document;
import org.apache.lucene.index.*;
import org.apache.lucene.search.DocIdSetIterator;
import org.apache.lucene.store.*;
import org.apache.lucene.util.*;

/** Untimed integrity and logical equality checks, keyed by the stored business ID. */
class VerifyIndex {
  // Order-independent multiset of SHA-256 records. Business IDs and record types
  // are part of each record; four 64-bit sums and a record count are finalized.
  static class Accumulator {
    final MessageDigest record;
    final long[] sums = new long[4];
    long count;
    Accumulator() throws Exception { record=MessageDigest.getInstance("SHA-256"); }
    void text(String s) { bytes(s.getBytes(StandardCharsets.UTF_8)); }
    void bytes(byte[] b) { number(b.length); record.update(b); }
    void number(long n) { record.update(ByteBuffer.allocate(8).putLong(n).array()); }
    void finish() {
      ByteBuffer b=ByteBuffer.wrap(record.digest());
      for(int i=0;i<4;i++) sums[i]+=b.getLong();
      count++;
    }
    String hash() throws Exception {
      MessageDigest d=MessageDigest.getInstance("SHA-256");
      for(long n:sums) d.update(ByteBuffer.allocate(8).putLong(n).array());
      d.update(ByteBuffer.allocate(8).putLong(count).array());
      return HexFormat.of().formatHex(d.digest());
    }
  }
  static void run(JsonObject c) throws Exception {
    int n = c.getInt("documents", 0);
    try (Directory dir = new MMapDirectory(Path.of(c.getString("index", "")))) {
      try (CheckIndex check = new CheckIndex(dir)) {
        check.setInfoStream(System.err);
        if (!check.checkIndex().clean) throw new IllegalStateException("CheckIndex failed");
      }
      try (DirectoryReader reader = DirectoryReader.open(dir); BufferedReader input = IndexingBench.input(c)) {
        if (reader.numDocs() != n || reader.maxDoc() != n || reader.hasDeletions()) throw new IllegalStateException("count/deletions mismatch");
        Set<String> expectedFields=Set.of("body","date","id","randomLabel","title","titleTokenized","timesecnum");
        Set<String> actualFields=new HashSet<>();
        for(FieldInfo field:FieldInfos.getMergedFieldInfos(reader)) {
          actualFields.add(field.name);
          boolean tokenized=field.name.equals("body") || field.name.equals("titleTokenized");
          IndexOptions expectedOptions=tokenized ? IndexOptions.DOCS_AND_FREQS_AND_POSITIONS :
              field.name.equals("timesecnum") ? IndexOptions.NONE : IndexOptions.DOCS;
          boolean point=field.name.equals("id") || field.name.equals("timesecnum");
          if(field.getIndexOptions()!=expectedOptions || field.hasNorms()!=tokenized ||
              field.getDocValuesType()!=DocValuesType.NONE || field.hasTermVectors() || field.getVectorDimension()!=0 ||
              field.getPointDimensionCount()!=(point?1:0) || field.getPointNumBytes()!=(point?4:0))
            throw new IllegalStateException("field schema mismatch: "+field.name);
        }
        if(!expectedFields.equals(actualFields)) throw new IllegalStateException("unexpected field set");
        Accumulator expectedStored=new Accumulator(), actualStored=new Accumulator();
        Accumulator expectedTime=new Accumulator(), actualTime=new Accumulator(), logical=new Accumulator();
        for(int i=0;i<n;i++) {
          String line=input.readLine(); if(line==null) throw new EOFException();
          String[] source=line.split("\\t",5);
          expectedStored.number(i); expectedStored.text(source[0]); expectedStored.text(source[1]); expectedStored.finish();
          expectedTime.number(i);
          expectedTime.number(java.time.LocalDateTime.parse(source[1],IndexingBench.DATE).toLocalTime().toSecondOfDay());
          expectedTime.finish();
        }
        int[] ids=new int[n]; java.util.BitSet seen=new java.util.BitSet(n);
        StoredFields stored = reader.storedFields();
        for (int doc=0;doc<n;doc++) {
          Document d = stored.document(doc);
          int id = Integer.parseInt(d.get("id"), 36);
          if (id<0 || id>=n || seen.get(id)) throw new IllegalStateException("duplicate/missing ID");
          seen.set(id); ids[doc]=id;
          if(d.getFields().size()!=3) throw new IllegalStateException("stored schema differs at "+id);
          actualStored.number(id); actualStored.text(d.get("titleTokenized")); actualStored.text(d.get("date")); actualStored.finish();
          logical.text("stored"); logical.number(id); logical.text(d.get("id")); logical.text(d.get("titleTokenized")); logical.text(d.get("date")); logical.finish();
        }
        if(!expectedStored.hash().equals(actualStored.hash())) throw new IllegalStateException("stored values differ from input");
        String[] fields={"body","date","id","randomLabel","title","titleTokenized"};
        for (String field : fields) {
          Terms terms = MultiTerms.getTerms(reader, field);
          if (terms==null) throw new IllegalStateException("missing terms: "+field);
          TermsEnum termsEnum=terms.iterator(); BytesRef term;
          while ((term=termsEnum.next())!=null) {
            PostingsEnum postings=termsEnum.postings(null, PostingsEnum.ALL);
            int doc;
            while ((doc=postings.nextDoc())!=DocIdSetIterator.NO_MORE_DOCS) {
              Accumulator digest=logical; digest.text("posting"); digest.number(ids[doc]);
              digest.text(field); digest.bytes(Arrays.copyOfRange(term.bytes,term.offset,term.offset+term.length));
              int freq=postings.freq(); digest.number(freq);
              if (terms.hasPositions()) for(int i=0;i<freq;i++) digest.number(postings.nextPosition());
              digest.finish();
            }
          }
          for (LeafReaderContext context : reader.leaves()) {
            NumericDocValues norms=context.reader().getNormValues(field);
            if (norms!=null) for(int doc=0;doc<context.reader().maxDoc();doc++) {
              if(!norms.advanceExact(doc)) throw new IllegalStateException("missing norm");
              logical.text("norm"); logical.text(field); logical.number(ids[context.docBase+doc]); logical.number(norms.longValue()); logical.finish();
            }
          }
        }
        for (String field : new String[]{"id","timesecnum"}) {
          int[] count={0}; java.util.BitSet pointSeen=new java.util.BitSet(n);
          for(LeafReaderContext context:reader.leaves()) {
            PointValues points=context.reader().getPointValues(field);
            if(points==null || points.getNumDimensions()!=1 || points.getBytesPerDimension()!=4) throw new IllegalStateException("point schema mismatch");
            points.intersect(new PointValues.IntersectVisitor() {
              public void visit(int doc) { throw new IllegalStateException("packed value required"); }
              public void visit(int doc, byte[] packed) {
                int id=ids[context.docBase+doc];
                int value=NumericUtils.sortableBytesToInt(packed,0);
                if(pointSeen.get(id) || field.equals("id") && value!=id) throw new IllegalStateException("point differs at "+id);
                if(field.equals("timesecnum")) { actualTime.number(id); actualTime.number(value); actualTime.finish(); }
                logical.text("point"); logical.text(field); logical.number(id); logical.number(value); logical.finish();
                pointSeen.set(id); count[0]++;
              }
              public PointValues.Relation compare(byte[] min,byte[] max) { return PointValues.Relation.CELL_CROSSES_QUERY; }
            });
          }
          if(count[0]!=n) throw new IllegalStateException("point count mismatch");
        }
        if(!expectedTime.hash().equals(actualTime.hash())) throw new IllegalStateException("time points differ from input");
        JsonArray segments=new JsonArray();
        for(LeafReaderContext context:reader.leaves()) segments.add(context.reader().numDocs());
        System.out.println(new JsonObject().add("num_docs",n).add("logical_sha256",logical.hash())
            .add("segment_documents",segments).add("check_index",true));
      }
    }
  }
}
