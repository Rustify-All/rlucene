# MS MARCO Passage Ranking v1

Microsoft supplies `collection.tar.gz` and `queries.tar.gz` for Passage Ranking v1.
The manifest pins both archive hashes, the uncompressed collection hash and every
derived input. Cite the MS MARCO paper when using the corpus.
[Microsoft release](https://github.com/microsoft/msmarco/blob/master/Datasets.md#passage-ranking-dataset) · [Manifest](manifest.json) · [MS MARCO paper](https://arxiv.org/abs/1611.09268)

From the repository root:

```sh
python3 -B benchmark/datasets/prepare.py ms-marco-passage-1m
```

This downloads the official archives into
`benchmark/data/ms-marco-passage-1m/` and verifies them before conversion.
To use exact copies you already have, supply
`--archive /path/to/collection.tar.gz --queries-archive /path/to/queries.tar.gz`.
Use `--destination /path/to/data` to choose another directory and pass
the same path to the indexing runner with `--data`.

The source has 8,841,823 rows. For an N-row sample, select the original
one-based rows `1 + floor(i * 8841823 / N)`, for `i = 0..N-1`, in source order.
The runner uses N = 4,096 for preflight and N = 1,000,000 for the full workload.
Split each TSV row at the first tab, remove the final LF and validate UTF-8.
The passage bytes are preserved. Each binary file begins with a big-endian
u32 record count. A record contains two big-endian u32 byte lengths followed
by the UTF-8 pid and passage. The preparation also creates a 14-row boundary
fixture and a fixed query set that includes 32 evenly spaced rows from
`queries.dev.tsv`.

The two archives and prepared files occupy about 1.5 GB. Preparation
preserves failed intermediate files in the printed staging path. For
the write and verification commands, see the [indexing guide](../../indexing/README.md).
